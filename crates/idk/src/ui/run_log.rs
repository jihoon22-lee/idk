//! Streaming plain log display, independent of terminal viewport generations.
use crate::{
    problems::{safe_utf8, Controls},
    run_wire::LogChunk,
};
use anyhow::{ensure, Result};
use base64::Engine;
#[derive(Default)]
pub(super) struct LogView {
    pub run_id: String,
    pub generation: u64,
    pub next: u64,
    pub limited: bool,
    pub follow: bool,
    pub scroll: u16,
    pub eof: bool,
    bytes: Vec<u8>,
    controls: Controls,
}
impl LogView {
    pub fn new(run_id: String, generation: u64, offset: u64) -> Self {
        Self {
            run_id,
            generation,
            next: offset,
            follow: true,
            ..Self::default()
        }
    }
    pub fn push(&mut self, chunk: &LogChunk) -> Result<()> {
        ensure!(
            self.run_id == chunk.descriptor.run_id
                && self.generation == chunk.descriptor.generation,
            "Log generation changed; refresh the Run before reading again"
        );
        ensure!(
            self.next == chunk.offset,
            "Log byte position changed; stale chunk discarded"
        );
        let bytes = base64::engine::general_purpose::STANDARD.decode(&chunk.data_base64)?;
        ensure!(
            chunk.next_offset == chunk.offset + bytes.len() as u64,
            "Invalid raw log offset"
        );
        self.bytes.extend(
            bytes
                .into_iter()
                .filter_map(|byte| self.controls.consume(byte)),
        );
        if self.bytes.len() > 262144 {
            let mut cut = self.bytes.len() - 262144;
            while cut < self.bytes.len() && self.bytes[cut] & 0xc0 == 0x80 {
                cut += 1;
            }
            self.bytes.drain(..cut);
            self.limited = true;
        }
        self.next = chunk.next_offset;
        self.eof = chunk.eof;
        Ok(())
    }
    pub fn text(&self) -> String {
        // Incomplete trailing UTF-8 remains buffered until the next chunk arrives.
        let mut start = self.bytes.len().saturating_sub(1);
        while start > 0 && self.bytes[start] & 0xc0 == 0x80 && self.bytes.len() - start < 4 {
            start -= 1;
        }
        let end = match std::str::from_utf8(&self.bytes[start..]) {
            Err(error) if error.error_len().is_none() => start + error.valid_up_to(),
            _ => self.bytes.len(),
        };
        safe_utf8(&self.bytes[..end], 262144)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_wire::{LogDescriptor, LogState};
    fn chunk(offset: u64, generation: u64, bytes: &[u8]) -> LogChunk {
        LogChunk {
            descriptor: LogDescriptor {
                run_id: "r".into(),
                generation,
                state: LogState::Recording,
                bytes: offset + bytes.len() as u64,
                observed_bytes: offset + bytes.len() as u64,
                limit_bytes: 1048576,
                merged_pty: true,
                file_identity: None,
            },
            offset,
            next_offset: offset + bytes.len() as u64,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            eof: false,
        }
    }
    #[test]
    fn split_unicode_controls_and_raw_offsets_survive_bounded_follow() {
        let mut view = LogView::new("r".into(), 7, 0);
        let first = b"\xffok\x1b]52;c;secret";
        view.push(&chunk(0, 7, first)).unwrap();
        assert_eq!(view.text(), "�ok");
        let next = b"\x07\xed\x95";
        view.push(&chunk(view.next, 7, next)).unwrap();
        assert_eq!(view.text(), "�ok");
        let next = b"\x9c\xea\xb8\x80\x1b[31m warning\x1b[0m\xe2\x80\xae";
        view.push(&chunk(view.next, 7, next)).unwrap();
        assert_eq!(view.text(), "�ok한글 warning");
        let unchanged = view.text();
        let offset = view.next;
        assert!(view.push(&chunk(offset, 8, b"stale")).is_err());
        assert!(view.push(&chunk(0, 7, b"stale")).is_err());
        assert_eq!(view.next, offset);
        assert_eq!(view.text(), unchanged);
        for _ in 0..5 {
            view.push(&chunk(view.next, 7, &vec![b'x'; 65536])).unwrap();
        }
        assert!(view.limited);
        assert!(view.text().len() <= 262144);
        assert_eq!(view.next, offset + 5 * 65536);
        assert!(!view.text().contains("secret"));
    }
}
