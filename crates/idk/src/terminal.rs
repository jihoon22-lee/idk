//! A live PTY and VT screen belong to the host, never to an attached UI.
//!
//! Input is queued with a fixed byte limit. One thread drains both directions using
//! nonblocking descriptors; it never holds a screen lock while waiting for I/O.
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, Processor, Rgb};
use alacritty_terminal::Term;
use anyhow::{anyhow, bail, ensure, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};

const MAX_PENDING_INPUT: usize = 1024 * 1024;
const MAX_EVENTS: usize = 1024;
const MAX_PENDING_REPLIES: usize = 64 * 1024;
const MAX_OSC_BYTES: usize = 16 * 1024;
const IO_CHUNK: usize = 8192;
/// Keeps even worst-case RGB/combining-mark screen JSON below a 4 MiB IPC frame.
pub const MAX_TERMINAL_CELLS: usize = 12_000;
const MAX_SCROLLBACK_CELLS: usize = 4_000_000;
const MAX_COMBINING_MARKS: usize = 32;
const LIMIT_SCAN_CELLS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCell {
    pub text: String,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strike: bool,
    pub hidden: bool,
    pub wide: bool,
    pub wide_spacer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCursor {
    pub row: u16,
    pub col: u16,
    pub shape: TerminalCursorShape,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalCursorShape {
    Block,
    Underline,
    Beam,
    HollowBlock,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse_click: bool,
    pub mouse_motion: bool,
    pub mouse_drag: bool,
    pub mouse_sgr: bool,
    pub mouse_utf8: bool,
    pub focus_reporting: bool,
    pub alternate_screen: bool,
    pub newline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub generation: u64,
    pub rows: u16,
    pub cols: u16,
    /// Dense row-major screen cells, including wide-character spacer cells.
    pub cells: Vec<TerminalCell>,
    pub cursor: Option<TerminalCursor>,
    pub display_offset: usize,
    pub modes: TerminalModes,
    pub title: String,
    pub reader_closed: bool,
    pub error: Option<String>,
    /// An excessive control string, query flood, or combining sequence was
    /// limited. This is nonfatal; terminal input and later output remain usable.
    pub output_limited: bool,
}

impl TerminalSnapshot {
    /// Plain visible text for copy/diagnostics, never escape sequence replay.
    pub fn text(&self) -> String {
        self.cells
            .chunks(usize::from(self.cols).max(1))
            .map(|row| {
                row.iter()
                    .filter(|c| !c.wide_spacer)
                    .map(|c| {
                        if c.hidden {
                            " ".to_owned()
                        } else {
                            c.text.clone()
                        }
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalExit {
    pub code: u32,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TerminalMatch {
    /// Terminal grid line: negative values refer to stored scrollback.
    pub line: i32,
    pub column: u16,
}

#[derive(Debug, Clone)]
pub struct TerminalStatus {
    pub generation: u64,
    pub rows: u16,
    pub cols: u16,
    pub reader_closed: bool,
    pub error: Option<String>,
    pub output_limited: bool,
}

#[derive(Clone, Copy)]
struct Size {
    rows: u16,
    cols: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

impl Size {
    fn checked(rows: u16, cols: u16) -> Result<Self> {
        ensure!(
            rows > 0 && cols >= 2,
            "terminal size requires rows > 0 and columns >= 2"
        );
        ensure!(
            usize::from(rows) * usize::from(cols) <= MAX_TERMINAL_CELLS,
            "terminal screen exceeds cell limit"
        );
        Ok(Self { rows, cols })
    }
    fn pty(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[derive(Clone, Default)]
struct Events {
    queue: Arc<Mutex<VecDeque<Event>>>,
    overflow: Arc<AtomicBool>,
}

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        if !matches!(
            event,
            Event::PtyWrite(_)
                | Event::ColorRequest(..)
                | Event::TextAreaSizeRequest(_)
                | Event::Title(_)
                | Event::ResetTitle
        ) {
            return;
        }
        if let Ok(mut queue) = self.queue.lock() {
            if queue.len() < MAX_EVENTS {
                queue.push_back(event);
            } else {
                self.overflow.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum IngressState {
    #[default]
    Ground,
    Escape,
    Osc,
    DiscardOsc,
}

/// vte's std feature has an unbounded OSC Vec, despite its no_std 1 KiB
/// default. Hold only a bounded OSC body before submitting it to the parser.
/// DCS is streamed to vte's no-op hook/put handlers; APC/SOS/PM are discarded by
/// its state machine without a payload allocation. Its sync buffer is 2 MiB.
#[derive(Default)]
struct Ingress {
    state: IngressState,
    osc: Vec<u8>,
    limited: bool,
}

impl Ingress {
    fn filter<'a>(&mut self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if self.state == IngressState::Ground && !bytes.contains(&0x1b) {
            return Cow::Borrowed(bytes);
        }
        let mut filtered = Vec::with_capacity(bytes.len() + self.osc.len());
        for &byte in bytes {
            match self.state {
                IngressState::Ground => {
                    filtered.push(byte);
                    if byte == 0x1b {
                        self.state = IngressState::Escape;
                    }
                }
                IngressState::Escape => {
                    if byte == b']' {
                        // The initial ESC is already in the parser. Withholding
                        // ] keeps it out of OSC state until the body is complete.
                        self.osc.push(byte);
                        self.state = IngressState::Osc;
                    } else {
                        filtered.push(byte);
                        // C0, DEL and non-ASCII bytes leave vte in Escape; CAN,
                        // SUB or an intermediate/final ASCII byte do not.
                        if matches!(byte, 0x18 | 0x1a | 0x20..=0x7e) {
                            self.state = IngressState::Ground;
                        }
                    }
                }
                IngressState::Osc => {
                    if matches!(byte, 0x07 | 0x18 | 0x1a | 0x1b) {
                        self.osc.push(byte);
                        filtered.extend_from_slice(&self.osc);
                        self.osc.clear();
                        self.state = if byte == 0x1b {
                            IngressState::Escape
                        } else {
                            IngressState::Ground
                        };
                    } else if self.osc.len() < MAX_OSC_BYTES {
                        self.osc.push(byte);
                    } else {
                        // Cancel the ESC waiting in the parser. No truncated
                        // OSC prefix is dispatched (not even a clipboard/title
                        // operation); discard through its real terminator.
                        filtered.push(0x18);
                        self.osc.clear();
                        self.state = IngressState::DiscardOsc;
                        self.limited = true;
                    }
                }
                IngressState::DiscardOsc => match byte {
                    0x07 => self.state = IngressState::Ground,
                    0x18 | 0x1a => {
                        filtered.push(byte);
                        self.state = IngressState::Ground;
                    }
                    0x1b => {
                        filtered.push(byte);
                        self.state = IngressState::Escape;
                    }
                    _ => {}
                },
            }
        }
        Cow::Owned(filtered)
    }
}

struct Engine {
    term: Term<Events>,
    parser: Processor,
    ingress: Ingress,
    events: Events,
    writer: Box<dyn Write + Send>,
    pending: VecDeque<Vec<u8>>,
    pending_offset: usize,
    pending_bytes: usize,
    scrollback: usize,
    title: String,
    reader_closed: bool,
    error: Option<String>,
    limit_scan: usize,
    output_limited: bool,
    generation: u64,
    search_cursor: Option<(String, TerminalMatch, u64)>,
}

impl Engine {
    fn new(size: Size, scrollback: usize, writer: Box<dyn Write + Send>) -> Self {
        let events = Events::default();
        Self {
            term: Term::new(
                Config {
                    scrolling_history: scrollback,
                    kitty_keyboard: false,
                    osc52: Osc52::Disabled,
                    ..Config::default()
                },
                &size,
                events.clone(),
            ),
            parser: Processor::new(),
            ingress: Ingress::default(),
            events,
            writer,
            pending: VecDeque::new(),
            pending_offset: 0,
            pending_bytes: 0,
            scrollback,
            title: String::new(),
            reader_closed: false,
            error: None,
            limit_scan: 0,
            output_limited: false,
            generation: 1,
            search_cursor: None,
        }
    }

    fn queue(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.pending_bytes.saturating_add(bytes.len()) <= MAX_PENDING_INPUT,
            "terminal input queue is full; retry after pending input drains"
        );
        if !bytes.is_empty() {
            self.pending.push_back(bytes.to_vec());
            self.pending_bytes += bytes.len();
        }
        Ok(())
    }

    fn process(&mut self, bytes: &[u8]) {
        let was_limited = self.output_limited;
        let filtered = self.ingress.filter(bytes);
        let has_bytes = !filtered.is_empty();
        self.parser.advance(&mut self.term, &filtered);
        self.output_limited |= self.ingress.limited;
        self.limit_cell_storage();
        self.events();
        if has_bytes || was_limited != self.output_limited {
            self.changed();
        }
    }

    fn tick(&mut self) {
        let before = (self.error.clone(), self.output_limited, self.title.clone());
        let mut synchronized_flush = false;
        if self
            .parser
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.parser.stop_sync(&mut self.term);
            synchronized_flush = true;
        }
        self.events();
        self.limit_cell_storage();
        self.flush();
        if synchronized_flush
            || before != (self.error.clone(), self.output_limited, self.title.clone())
        {
            self.changed();
        }
    }

    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    fn finish_output(&mut self) {
        // A crashing/exiting full-screen program may omit synchronized-update
        // end. There will be no future reader tick to expire the timeout, so
        // commit all already-received display bytes before publishing EOF.
        self.parser.stop_sync(&mut self.term);
        if !self.ingress.osc.is_empty() {
            self.output_limited = true;
            self.ingress.osc.clear();
        }
        self.events();
        self.limit_cell_storage();
        self.reader_closed = true;
        self.changed();
    }

    fn limit_cell_storage(&mut self) {
        // Alacritty bounds rows but permits arbitrarily many combining marks in
        // one cell. Sweep incrementally, including history, without scanning an
        // entire large history under the screen lock on every output packet.
        // Only 8 KiB of output can enter between sweeps. Snapshot serialization
        // also clips immediately, so an unswept cell cannot inflate IPC frames.
        let cols = self.term.columns();
        let history = self.term.history_size();
        let total = self.term.total_lines() * cols;
        for _ in 0..LIMIT_SCAN_CELLS.min(total) {
            self.limit_scan %= total;
            let point = Point::new(
                Line((self.limit_scan / cols) as i32 - history as i32),
                Column(self.limit_scan % cols),
            );
            self.limit_scan += 1;
            let cell = &mut self.term.grid_mut()[point];
            if cell
                .zerowidth()
                .is_some_and(|marks| marks.len() > MAX_COMBINING_MARKS)
            {
                let marks: Vec<_> = cell
                    .zerowidth()
                    .unwrap()
                    .iter()
                    .copied()
                    .take(MAX_COMBINING_MARKS)
                    .collect();
                let underline = cell.underline_color();
                let hyperlink = cell.hyperlink();
                cell.extra = None;
                cell.set_underline_color(underline);
                cell.set_hyperlink(hyperlink);
                for mark in marks {
                    cell.push_zerowidth(mark);
                }
                self.output_limited = true;
            }
        }
    }

    fn events(&mut self) {
        let events = match self.events.queue.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(_) => {
                self.error = Some("terminal event queue lock failed".into());
                return;
            }
        };
        if self.events.overflow.swap(false, Ordering::Relaxed) {
            self.output_limited = true;
        }
        for event in events {
            let reply = match event {
                Event::PtyWrite(text) => Some(text),
                Event::ColorRequest(index, formatter) => Some(formatter(
                    self.term.colors()[index].unwrap_or_else(|| palette(index)),
                )),
                Event::TextAreaSizeRequest(formatter) => Some(formatter(WindowSize {
                    num_lines: self.term.screen_lines() as u16,
                    num_cols: self.term.columns() as u16,
                    cell_width: 0,
                    cell_height: 0,
                })),
                Event::Title(title) => {
                    self.title = title
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(256)
                        .collect();
                    None
                }
                Event::ResetTitle => {
                    self.title.clear();
                    None
                }
                _ => None,
            };
            if let Some(reply) = reply {
                // A child must not fill the entire user-input queue with its
                // own replies and thereby prevent Ctrl-C or other user input.
                if self.pending_bytes.saturating_add(reply.len()) > MAX_PENDING_REPLIES
                    || self.queue(reply.as_bytes()).is_err()
                {
                    self.output_limited = true;
                }
            }
        }
    }

    fn flush(&mut self) {
        // One bounded write per iteration, even while a child floods output.
        let Some(front) = self.pending.front() else {
            return;
        };
        let end = front.len().min(self.pending_offset + IO_CHUNK);
        match self.writer.write(&front[self.pending_offset..end]) {
            Ok(0) => self.error = Some("PTY input closed before queued input drained".into()),
            Ok(n) => {
                self.pending_offset += n;
                self.pending_bytes -= n;
                if self.pending_offset == front.len() {
                    self.pending.pop_front();
                    self.pending_offset = 0;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                self.error = Some(format!("PTY write failed: {error}"));
                self.pending.clear();
                self.pending_bytes = 0;
                self.pending_offset = 0;
            }
        }
    }

    fn snapshot(&self) -> TerminalSnapshot {
        let content = self.term.renderable_content();
        let mode = content.mode;
        let offset = content.display_offset;
        let point = content.cursor.point;
        let row = i64::from(point.line.0) + offset as i64;
        let cursor_style = self.term.cursor_style();
        let cursor = if mode.contains(TermMode::SHOW_CURSOR)
            && cursor_style.shape != CursorShape::Hidden
            && row >= 0
            && row < self.term.screen_lines() as i64
        {
            Some(TerminalCursor {
                row: row as u16,
                col: point.column.0 as u16,
                shape: match cursor_style.shape {
                    CursorShape::Underline => TerminalCursorShape::Underline,
                    CursorShape::Beam => TerminalCursorShape::Beam,
                    CursorShape::HollowBlock => TerminalCursorShape::HollowBlock,
                    _ => TerminalCursorShape::Block,
                },
                blinking: cursor_style.blinking,
            })
        } else {
            None
        };
        let mut output_limited = self.output_limited;
        let cells = content
            .display_iter
            .map(|indexed| {
                let cell = indexed.cell;
                let flags = cell.flags;
                let mut text = cell.c.to_string();
                if let Some(extra) = cell.zerowidth() {
                    output_limited |= extra.len() > MAX_COMBINING_MARKS;
                    text.extend(extra.iter().take(MAX_COMBINING_MARKS));
                }
                TerminalCell {
                    text,
                    fg: convert_color(cell.fg, &self.term),
                    bg: convert_color(cell.bg, &self.term),
                    bold: flags.contains(Flags::BOLD),
                    dim: flags.contains(Flags::DIM),
                    italic: flags.contains(Flags::ITALIC),
                    underline: flags.intersects(Flags::ALL_UNDERLINES),
                    inverse: flags.contains(Flags::INVERSE),
                    strike: flags.contains(Flags::STRIKEOUT),
                    hidden: flags.contains(Flags::HIDDEN),
                    wide: flags.contains(Flags::WIDE_CHAR),
                    wide_spacer: flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
                }
            })
            .collect();
        TerminalSnapshot {
            generation: self.generation,
            rows: self.term.screen_lines() as u16,
            cols: self.term.columns() as u16,
            cells,
            cursor,
            display_offset: offset,
            modes: TerminalModes {
                application_cursor: mode.contains(TermMode::APP_CURSOR),
                application_keypad: mode.contains(TermMode::APP_KEYPAD),
                bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
                mouse_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
                mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
                mouse_drag: mode.contains(TermMode::MOUSE_DRAG),
                mouse_sgr: mode.contains(TermMode::SGR_MOUSE),
                mouse_utf8: mode.contains(TermMode::UTF8_MOUSE),
                focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
                alternate_screen: mode.contains(TermMode::ALT_SCREEN),
                newline: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            },
            title: self.title.clone(),
            reader_closed: self.reader_closed,
            error: self.error.clone(),
            output_limited,
        }
    }
}

fn convert_color(color: Color, term: &Term<Events>) -> TerminalColor {
    let index = match color {
        Color::Spec(rgb) => return TerminalColor::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => usize::from(index),
        Color::Named(named) => named as usize,
    };
    if let Some(rgb) = term.colors()[index] {
        return TerminalColor::Rgb(rgb.r, rgb.g, rgb.b);
    }
    match index {
        0..=255 => TerminalColor::Indexed(index as u8),
        259..=266 => {
            let rgb = palette(index);
            TerminalColor::Rgb(rgb.r, rgb.g, rgb.b)
        }
        _ => TerminalColor::Default,
    }
}

fn palette(index: usize) -> Rgb {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let (r, g, b) = match index {
        0..=15 => BASE[index],
        16..=231 => {
            let n = index - 16;
            let level = |n: usize| if n == 0 { 0 } else { (55 + n * 40) as u8 };
            (level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        232..=255 => {
            let v = (8 + (index - 232) * 10) as u8;
            (v, v, v)
        }
        257 => (0, 0, 0),
        259..=266 => {
            let (r, g, b) = BASE[index - 259];
            (r / 2, g / 2, b / 2)
        }
        _ => (229, 229, 229),
    };
    Rgb { r, g, b }
}

type OwnedChild = Box<dyn Child + Send + Sync>;

/// Dropping this object closes its owned terminal. UI detach must retain it.
/// Drop reaps the owned child without treating hangup as confirmed cancellation
/// and without waiting for a long-running child while holding a host lock.
pub struct TerminalSession {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<OwnedChild>,
    pid: Option<u32>,
    engine: Arc<Mutex<Engine>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    reaper: Option<mpsc::SyncSender<OwnedChild>>,
    exit: Option<TerminalExit>,
    defer_reap: bool,
    reaped: bool,
}

impl TerminalSession {
    pub fn spawn(command: CommandBuilder, rows: u16, cols: u16, scrollback: usize) -> Result<Self> {
        let size = Size::checked(rows, cols)?;
        ensure!(
            scrollback.saturating_mul(usize::from(cols)) <= MAX_SCROLLBACK_CELLS,
            "terminal scrollback exceeds cell limit"
        );
        let pair = native_pty_system()
            .openpty(size.pty())
            .context("open terminal PTY")?;
        let fd = pair
            .master
            .as_raw_fd()
            .context("PTY has no Unix descriptor")?;
        // SAFETY: fd is a live PTY descriptor retained by pair.master. F_GETFL and
        // F_SETFL do not transfer ownership. All duplicated endpoints share flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error()).context("make PTY nonblocking");
        }
        // A separate owned descriptor keeps poll valid even during host cleanup.
        let poll_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if poll_fd < 0 {
            return Err(std::io::Error::last_os_error()).context("clone PTY poll descriptor");
        }
        // SAFETY: F_DUPFD_CLOEXEC returned a new descriptor owned solely here.
        let poll_file = unsafe { File::from_raw_fd(poll_fd) };
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .context("spawn terminal shell")?;
        let pid = child.process_id();
        drop(pair.slave);
        let engine = Arc::new(Mutex::new(Engine::new(size, scrollback, writer)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_engine = engine.clone();
        let thread_stop = stop.clone();
        let (reaper, reap_child) = mpsc::sync_channel::<OwnedChild>(1);
        let reader_thread = match thread::Builder::new()
            .name("idk-pty".into())
            .spawn(move || {
                reader_loop(reader, poll_file, thread_engine, thread_stop);
                // reader_loop owns and drops every PTY descriptor/engine ref
                // before this wait. No extra thread is needed on normal Drop.
                if let Ok(mut child) = reap_child.recv() {
                    let _ = child.wait();
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("start terminal reader");
            }
        };
        Ok(Self {
            master: Some(pair.master),
            child: Some(child),
            pid,
            engine,
            stop,
            reader: Some(reader_thread),
            reaper: Some(reaper),
            exit: None,
            defer_reap: false,
            reaped: false,
        })
    }

    fn engine(&self) -> Result<MutexGuard<'_, Engine>> {
        self.engine
            .lock()
            .map_err(|_| anyhow!("terminal screen lock failed"))
    }

    pub fn snapshot(&self) -> Result<TerminalSnapshot> {
        Ok(self.engine()?.snapshot())
    }

    pub fn snapshot_since(&self, generation: Option<u64>) -> Result<Option<TerminalSnapshot>> {
        let engine = self.engine()?;
        if generation == Some(engine.generation) {
            Ok(None)
        } else {
            Ok(Some(engine.snapshot()))
        }
    }

    pub fn status(&self) -> Result<TerminalStatus> {
        let engine = self.engine()?;
        Ok(TerminalStatus {
            generation: engine.generation,
            rows: engine.term.screen_lines() as u16,
            cols: engine.term.columns() as u16,
            reader_closed: engine.reader_closed,
            error: engine.error.clone(),
            output_limited: engine.output_limited,
        })
    }

    /// Accept bytes into the bounded queue. Later I/O errors appear in snapshot.error.
    pub fn input(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(self.try_wait()?.is_none(), "terminal shell has exited");
        let mut engine = self.engine()?;
        ensure!(!engine.reader_closed, "terminal PTY is closed");
        if let Some(error) = &engine.error {
            bail!("terminal I/O failed: {error}");
        }
        engine.queue(bytes)?;
        let offset = engine.term.grid().display_offset();
        engine.term.scroll_display(Scroll::Bottom);
        if offset != 0 {
            engine.changed();
        }
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let size = Size::checked(rows, cols)?;
        let mut engine = self.engine()?;
        if engine.term.screen_lines() == usize::from(rows)
            && engine.term.columns() == usize::from(cols)
        {
            return Ok(());
        }
        ensure!(
            engine.scrollback.saturating_mul(usize::from(cols)) <= MAX_SCROLLBACK_CELLS,
            "resized scrollback exceeds cell limit"
        );
        self.master
            .as_ref()
            .context("terminal output collection has ended")?
            .resize(size.pty())
            .context("resize PTY")?;
        engine.term.resize(size);
        engine.changed();
        Ok(())
    }

    pub fn scroll(&mut self, delta: i32) -> Result<()> {
        // The upstream grid adds this delta using i32 arithmetic. IPC callers
        // may legitimately use extreme values for top/bottom; clamp first.
        let bound = MAX_SCROLLBACK_CELLS as i32;
        let mut engine = self.engine()?;
        let offset = engine.term.grid().display_offset();
        engine
            .term
            .scroll_display(Scroll::Delta(delta.clamp(-bound, bound)));
        if engine.term.grid().display_offset() != offset {
            engine.changed();
        }
        Ok(())
    }

    /// Literal search over all retained display rows, wrapping at history ends.
    /// A query does not span a display-row boundary. Repeat searches advance;
    /// new output restarts at the current viewport rather than using stale rows.
    pub fn search(
        &mut self,
        query: &str,
        backwards: bool,
    ) -> Result<(Option<TerminalMatch>, usize)> {
        ensure!(
            !query.is_empty() && query.len() <= 256 && !query.chars().any(char::is_control),
            "search needs 1–256 bytes of visible text"
        );
        let mut engine = self.engine()?;
        let history = engine.term.history_size();
        let rows = engine.term.total_lines();
        let cols = engine.term.columns();
        let old = engine
            .search_cursor
            .as_ref()
            .filter(|(text, _, generation)| text == query && *generation == engine.generation)
            .map(|(_, point, _)| *point);
        let anchor = old.unwrap_or(TerminalMatch {
            line: if backwards {
                engine.term.screen_lines() as i32 - 1 - engine.term.grid().display_offset() as i32
            } else {
                -(engine.term.grid().display_offset() as i32)
            },
            column: if backwards { cols as u16 - 1 } else { 0 },
        });
        let mut first = None;
        let mut last = None;
        let mut next = None;
        let mut previous = None;
        for row in -(history as i32)..engine.term.screen_lines() as i32 {
            let mut text = String::new();
            let mut columns = Vec::with_capacity(cols);
            for column in 0..cols {
                let cell = &engine.term.grid()[Point::new(Line(row), Column(column))];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                columns.push((text.len(), column as u16));
                if cell.flags.contains(Flags::HIDDEN) {
                    text.push(' ');
                } else {
                    text.push(cell.c);
                    if let Some(extra) = cell.zerowidth() {
                        text.extend(extra.iter().take(MAX_COMBINING_MARKS));
                    }
                }
            }
            for (offset, _) in text.match_indices(query) {
                let index = columns
                    .partition_point(|(byte, _)| *byte <= offset)
                    .saturating_sub(1);
                let Some((_, column)) = columns.get(index) else {
                    continue;
                };
                let point = TerminalMatch {
                    line: row,
                    column: *column,
                };
                if first.is_none() {
                    first = Some(point);
                }
                last = Some(point);
                if next.is_none() && (point > anchor || (old.is_none() && point == anchor)) {
                    next = Some(point);
                }
                if point < anchor || (old.is_none() && point == anchor) {
                    previous = Some(point);
                }
            }
        }
        let found = if backwards {
            previous.or(last)
        } else {
            next.or(first)
        };
        if let Some(point) = found {
            engine.term.scroll_to_point(Point::new(
                Line(point.line),
                Column(usize::from(point.column)),
            ));
            engine.changed();
            engine.search_cursor = Some((query.into(), point, engine.generation));
        }
        Ok((found, rows))
    }

    pub fn try_wait(&mut self) -> Result<Option<TerminalExit>> {
        if self.defer_reap {
            return self.observe_exit();
        }
        if !self.reaped {
            if let Some(status) = self
                .child
                .as_mut()
                .context("terminal child handle unavailable")?
                .try_wait()
                .context("observe terminal child")?
            {
                self.exit = Some(TerminalExit {
                    code: status.exit_code(),
                    signal: status.signal().map(str::to_owned),
                });
                self.reaped = true;
            }
        }
        Ok(self.exit.clone())
    }

    /// Host-only lifetime mode: retain the leader as an unreaped SID anchor
    /// until its adopted same-session descendants have been accounted for.
    pub(crate) fn defer_reaping(&mut self) {
        self.defer_reap = true;
    }

    fn observe_exit(&mut self) -> Result<Option<TerminalExit>> {
        if self.exit.is_none() {
            let pid = self.pid.context("terminal child PID unavailable")?;
            let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut status,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("observe owned terminal without releasing its SID");
            }
            if unsafe { status.si_pid() } != 0 {
                use std::os::unix::process::ExitStatusExt;
                let code = unsafe { status.si_status() };
                let raw = match status.si_code {
                    libc::CLD_EXITED => code << 8,
                    libc::CLD_KILLED => code,
                    libc::CLD_DUMPED => code | 0x80,
                    _ => bail!("unexpected terminal wait status"),
                };
                let status =
                    portable_pty::ExitStatus::from(std::process::ExitStatus::from_raw(raw));
                self.exit = Some(TerminalExit {
                    code: status.exit_code(),
                    signal: status.signal().map(str::to_owned),
                });
            }
        }
        Ok(self.exit.clone())
    }

    pub(crate) fn reap_exit(&mut self) -> Result<TerminalExit> {
        ensure!(
            self.observe_exit()?.is_some(),
            "terminal leader has not exited"
        );
        self.defer_reap = false;
        self.try_wait()?
            .context("terminal leader exit was not reaped")
    }

    /// Signal the still-owned leader alone. The host handles adopted background
    /// children individually after exit; no process-group PID lookup grants it
    /// authority to signal a potentially reused process group.
    pub(crate) fn request_host_close(&mut self, force: bool) -> Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        let pid = self.pid.context("terminal child PID unavailable")? as libc::pid_t;
        ensure!(
            unsafe { libc::getsid(pid) } == pid,
            "terminal process session ownership changed"
        );
        signal(pid, if force { libc::SIGKILL } else { libc::SIGHUP })
    }

    /// Close bounded PTY collection while retaining an unreaped child handle.
    /// This permits honest descendant cleanup after the final-output deadline.
    pub(crate) fn end_collection(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        {
            let mut engine = self.engine()?;
            engine.output_limited = true;
            engine.writer = Box::new(std::io::sink());
            engine.pending.clear();
            engine.pending_bytes = 0;
            engine.pending_offset = 0;
            engine.finish_output();
        }
        self.master.take();
        Ok(())
    }

    /// Request hangup of the owned shell and its current foreground job. This is
    /// a signal request, not proof of exit: callers must continue try_wait().
    pub fn terminate(&mut self) -> Result<()> {
        self.signal_owned(libc::SIGHUP)
    }

    /// Force the still-owned shell and current owned foreground process group.
    /// Detached/nohup descendants outside that group are not claimed as reaped.
    pub fn force_terminate(&mut self) -> Result<()> {
        self.signal_owned(libc::SIGKILL)
    }

    fn signal_owned(&mut self, requested: libc::c_int) -> Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        let pid = self.pid.context("terminal child PID unavailable")? as libc::pid_t;
        // A live, unreaped child PID cannot be reused. PTY spawning made it a
        // session leader; reject any different session before signalling a group.
        if unsafe { libc::getsid(pid) } != pid {
            bail!("terminal process session ownership changed");
        }
        if let Some(group) = self
            .master
            .as_ref()
            .and_then(|master| master.process_group_leader())
        {
            if group > 1 && unsafe { libc::getsid(group) } == pid {
                signal(-group, requested)?;
            }
        }
        signal(pid, requested)
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.pid
    }

    /// Observed shell cwd only; an inaccessible/exited process is unknown.
    pub fn cwd(&self) -> Option<PathBuf> {
        if self.exit.is_some() {
            return None;
        }
        let pid = self.pid?;
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
}

fn signal(pid: libc::pid_t, signal: libc::c_int) -> Result<()> {
    // SAFETY: caller supplies a verified owned PID/group; no pointers involved.
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.into())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(mut child) = self.child.take() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let unqueued = match self.reaper.take() {
                    Some(reaper) => reaper.send(child).err().map(|error| error.0),
                    None => Some(child),
                };
                // A disconnected worker means it failed before cleanup. This
                // fallback carries only the child handle, never PTY endpoints.
                if let Some(mut child) = unqueued {
                    let _ = thread::Builder::new()
                        .name("idk-child-reaper".into())
                        .spawn(move || {
                            let _ = child.wait();
                        });
                }
            }
        }
        // Wake an idle worker when try_wait already reaped the child, and detach
        // instead of joining a wait that may outlive PTY closure (e.g. nohup).
        drop(self.reaper.take());
        drop(self.reader.take());
    }
}

fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    poll_file: File,
    engine: Arc<Mutex<Engine>>,
    stop: Arc<AtomicBool>,
) {
    let mut bytes = [0u8; IO_CHUNK];
    while !stop.load(Ordering::Acquire) {
        let pending = match engine.lock() {
            Ok(mut state) => {
                state.tick();
                state.pending_bytes > 0
            }
            Err(_) => break,
        };
        let mut poll = libc::pollfd {
            fd: poll_file.as_raw_fd(),
            events: libc::POLLIN | if pending { libc::POLLOUT } else { 0 },
            revents: 0,
        };
        // SAFETY: one initialized pollfd, descriptor retained by poll_file.
        let result = unsafe { libc::poll(&mut poll, 1, 25) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if let Ok(mut state) = engine.lock() {
                state.error = Some(format!("PTY poll failed: {error}"));
                state.finish_output();
            }
            break;
        }
        if poll.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
            continue;
        }
        match reader.read(&mut bytes) {
            Ok(0) => {
                if let Ok(mut state) = engine.lock() {
                    state.finish_output();
                }
                break;
            }
            Ok(count) => {
                if let Ok(mut state) = engine.lock() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    state.process(&bytes[..count]);
                } else {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                if let Ok(mut state) = engine.lock() {
                    // Linux PTY masters report EIO after the final slave closes.
                    if error.raw_os_error() != Some(libc::EIO) {
                        state.error = Some(format!("PTY read failed: {error}"));
                    }
                    state.finish_output();
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(rows: u16, cols: u16) -> Engine {
        Engine::new(Size { rows, cols }, 100, Box::new(std::io::sink()))
    }

    #[test]
    fn dec_charset_and_split_utf8_are_real_cells() {
        let mut engine = engine(4, 20);
        engine.process(b"\x1b(0lqqk\x1b(B\r\n");
        for byte in "한글e\u{301}".as_bytes() {
            engine.process(&[*byte]);
        }
        let screen = engine.snapshot();
        assert!(
            screen.text().starts_with("┌──┐\n한글e\u{301}"),
            "{}",
            screen.text()
        );
        assert!(screen.cells[20].wide);
        assert!(screen.cells[21].wide_spacer);
        assert_eq!(screen.cells[24].text, "e\u{301}");
    }

    #[test]
    fn alternate_screen_resize_and_modes_restore() {
        let mut engine = engine(4, 20);
        engine.process(b"main\x1b[?1049h\x1b[?1h\x1b[?2004hfull");
        assert!(engine.snapshot().modes.alternate_screen);
        engine.term.resize(Size { rows: 6, cols: 30 });
        assert_eq!(engine.snapshot().cells.len(), 180);
        engine.process(b"\x1b[?1049l");
        assert!(engine.snapshot().text().starts_with("main"));
        assert!(engine.snapshot().modes.application_cursor);
        assert!(engine.snapshot().modes.bracketed_paste);
    }

    #[test]
    fn queries_reply_and_clipboard_requests_do_not_escape() {
        let mut engine = engine(24, 80);
        engine.process(
            b"\x1b[4;9H\x1b[6n\x1b[c\x1b[18t\x1b]10;?\x07\x1b]52;c;YWJj\x07\x1b]52;c;?\x07",
        );
        let replies = engine.pending.iter().flatten().copied().collect::<Vec<_>>();
        let replies = String::from_utf8(replies).unwrap();
        assert!(replies.contains("\x1b[4;9R"), "{replies:?}");
        assert!(replies.contains("\x1b[?6c"));
        assert!(replies.contains("\x1b[8;24;80t"));
        assert!(replies.contains("rgb:"));
        assert!(!replies.contains("52;"));
    }

    #[test]
    fn split_osc_is_preserved_and_oversize_osc_is_discarded_through_terminator() {
        let mut engine = engine(4, 40);
        // A C0 byte inside Escape still permits OSC, matching vte's state machine.
        for byte in "\x1b\0]2;한글 title\x07".as_bytes() {
            engine.process(&[*byte]);
        }
        assert_eq!(engine.snapshot().title, "한글 title");
        engine.process(b"\x1b]2;");
        for _ in 0..128 {
            engine.process(&[b'x'; IO_CHUNK]);
            assert!(engine.ingress.osc.len() <= MAX_OSC_BYTES);
        }
        assert!(engine.snapshot().output_limited);
        assert!(engine.snapshot().error.is_none());
        assert_eq!(engine.snapshot().title, "한글 title");
        // Neither the clipped title nor its discarded tail becomes screen text.
        engine.process(b"discarded tail\x1b");
        engine.process(b"\\after\x1b]2;recovered");
        engine.process(b"\x07\x1b[6n");
        assert_eq!(engine.snapshot().title, "recovered");
        assert!(engine.snapshot().text().starts_with("after"));
        assert!(
            !engine.pending.is_empty(),
            "queries must still receive replies"
        );
    }

    #[test]
    fn osc_budget_preserves_synchronized_output_and_cancel_recovery() {
        let mut engine = engine(4, 40);
        engine.process(b"\x1b[?2026hbefore\x1b]2;");
        for _ in 0..4 {
            engine.process(&[b'x'; IO_CHUNK]);
        }
        engine.process(b"\x18after\x1b[?2026l");
        assert!(engine.snapshot().text().starts_with("beforeafter"));
        assert!(engine.snapshot().output_limited);
    }

    #[test]
    fn unsupported_control_string_payloads_are_not_accumulated_or_replayed() {
        for prefix in [b"\x1bPq".as_slice(), b"\x1b_", b"\x1bX", b"\x1b^"] {
            let mut engine = engine(4, 40);
            engine.process(prefix);
            for _ in 0..32 {
                engine.process(&[b'x'; IO_CHUNK]);
            }
            assert!(engine.ingress.osc.is_empty());
            engine.process(b"\x1b\\after");
            assert!(engine.snapshot().text().starts_with("after"));
            assert!(engine.snapshot().error.is_none());
        }
    }

    #[test]
    fn query_limits_are_nonfatal_and_leave_space_for_user_interrupts() {
        let mut engine = engine(4, 40);
        for _ in 0..32 {
            engine.process(&b"\x1b[c".repeat(2000));
        }
        assert!(engine.snapshot().output_limited);
        assert!(engine.snapshot().error.is_none());
        assert!(engine.pending_bytes <= MAX_PENDING_REPLIES);
        engine.queue(&[3]).unwrap();
        assert_eq!(engine.pending.back().unwrap(), &[3]);
    }

    #[test]
    fn scrollback_and_input_queue_are_bounded() {
        let mut engine = engine(3, 20);
        for n in 0..150 {
            engine.process(format!("line{n}\r\n").as_bytes());
        }
        engine.term.scroll_display(Scroll::Top);
        assert!(engine.snapshot().display_offset <= 100);
        assert!(!engine.snapshot().text().contains("line0\n"));
        assert!(engine.queue(&vec![0; MAX_PENDING_INPUT + 1]).is_err());
        assert_eq!(engine.pending_bytes, 0);
    }

    #[test]
    fn pathological_combining_marks_do_not_expand_a_cell_without_bound() {
        let mut engine = engine(4, 20);
        engine.process("a".as_bytes());
        engine.process("\u{301}".repeat(4000).as_bytes());
        let screen = engine.snapshot();
        assert_eq!(
            screen.cells[0].text.chars().count(),
            MAX_COMBINING_MARKS + 1
        );
        assert!(screen.output_limited);
        assert_eq!(
            engine.term.grid()[Point::new(Line(0), Column(0))]
                .zerowidth()
                .unwrap()
                .len(),
            MAX_COMBINING_MARKS
        );
    }

    #[test]
    fn cursor_visibility_shape_and_maximum_snapshot_wire_size() {
        let mut engine = engine(50, 240);
        engine.process(b"\x1b[6 q");
        let cursor = engine.snapshot().cursor.unwrap();
        assert_eq!(cursor.shape, TerminalCursorShape::Beam);
        assert!(!cursor.blinking);
        engine.process(b"\x1b[?25l");
        assert!(engine.snapshot().cursor.is_none());
        let mut screen = engine.snapshot();
        for cell in &mut screen.cells {
            cell.text = format!("𐀀{}", "\u{e0100}".repeat(MAX_COMBINING_MARKS));
            cell.fg = TerminalColor::Rgb(255, 255, 255);
            cell.bg = TerminalColor::Rgb(255, 255, 255);
        }
        let encoded = serde_json::to_vec(&screen).unwrap();
        assert!(
            encoded.len() < 4 * 1024 * 1024 - 8192,
            "snapshot uses {} bytes",
            encoded.len()
        );
    }
}
