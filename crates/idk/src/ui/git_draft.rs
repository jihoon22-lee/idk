//! Commit messages remain local drafts until a concrete index review is submitted.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

#[derive(Clone, Debug, Default)]
pub(super) struct CommitDraft {
    pub text: String,
    pub cursor: usize,
}
impl CommitDraft {
    pub fn insert(&mut self, text: &str) -> Result<(), &'static str> {
        let text = text.replace("\r\n", "\n");
        if text
            .chars()
            .any(|character| character.is_control() && character != '\n')
        {
            return Err("Only message text and line breaks can be inserted.");
        }
        if self.text.len() + text.len() > 65536 {
            return Err("Commit message exceeds 64 KiB; the draft is unchanged.");
        }
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        Ok(())
    }
    fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1)
    }
    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index)
    }
    pub fn position(&self) -> (usize, usize) {
        (
            self.text[..self.cursor]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
            Line::from(&self.text[self.line_start()..self.cursor]).width(),
        )
    }
    fn vertical(&mut self, down: bool) {
        let column = self.position().1;
        let (start, end) = if down {
            let end = self.line_end();
            if end == self.text.len() {
                return;
            }
            let start = end + 1;
            (
                start,
                self.text[start..]
                    .find('\n')
                    .map_or(self.text.len(), |index| start + index),
            )
        } else {
            let start = self.line_start();
            if start == 0 {
                return;
            }
            let end = start - 1;
            (
                self.text[..end].rfind('\n').map_or(0, |index| index + 1),
                end,
            )
        };
        let mut width = 0;
        self.cursor = start;
        for (index, character) in self.text[start..end].char_indices() {
            let next = Line::from(character.to_string()).width();
            if width + next > column {
                break;
            }
            width += next;
            self.cursor = start + index + character.len_utf8();
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Result<(), &'static str> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Home if control => self.cursor = 0,
            KeyCode::End if control => self.cursor = self.text.len(),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Char('a') if control => self.cursor = self.line_start(),
            KeyCode::Char('e') if control => self.cursor = self.line_end(),
            KeyCode::Char('u') if control => {
                let start = self.line_start();
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('k') if control => {
                let end = self.line_end();
                self.text.drain(self.cursor..end);
            }
            KeyCode::Left => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(index, _)| index)
            }
            KeyCode::Right => {
                if let Some(character) = self.text[self.cursor..].chars().next() {
                    self.cursor += character.len_utf8();
                }
            }
            KeyCode::Up => self.vertical(false),
            KeyCode::Down => self.vertical(true),
            KeyCode::Enter => self.insert("\n")?,
            KeyCode::Backspace if self.cursor > 0 => {
                let index = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .unwrap()
                    .0;
                self.text.drain(index..self.cursor);
                self.cursor = index;
            }
            KeyCode::Delete => {
                if let Some(character) = self.text[self.cursor..].chars().next() {
                    self.text
                        .drain(self.cursor..self.cursor + character.len_utf8());
                }
            }
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.insert(&character.to_string())?
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_unicode_draft_preserves_boundaries_and_rejects_controls_atomically() {
        let mut draft = CommitDraft::default();
        draft.insert("제목\r\n\r\n본문 text").unwrap();
        assert_eq!(draft.text, "제목\n\n본문 text");
        let unchanged = draft.text.clone();
        assert!(draft.insert("\x1b]52;secret").is_err());
        assert_eq!(draft.text, unchanged);
        draft
            .key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
            .unwrap();
        draft
            .key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(draft.text, "제목\n본문 text");
        draft
            .key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .unwrap();
        draft
            .key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(draft.cursor, "제".len());
        draft
            .key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(draft.text, "제\n본문 text");
        let unchanged = draft.text.clone();
        assert!(draft.insert(&"x".repeat(65536)).is_err());
        assert_eq!(draft.text, unchanged);
    }
}
