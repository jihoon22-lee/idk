#[path = "common/run_fixture.rs"]
mod fixture;
use base64::Engine;
use idk_workspace::problems::{display_text, ProblemParser, ProblemSet, Severity};
use idk_workspace::run_wire::{LogChunk, LogState};
use std::path::Path;
fn parse(bytes: &[u8], chunk_size: usize) -> ProblemSet {
    let mut run = fixture::run(Path::new("/approved/project"));
    run.log.bytes = bytes.len() as u64;
    run.log.observed_bytes = run.log.bytes;
    let mut parser = ProblemParser::new(&run);
    for (index, bytes) in bytes.chunks(chunk_size).enumerate() {
        let offset = (index * chunk_size) as u64;
        parser
            .push(&LogChunk {
                descriptor: run.log.clone(),
                offset,
                next_offset: offset + bytes.len() as u64,
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                eof: false,
            })
            .unwrap();
    }
    parser.finish(&run.log).unwrap()
}
#[test]
fn real_tool_formats_keep_run_source_and_raw_log_positions_across_single_byte_chunks() {
    let output=b"\x1b[31msrc/widget.cpp:12:7: error: cannot convert value\x1b[0m\n  build_context();\n  ^~~~~~~~~~~~~\ninclude/a.h:8: Warning: Property declaration is incomplete\nCMake Error at CMakeLists.txt:9 (message):\n  configuration failed\nmake[1]: *** [Makefile:44: widget.o] Error 2\nTraceback (most recent call last):\n  File \"tests/check.py\", line 27, in test_failure\n    assert False\nAssertionError: expected output\ntests/test_case.py:19: ValueError: wrong result\nuic: Error in line 4, column 2 : Premature end of document.\nFile 'forms/main.ui' is not valid\n";
    let whole = parse(output, output.len());
    let fragmented = parse(output, 1);
    assert_eq!(whole.problems.len(), 7);
    assert_eq!(whole.count(Severity::Error), 6);
    assert_eq!(whole.count(Severity::Warning), 1);
    assert!(whole.control_sequences_removed);
    assert!(!whole.partial);
    assert!(!whole.limited);
    assert_eq!(
        serde_json::to_value(&whole).unwrap(),
        serde_json::to_value(&fragmented).unwrap()
    );
    let first = &whole.problems[0];
    assert_eq!(first.file.as_deref(), Some(Path::new("src/widget.cpp")));
    assert_eq!(first.range.as_ref().unwrap().line, 12);
    assert_eq!(first.range.as_ref().unwrap().column, Some(7));
    assert_eq!(first.log_offset, 0);
    assert_eq!(first.source_generation, Some(7));
    assert_eq!(first.details.len(), 2);
    let python = &whole.problems[4];
    assert_eq!(python.file.as_deref(), Some(Path::new("tests/check.py")));
    assert!(output[python.log_offset as usize..].starts_with(b"  File \"tests/check.py\""));
    let uic = &whole.problems[6];
    assert_eq!(uic.file.as_deref(), Some(Path::new("forms/main.ui")));
}
#[test]
fn parser_findings_never_turn_failed_exit_or_missing_log_into_success() {
    let set = parse(b"unsupported custom failure\n", 8);
    assert!(set.problems.is_empty());
    let mut run = fixture::run(Path::new("/approved/project"));
    run.log.state = LogState::Disabled;
    assert!(ProblemParser::new(&run).finish(&run.log).unwrap().partial);
    assert_eq!(run.exit_code, Some(2));
    let set = parse(
        b"file.cpp:3:1: error: reported despite shell exit zero\n",
        11,
    );
    assert_eq!(set.count(Severity::Error), 1);
}
#[test]
fn run_generation_gaps_and_replayed_chunks_cannot_mix_diagnostics() {
    let mut run = fixture::run(Path::new("/approved/project"));
    run.log.bytes = 4;
    run.log.observed_bytes = 4;
    let chunk = LogChunk {
        descriptor: run.log.clone(),
        offset: 0,
        next_offset: 4,
        data_base64: base64::engine::general_purpose::STANDARD.encode(b"text"),
        eof: true,
    };
    let mut parser = ProblemParser::new(&run);
    parser.push(&chunk).unwrap();
    assert!(parser.push(&chunk).is_err());
    let mut other = chunk.clone();
    other.descriptor.generation += 1;
    assert!(ProblemParser::new(&run).push(&other).is_err());
    other = chunk.clone();
    other.offset = 1;
    other.next_offset = 5;
    assert!(ProblemParser::new(&run).push(&other).is_err());
    other = chunk;
    other.descriptor.run_id = "unrelated".into();
    assert!(ProblemParser::new(&run).push(&other).is_err());
}
#[test]
fn oversized_lines_problem_flood_and_ansi_payloads_are_bounded_and_visible_as_partial() {
    let mut bytes = vec![b'x'; 40 * 1024];
    bytes.extend_from_slice(b"\n");
    for line in 1..=1000 {
        bytes.extend_from_slice(format!("file.cpp:{line}:2: error: repeated\n").as_bytes());
    }
    let parsed = parse(&bytes, 8192);
    assert!(parsed.limited);
    assert_eq!(parsed.problems.len(), 256);
    let mut bytes = b"\x1b]52;c;".to_vec();
    bytes.extend(vec![b'x'; 128 * 1024]);
    bytes.extend_from_slice(b"\x07real.cpp:1:2: error: visible\n");
    let parsed = parse(&bytes, 8192);
    assert_eq!(parsed.count(Severity::Error), 1);
    assert!(parsed.control_sequences_removed);
    assert!(parse(b"file.cpp:1: error: bad utf8 \xff\n", 5).partial);
    assert!(parse(b"\x1b]unterminated", 2).partial);
    let safe = display_text(
        "\u{1b}]52;c;secret\u{7}safe\u{1b}[31m text\u{9b}\u{202e}\u{1b}[0m".as_bytes(),
        1024,
    );
    assert_eq!(safe, "safe text");
    assert!(!safe.contains(char::is_control));
}

#[test]
fn invalid_filename_bytes_never_become_a_different_lossy_editor_location() {
    let parsed = parse(b"bad\xff.cpp:3:1: error: unrepresentable path\n", 3);
    assert!(parsed.partial);
    assert!(parsed.problems.is_empty());
    let parsed=parse(b"Traceback (most recent call last):\n  File \"valid.py\", line 7\n  File \"bad\xff.py\", line 9\nException: failure\n",5);
    assert!(parsed.partial);
    assert!(parsed.problems.is_empty());
    let parsed=parse(b"Traceback (most recent call last):\n  File \"tests/simple.py\", line 4\nException: ordinary exception\ntests/test_plain.py:9: Exception: test failure\n",2);
    assert_eq!(parsed.problems.len(), 2);
    assert_eq!(
        parsed.problems[0].file.as_deref(),
        Some(Path::new("tests/simple.py"))
    );
}

#[test]
fn stripped_control_characters_never_alias_an_existing_plain_filename() {
    for path in [
        "bad\u{202e}.cpp",
        "bad\u{9b}.cpp",
        "bad\u{8}.cpp",
        "bad\u{7f}.cpp",
    ] {
        let line = format!("{path}:1:2: error: ambiguous location\n");
        let parsed = parse(line.as_bytes(), 1);
        assert!(parsed.problems.is_empty());
        assert!(parsed.partial);
        assert!(parsed.control_sequences_removed);
    }
    let mut line = b"Traceback (most recent call last):\n  File \"valid.py\", line 2\n".to_vec();
    line.extend(vec![b'x'; 20 * 1024]);
    line.extend_from_slice(b"\nException: earlier frame cannot substitute\n");
    assert!(parse(&line, 8192).problems.is_empty());
}
