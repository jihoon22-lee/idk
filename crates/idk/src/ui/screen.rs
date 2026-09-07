//! A VT snapshot is data. Only sanitized cells are rendered; escape strings are never replayed.
use crate::terminal::{TerminalColor, TerminalCursorShape, TerminalSnapshot, MAX_TERMINAL_CELLS};
use anyhow::{ensure, Result};
use crossterm::cursor::SetCursorStyle;
use ratatui::{
    buffer::{Buffer, CellDiffOption},
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    widgets::{Clear, Widget},
    Frame,
};
use std::num::NonZeroU16;

pub fn validate(snapshot: &TerminalSnapshot) -> Result<()> {
    let count = usize::from(snapshot.rows) * usize::from(snapshot.cols);
    ensure!(
        snapshot.rows > 0 && snapshot.cols >= 2 && count <= MAX_TERMINAL_CELLS,
        "Invalid terminal screen dimensions."
    );
    ensure!(
        snapshot.cells.len() == count,
        "Incomplete terminal screen snapshot."
    );
    ensure!(
        snapshot.cells.iter().all(|cell| cell.text.len() <= 256),
        "Terminal cell exceeds text limit."
    );
    Ok(())
}

pub struct TerminalView<'a> {
    pub snapshot: &'a TerminalSnapshot,
}

impl Widget for TerminalView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        Clear.render(area, buffer);
        if validate(self.snapshot).is_err() {
            return;
        }
        for row in 0..self.snapshot.rows.min(area.height) {
            for col in 0..self.snapshot.cols.min(area.width) {
                let source = &self.snapshot.cells
                    [usize::from(row) * usize::from(self.snapshot.cols) + usize::from(col)];
                let destination = &mut buffer[(area.x + col, area.y + row)];
                let mut modifiers = Modifier::empty();
                for (enabled, flag) in [
                    (source.bold, Modifier::BOLD),
                    (source.dim, Modifier::DIM),
                    (source.italic, Modifier::ITALIC),
                    (source.underline, Modifier::UNDERLINED),
                    (source.inverse, Modifier::REVERSED),
                    (source.strike, Modifier::CROSSED_OUT),
                ] {
                    if enabled {
                        modifiers.insert(flag);
                    }
                }
                destination.set_style(
                    Style::default()
                        .fg(color(&source.fg))
                        .bg(color(&source.bg))
                        .add_modifier(modifiers),
                );
                if source.wide_spacer {
                    destination.set_symbol(" ");
                    if col > 0
                        && self.snapshot.cells[usize::from(row) * usize::from(self.snapshot.cols)
                            + usize::from(col)
                            - 1]
                        .wide
                    {
                        destination.set_diff_option(CellDiffOption::Skip);
                    }
                    continue;
                }
                if source.hidden || (source.wide && col + 1 >= area.width) {
                    destination.set_symbol(" ");
                    continue;
                }
                let text: String = source
                    .text
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                destination.set_symbol(if text.is_empty() { " " } else { &text });
                destination.set_diff_option(CellDiffOption::ForcedWidth(
                    NonZeroU16::new(if source.wide { 2 } else { 1 }).unwrap(),
                ));
            }
        }
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: &TerminalSnapshot, show_cursor: bool) {
    frame.render_widget(TerminalView { snapshot }, area);
    if show_cursor && validate(snapshot).is_ok() {
        if let Some(cursor) = &snapshot.cursor {
            if cursor.row < area.height && cursor.col < area.width && snapshot.display_offset == 0 {
                frame.set_cursor_position(Position::new(area.x + cursor.col, area.y + cursor.row));
            }
        }
    }
}

pub fn cursor_style(snapshot: &TerminalSnapshot) -> Option<SetCursorStyle> {
    snapshot
        .cursor
        .as_ref()
        .map(|cursor| match (cursor.shape, cursor.blinking) {
            (TerminalCursorShape::Underline, true) => SetCursorStyle::BlinkingUnderScore,
            (TerminalCursorShape::Underline, false) => SetCursorStyle::SteadyUnderScore,
            (TerminalCursorShape::Beam, true) => SetCursorStyle::BlinkingBar,
            (TerminalCursorShape::Beam, false) => SetCursorStyle::SteadyBar,
            (_, true) => SetCursorStyle::BlinkingBlock,
            (_, false) => SetCursorStyle::SteadyBlock,
        })
}

fn color(color: &TerminalColor) -> Color {
    match color {
        TerminalColor::Default => Color::Reset,
        TerminalColor::Indexed(index) => Color::Indexed(*index),
        TerminalColor::Rgb(r, g, b) => Color::Rgb(*r, *g, *b),
    }
}
