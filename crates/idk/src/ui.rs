//! Thin B01 UI; project workflows are added with their data/runtime contracts.
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use ratatui::widgets::{Block, Borders, Paragraph};

pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    let outcome = (|| {
        loop {
            terminal.draw(|frame| {
                let panel = Paragraph::new("Project workspace\n\nThe native foundation is ready.\n\nq / Esc: close this screen\nRun `idk probe --shell /path/to/tcsh` for the synthetic compatibility check.")
                    .block(Block::default().borders(Borders::ALL).title(" idk 0.4 · foundation "));
                frame.render_widget(panel, frame.area());
            })?;
            if let Event::Key(key) = event::read()? {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break;
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    outcome
}
