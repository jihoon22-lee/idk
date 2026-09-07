//! Bounded diagnostics from registered-run logs. Terminal viewport contents and
//! parser findings never substitute for the run's actual exit status.
use crate::run_wire::{LogChunk, LogDescriptor, LogState, RunInfo};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::OnceLock;

const MAX_LINE: usize = 16 * 1024;
const MAX_PROBLEMS: usize = 256;
const MAX_RETAINED_TEXT: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Note,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub line: u32,
    pub column: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    pub id: String,
    pub run_id: String,
    pub project_id: String,
    /// Reported by the tool, not an approved filesystem location.
    pub file: Option<PathBuf>,
    pub range: Option<SourceRange>,
    pub severity: Severity,
    pub message: String,
    pub details: Vec<String>,
    pub log_offset: u64,
    pub log_generation: u64,
    pub source_generation: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemSet {
    pub run_id: String,
    pub project_id: String,
    pub definition_revision: u64,
    pub source_generation: Option<u64>,
    pub source_changed: Option<bool>,
    pub log_generation: u64,
    pub parsed_bytes: u64,
    pub problems: Vec<Problem>,
    pub limited: bool,
    pub partial: bool,
    pub control_sequences_removed: bool,
}
impl ProblemSet {
    pub fn count(&self, severity: Severity) -> usize {
        self.problems
            .iter()
            .filter(|problem| problem.severity == severity)
            .count()
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Escape {
    #[default]
    Plain,
    Start,
    Intermediate,
    Csi,
    Osc,
    OscTerminator,
    String,
    StringTerminator,
}
#[derive(Default)]
pub(crate) struct Controls {
    state: Escape,
    removed: bool,
}
impl Controls {
    pub(crate) fn consume(&mut self, byte: u8) -> Option<u8> {
        use Escape::*;
        match self.state {
            Plain => match byte {
                0x1b => {
                    self.state = Start;
                    self.removed = true;
                    None
                }
                b'\r' => Some(b'\n'),
                b'\n' | b'\t' | 0x20..=0x7e | 0x80..=0xff => Some(byte),
                _ => {
                    self.removed = true;
                    None
                }
            },
            Start => {
                self.state = match byte {
                    b'[' => Csi,
                    b']' => Osc,
                    b'P' | b'X' | b'^' | b'_' => String,
                    0x20..=0x2f => Intermediate,
                    0x1b => Start,
                    _ => Plain,
                };
                None
            }
            Intermediate => {
                if (0x30..=0x7e).contains(&byte) {
                    self.state = Plain;
                }
                None
            }
            Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    self.state = Plain;
                } else if byte == 0x1b {
                    self.state = Start;
                }
                None
            }
            Osc => {
                if byte == 7 {
                    self.state = Plain;
                } else if byte == 0x1b {
                    self.state = OscTerminator;
                }
                None
            }
            OscTerminator => {
                self.state = if byte == b'\\' {
                    Plain
                } else if byte == 0x1b {
                    OscTerminator
                } else {
                    Osc
                };
                None
            }
            String => {
                if byte == 0x1b {
                    self.state = StringTerminator;
                }
                None
            }
            StringTerminator => {
                self.state = if byte == b'\\' {
                    Plain
                } else if byte == 0x1b {
                    StringTerminator
                } else {
                    String
                };
                None
            }
        }
    }
}

/// Safe plain display text, including for raw search results. It never returns
/// an escape character, C1 control or bidi override to the outer terminal.
pub fn display_text(bytes: &[u8], max_bytes: usize) -> String {
    let mut controls = Controls::default();
    let plain = bytes
        .iter()
        .filter_map(|byte| controls.consume(*byte))
        .take(max_bytes.saturating_mul(4))
        .collect::<Vec<_>>();
    safe_utf8(&plain, max_bytes)
}
pub(crate) fn safe_utf8(bytes: &[u8], maximum: usize) -> String {
    let mut text = String::new();
    for character in String::from_utf8_lossy(bytes).chars() {
        if forbidden_character(character) {
            continue;
        }
        if text.len() + character.len_utf8() > maximum {
            break;
        }
        text.push(character);
    }
    text
}

fn forbidden_character(character: char) -> bool {
    (character.is_control() && !matches!(character, '\n' | '\t'))
        || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

pub struct ProblemParser {
    result: ProblemSet,
    line: Vec<u8>,
    line_offset: u64,
    next_offset: u64,
    line_limited: bool,
    line_ambiguous: bool,
    controls: Controls,
    retained: usize,
    last_problem: Option<usize>,
    python_frame: Option<(PathBuf, u32, u64)>,
    uic_error: Option<(u32, Option<u32>, String, u64)>,
}
impl ProblemParser {
    pub fn new(run: &RunInfo) -> Self {
        Self {
            result: ProblemSet {
                run_id: run.run_id.clone(),
                project_id: run.project_id.clone(),
                definition_revision: run.definition_revision,
                source_generation: run.source_start.generation,
                source_changed: run.source_changed,
                log_generation: run.log.generation,
                parsed_bytes: 0,
                problems: Vec::new(),
                limited: false,
                partial: false,
                control_sequences_removed: false,
            },
            line: Vec::new(),
            line_offset: 0,
            next_offset: 0,
            line_limited: false,
            line_ambiguous: false,
            controls: Controls::default(),
            retained: 0,
            last_problem: None,
            python_frame: None,
            uic_error: None,
        }
    }
    pub fn push(&mut self, chunk: &LogChunk) -> Result<()> {
        ensure!(
            chunk.descriptor.run_id == self.result.run_id
                && chunk.descriptor.generation == self.result.log_generation,
            "log belongs to another run or generation; start a new diagnostic parser"
        );
        ensure!(
            chunk.offset == self.next_offset,
            "log has a gap or repeated chunk; diagnostics were not combined"
        );
        ensure!(
            chunk.data_base64.len() <= 128 * 1024,
            "diagnostic input chunk exceeds bound"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&chunk.data_base64)
            .context("invalid raw log encoding")?;
        ensure!(
            chunk.offset.checked_add(bytes.len() as u64) == Some(chunk.next_offset)
                && chunk.next_offset <= chunk.descriptor.bytes,
            "invalid raw log position"
        );
        for (index, byte) in bytes.iter().enumerate() {
            let end = chunk.offset + index as u64 + 1;
            if self.controls.state == Escape::Plain
                && (byte.is_ascii_control())
                && !matches!(*byte, b'\n' | b'\r' | b'\t' | 0x1b)
            {
                self.line_ambiguous = true;
            }
            if let Some(byte) = self.controls.consume(*byte) {
                if byte == b'\n' {
                    self.complete_line();
                    self.line_offset = end;
                } else if self.line.len() < MAX_LINE {
                    self.line.push(byte);
                } else {
                    self.line_limited = true;
                    self.result.limited = true;
                }
            }
        }
        self.next_offset = chunk.next_offset;
        self.result.parsed_bytes = self.next_offset;
        Ok(())
    }
    pub fn finish(mut self, descriptor: &LogDescriptor) -> Result<ProblemSet> {
        ensure!(
            descriptor.run_id == self.result.run_id
                && descriptor.generation == self.result.log_generation,
            "log changed while diagnostics were collected"
        );
        self.complete_line();
        self.result.partial |= self.next_offset < descriptor.bytes
            || self.controls.state != Escape::Plain
            || matches!(
                descriptor.state,
                LogState::Disabled
                    | LogState::Recording
                    | LogState::WriteFailed
                    | LogState::Expired
                    | LogState::Partial
            );
        self.result.limited |= matches!(descriptor.state, LogState::Limited)
            || descriptor.observed_bytes > descriptor.bytes;
        self.result.control_sequences_removed = self.controls.removed;
        Ok(self.result)
    }
    fn complete_line(&mut self) {
        let bytes = std::mem::take(&mut self.line);
        let unicode_controls =
            std::str::from_utf8(&bytes).is_ok_and(|text| text.chars().any(forbidden_character));
        if unicode_controls {
            self.controls.removed = true;
        }
        if self.line_limited
            || self.line_ambiguous
            || unicode_controls
            || std::str::from_utf8(&bytes).is_err()
        {
            // Lossy display text cannot name the original filesystem entry.
            // Preserve the raw log and refuse to manufacture a different path.
            self.result.partial = true;
            self.python_frame = None;
            self.uic_error = None;
            self.last_problem = None;
            self.line_limited = false;
            self.line_ambiguous = false;
            return;
        }
        let line = safe_utf8(&bytes, MAX_LINE);
        if !self.line_limited {
            self.parse_line(&line);
        }
        self.line_limited = false;
    }
    fn parse_line(&mut self, line: &str) {
        let grammar = grammar();
        if let Some(capture) = grammar.compiler.captures(line) {
            if let Some(position) = position(&capture["line"]) {
                let severity = match capture["severity"].to_ascii_lowercase().as_str() {
                    "warning" => Severity::Warning,
                    "note" => Severity::Note,
                    _ => Severity::Error,
                };
                self.add(
                    Some(unquote(&capture["file"]).into()),
                    Some(SourceRange {
                        line: position,
                        column: capture
                            .name("column")
                            .and_then(|value| position_number(value.as_str())),
                    }),
                    severity,
                    &capture["message"],
                    self.line_offset,
                );
                return;
            }
        }
        if let Some(capture) = grammar.cmake.captures(line) {
            if let Some(number) = position(&capture["line"]) {
                let severity = if capture["severity"].starts_with("Warning") {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                self.add(
                    Some(capture["file"].into()),
                    Some(SourceRange {
                        line: number,
                        column: None,
                    }),
                    severity,
                    &format!("CMake {} ({})", &capture["severity"], &capture["command"]),
                    self.line_offset,
                );
                return;
            }
        }
        if let Some(capture) = grammar.make.captures(line) {
            self.add(
                Some(capture["file"].into()),
                position(&capture["line"]).map(|line| SourceRange { line, column: None }),
                Severity::Error,
                &capture["message"],
                self.line_offset,
            );
            return;
        }
        if let Some(capture) = grammar.pytest.captures(line) {
            if let Some(number) = position(&capture["line"]) {
                self.add(
                    Some(capture["file"].into()),
                    Some(SourceRange {
                        line: number,
                        column: None,
                    }),
                    Severity::Error,
                    &capture["message"],
                    self.line_offset,
                );
                return;
            }
        }
        if let Some(capture) = grammar.python_frame.captures(line) {
            self.python_frame = position(&capture["line"])
                .map(|line| (PathBuf::from(&capture["file"]), line, self.line_offset));
            return;
        }
        if line.starts_with("Traceback (most recent call last)") {
            self.python_frame = None;
            self.last_problem = None;
            return;
        }
        if grammar.python_error.is_match(line.trim()) {
            if let Some((file, line_number, offset)) = self.python_frame.take() {
                self.add(
                    Some(file),
                    Some(SourceRange {
                        line: line_number,
                        column: None,
                    }),
                    Severity::Error,
                    line.trim(),
                    offset,
                );
                return;
            }
        }
        if let Some(capture) = grammar.uic_error.captures(line) {
            self.uic_error = position(&capture["line"]).map(|line| {
                (
                    line,
                    capture
                        .name("column")
                        .and_then(|column| position(column.as_str())),
                    capture["message"].into(),
                    self.line_offset,
                )
            });
            return;
        }
        if let Some(capture) = grammar.uic_file.captures(line) {
            if let Some((line, column, message, offset)) = self.uic_error.take() {
                self.add(
                    Some(capture["file"].into()),
                    Some(SourceRange { line, column }),
                    Severity::Error,
                    &message,
                    offset,
                );
                return;
            }
        }
        if line.starts_with("Project ERROR:") || line.starts_with("CMake Error:") {
            self.add(None, None, Severity::Error, line, self.line_offset);
            return;
        }
        if line.starts_with(char::is_whitespace) && !line.trim().is_empty() {
            if let Some(index) = self.last_problem {
                let detail = safe_utf8(line.as_bytes(), 512);
                if self.result.problems[index].details.len() < 4
                    && self.retained + detail.len() <= MAX_RETAINED_TEXT
                {
                    self.retained += detail.len();
                    self.result.problems[index].details.push(detail);
                } else {
                    self.result.limited = true;
                }
            }
        } else if !line.trim().is_empty() {
            self.last_problem = None;
        }
    }
    fn add(
        &mut self,
        file: Option<PathBuf>,
        range: Option<SourceRange>,
        severity: Severity,
        message: &str,
        offset: u64,
    ) {
        let message = safe_utf8(message.as_bytes(), 2048);
        let file = file.filter(|path| path.as_os_str().as_encoded_bytes().len() <= 4096);
        let size = message.len()
            + file
                .as_ref()
                .map_or(0, |path| path.as_os_str().as_encoded_bytes().len());
        if self.result.problems.len() >= MAX_PROBLEMS || self.retained + size > MAX_RETAINED_TEXT {
            self.result.limited = true;
            self.last_problem = None;
            return;
        }
        let mut hash = Sha256::new();
        for field in [
            self.result.run_id.as_bytes(),
            &self.result.log_generation.to_le_bytes(),
            &offset.to_le_bytes(),
            message.as_bytes(),
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field);
        }
        self.result.problems.push(Problem {
            id: format!("{:x}", hash.finalize()),
            run_id: self.result.run_id.clone(),
            project_id: self.result.project_id.clone(),
            file,
            range,
            severity,
            message,
            details: Vec::new(),
            log_offset: offset,
            log_generation: self.result.log_generation,
            source_generation: self.result.source_generation,
        });
        self.retained += size;
        self.last_problem = Some(self.result.problems.len() - 1);
    }
}
fn position(text: &str) -> Option<u32> {
    position_number(text)
}
fn position_number(text: &str) -> Option<u32> {
    text.parse::<u32>()
        .ok()
        .filter(|number| *number > 0 && *number <= 1_000_000_000)
}
fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
}
struct Grammar {
    compiler: Regex,
    cmake: Regex,
    make: Regex,
    pytest: Regex,
    python_frame: Regex,
    python_error: Regex,
    uic_error: Regex,
    uic_file: Regex,
}
fn grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(||Grammar {
        compiler:Regex::new(r"^(?P<file>.+?):(?P<line>[0-9]+)(?::(?P<column>[0-9]+))?:\s*(?P<severity>fatal error|error|warning|note|Error|Warning):\s*(?P<message>.*)$").unwrap(),
        cmake:Regex::new(r"^CMake (?P<severity>Error|Warning(?: \(dev\))?) at (?P<file>.+):(?P<line>[0-9]+) \((?P<command>[^)]+)\):").unwrap(),
        make:Regex::new(r"^(?:g?make)(?:\[[0-9]+\])?: \*\*\* \[(?P<file>.+?):(?P<line>[0-9]+): [^]]+\] (?P<message>.*)$").unwrap(),
        pytest:Regex::new(r"^(?P<file>.+\.py):(?P<line>[0-9]+): (?P<message>(?:[A-Za-z_][A-Za-z0-9_]*\.)?(?:[A-Za-z_][A-Za-z0-9_]*)?(?:Error|Exception)(?::.*)?)$").unwrap(),
        python_frame:Regex::new(r#"^\s*File "(?P<file>.+)", line (?P<line>[0-9]+)(?:, in .*)?$"#).unwrap(),
        python_error:Regex::new(r"^(?:[A-Za-z_][A-Za-z0-9_]*\.)?(?:[A-Za-z_][A-Za-z0-9_]*)?(?:Error|Exception|Interrupt|Exit)(?::.*)?$").unwrap(),
        uic_error:Regex::new(r"^uic: Error in line (?P<line>[0-9]+), column (?P<column>[0-9]+)\s*:\s*(?P<message>.*)$").unwrap(),
        uic_file:Regex::new(r"^File '(?P<file>.+)' is not valid$").unwrap(),
    })
}
