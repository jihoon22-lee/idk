//! Encode input for xterm-compatible modes, never replay output.
use crate::terminal::TerminalModes;
use anyhow::{bail, ensure, Result};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;

pub const MAX_PASTE_BYTES: usize = 64 * 1024;

pub fn encode_key(key: KeyEvent, modes: &TerminalModes) -> Result<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return Ok(Vec::new());
    }
    let modifiers = key.modifiers;
    let number = 1
        + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL));
    if modes.application_keypad && key.state.contains(KeyEventState::KEYPAD) && number == 1 {
        let final_byte = match key.code {
            KeyCode::Char(c @ '0'..='9') => Some(b'p' + (c as u8 - b'0')),
            KeyCode::Char('.') => Some(b'n'),
            KeyCode::Char('-') => Some(b'm'),
            KeyCode::Char('+') => Some(b'k'),
            KeyCode::Char('*') => Some(b'j'),
            KeyCode::Char('/') => Some(b'o'),
            KeyCode::Char('=') => Some(b'X'),
            KeyCode::Enter => Some(b'M'),
            _ => None,
        };
        if let Some(byte) = final_byte {
            return Ok(vec![0x1b, b'O', byte]);
        }
    }
    let navigation = match key.code {
        KeyCode::Up => Some('A'),
        KeyCode::Down => Some('B'),
        KeyCode::Right => Some('C'),
        KeyCode::Left => Some('D'),
        KeyCode::Home => Some('H'),
        KeyCode::End => Some('F'),
        _ => None,
    };
    if let Some(final_byte) = navigation {
        return Ok(if number != 1 {
            format!("\x1b[1;{number}{final_byte}").into_bytes()
        } else if modes.application_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        });
    }
    let tilde = match key.code {
        KeyCode::Insert => Some(2),
        KeyCode::Delete => Some(3),
        KeyCode::PageUp => Some(5),
        KeyCode::PageDown => Some(6),
        KeyCode::F(5) => Some(15),
        KeyCode::F(6) => Some(17),
        KeyCode::F(7) => Some(18),
        KeyCode::F(8) => Some(19),
        KeyCode::F(9) => Some(20),
        KeyCode::F(10) => Some(21),
        KeyCode::F(11) => Some(23),
        KeyCode::F(12) => Some(24),
        KeyCode::F(13) => Some(25),
        KeyCode::F(14) => Some(26),
        KeyCode::F(15) => Some(28),
        KeyCode::F(16) => Some(29),
        KeyCode::F(17) => Some(31),
        KeyCode::F(18) => Some(32),
        KeyCode::F(19) => Some(33),
        KeyCode::F(20) => Some(34),
        _ => None,
    };
    if let Some(code) = tilde {
        return Ok(if number == 1 {
            format!("\x1b[{code}~")
        } else {
            format!("\x1b[{code};{number}~")
        }
        .into_bytes());
    }
    if let KeyCode::F(code @ 1..=4) = key.code {
        let final_byte = char::from(b'P' + code - 1);
        return Ok(if number == 1 {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[1;{number}{final_byte}")
        }
        .into_bytes());
    }
    let mut bytes = match key.code {
        KeyCode::Char(character) => {
            if modifiers.contains(KeyModifiers::CONTROL) {
                let code = match character {
                    '@' | ' ' => 0,
                    c if u32::from(c) == 0x60 => 0,
                    'a'..='z' => character as u8 - b'a' + 1,
                    'A'..='Z' => character as u8 - b'A' + 1,
                    '[' => 27, '\\' => 28, ']' => 29, '^' => 30, '_' => 31, '?' => 127,
                    _ => bail!("This control-key combination is not representable in the terminal's input mode."),
                };
                vec![code]
            } else {
                character.to_string().into_bytes()
            }
        }
        KeyCode::Enter => {
            if modes.newline {
                b"\r\n".to_vec()
            } else {
                vec![b'\r']
            }
        }
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => return Ok(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => vec![if modifiers.contains(KeyModifiers::CONTROL) {
            8
        } else {
            127
        }],
        KeyCode::Esc => vec![27],
        KeyCode::Null => vec![0],
        KeyCode::CapsLock | KeyCode::ScrollLock | KeyCode::NumLock | KeyCode::Modifier(_) => {
            return Ok(Vec::new())
        }
        _ => bail!("This key is not supported by the terminal's input mode."),
    };
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 27);
    }
    Ok(bytes)
}

pub fn encode_paste(text: &str, modes: &TerminalModes) -> Result<Vec<u8>> {
    ensure!(
        text.len() + if modes.bracketed_paste { 12 } else { 0 } <= MAX_PASTE_BYTES,
        "Paste exceeds 64 KiB; nothing was sent."
    );
    ensure!(
        !text.contains(['\0', '\x1b']),
        "Paste containing NUL or ESC was not sent."
    );
    let mut bytes = Vec::with_capacity(text.len() + 12);
    if modes.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.extend_from_slice(text.as_bytes());
    if modes.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[201~");
    }
    Ok(bytes)
}

pub fn encode_focus(focused: bool, modes: &TerminalModes) -> Vec<u8> {
    if !modes.focus_reporting {
        Vec::new()
    } else if focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}

pub fn encode_mouse(event: MouseEvent, modes: &TerminalModes, area: Rect) -> Option<Vec<u8>> {
    if event.column < area.x
        || event.row < area.y
        || event.column >= area.right()
        || event.row >= area.bottom()
        || !(modes.mouse_click || modes.mouse_drag || modes.mouse_motion)
    {
        return None;
    }
    let button_code = |button| match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match event.kind {
        MouseEventKind::Down(button) => (button_code(button), false),
        MouseEventKind::Up(button) => (
            if modes.mouse_sgr {
                button_code(button)
            } else {
                3
            },
            true,
        ),
        MouseEventKind::Drag(button) if modes.mouse_drag || modes.mouse_motion => {
            (button_code(button) + 32, false)
        }
        MouseEventKind::Moved if modes.mouse_motion => (35, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        _ => return None,
    };
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let x = event.column - area.x + 1;
    let y = event.row - area.y + 1;
    if modes.mouse_sgr {
        return Some(
            format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes(),
        );
    }
    let mut bytes = b"\x1b[M".to_vec();
    bytes.push(code + 32);
    if modes.mouse_utf8 {
        if x > 2015 || y > 2015 {
            return None;
        }
        for coordinate in [x, y] {
            let mut encoded = [0; 4];
            bytes.extend_from_slice(
                char::from_u32(u32::from(coordinate) + 32)?
                    .encode_utf8(&mut encoded)
                    .as_bytes(),
            );
        }
    } else {
        if x > 223 || y > 223 {
            return None;
        }
        bytes.extend_from_slice(&[x as u8 + 32, y as u8 + 32]);
    }
    Some(bytes)
}
