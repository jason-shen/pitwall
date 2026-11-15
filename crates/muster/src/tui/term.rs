//! Render a vt100 screen into a ratatui buffer, and encode keys for the PTY.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

pub fn render(screen: &vt100::Screen, area: Rect, buf: &mut Buffer) {
    let (rows, cols) = screen.size();
    let h = area.height.min(rows.get());
    let w = area.width.min(cols.get());
    for r in 0..h {
        let mut c = 0;
        while c < w {
            let Some(cell) = screen.cell(r, c) else { break };
            let x = area.x + c;
            let y = area.y + r;
            if cell.is_wide_continuation() {
                c += 1;
                continue;
            }
            let mut st = Style::default()
                .fg(color(cell.fgcolor()))
                .bg(color(cell.bgcolor()));
            let mut m = Modifier::empty();
            if cell.bold() {
                m |= Modifier::BOLD;
            }
            if cell.italic() {
                m |= Modifier::ITALIC;
            }
            if cell.underline() {
                m |= Modifier::UNDERLINED;
            }
            if cell.inverse() {
                m |= Modifier::REVERSED;
            }
            if cell.dim() {
                m |= Modifier::DIM;
            }
            st = st.add_modifier(m);
            let s = cell.contents();
            let sym = if s.is_empty() { " " } else { s };
            if cell.is_wide() && c + 1 >= w {
                // no room for the second half
                buf[(x, y)].set_symbol(" ").set_style(st);
                break;
            }
            buf[(x, y)].set_symbol(sym).set_style(st);
            if cell.is_wide() {
                buf[(x + 1, y)].reset();
                c += 2;
            } else {
                c += 1;
            }
        }
    }
}

/// Bytes to send to the PTY for a key press.
pub fn encode(k: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    if k.kind == KeyEventKind::Release {
        return None;
    }
    let m = k.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);
    let md = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |c: char| -> Vec<u8> {
        if md > 1 {
            format!("\x1b[1;{md}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if md > 1 {
            format!("\x1b[{n};{md}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let mut out = match k.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                'A'..='Z' => c as u8 - b'A' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '/' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return None,
            };
            vec![b]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if ctrl {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi('A'),
        KeyCode::Down => csi('B'),
        KeyCode::Right => csi('C'),
        KeyCode::Left => csi('D'),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => tilde(15),
            6 => tilde(17),
            7 => tilde(18),
            8 => tilde(19),
            9 => tilde(20),
            10 => tilde(21),
            11 => tilde(23),
            12 => tilde(24),
            _ => return None,
        },
        _ => return None,
    };
    // Alt-prefix for plain characters/enter/backspace (arrows already carry it).
    if alt
        && matches!(
            k.code,
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace
        )
    {
        out.insert(0, 0x1b);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn encodes() {
        assert_eq!(
            encode(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Some(vec![3])
        );
        assert_eq!(
            encode(&key(KeyCode::Char('x'), KeyModifiers::ALT), false),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Left, KeyModifiers::CONTROL), false),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Char('中'), KeyModifiers::NONE), false),
            Some("中".as_bytes().to_vec())
        );
    }

    #[test]
    fn renders_wide_and_colors() {
        let mut p = vt100::Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(10).unwrap(),
            0,
        );
        p.process("中a\x1b[31mR".as_bytes());
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        render(p.screen(), area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "中");
        assert_eq!(buf[(2, 0)].symbol(), "a");
        assert_eq!(buf[(3, 0)].fg, Color::Indexed(1));
    }
}
