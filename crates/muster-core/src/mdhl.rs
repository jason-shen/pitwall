//! Markdown colouring for the editor. Syntect's Markdown grammar paints whole
//! list items in one loud colour; prose should stay prose, with only the
//! structure (headings, markers, code, links, guide comments) set apart.

use crate::editor::Rgb;

struct Palette {
    text: Rgb,
    dim: Rgb,
    heading: Rgb,
    marker: Rgb,
    done: Rgb,
    strong: Rgb,
    code: Rgb,
    link: Rgb,
}

const DARK: Palette = Palette {
    text: (0xd4, 0xd7, 0xdd),
    dim: (0x5c, 0x63, 0x70),
    heading: (0x82, 0xaa, 0xff),
    marker: (0x7f, 0x8c, 0x99),
    done: (0x7f, 0xc8, 0x8a),
    strong: (0xe6, 0xc0, 0x7a),
    code: (0xc3, 0xa6, 0xff),
    link: (0x6c, 0xc4, 0xd8),
};

const LIGHT: Palette = Palette {
    text: (0x2b, 0x2f, 0x36),
    dim: (0x9a, 0xa0, 0xa8),
    heading: (0x1f, 0x5f, 0xc8),
    marker: (0x6a, 0x73, 0x7d),
    done: (0x2e, 0x8b, 0x3e),
    strong: (0x9a, 0x5b, 0x00),
    code: (0x7a, 0x3e, 0xc8),
    link: (0x00, 0x7a, 0x99),
};

#[derive(Default)]
struct State {
    fence: bool,
    comment: bool,
}

/// Spans of (colour, byte length) for each line, as the editor draws them.
pub fn highlight(lines: &[String], light: bool) -> Vec<Vec<(Rgb, usize)>> {
    let p = if light { &LIGHT } else { &DARK };
    let mut st = State::default();
    lines.iter().map(|l| line(l, p, &mut st)).collect()
}

/// Joins neighbouring runs of the same colour.
struct Out(Vec<(Rgb, usize)>);

impl Out {
    fn push(&mut self, c: Rgb, len: usize) {
        if len == 0 {
            return;
        }
        match self.0.last_mut() {
            Some((last, n)) if *last == c => *n += len,
            _ => self.0.push((c, len)),
        }
    }
}

fn line(l: &str, p: &Palette, st: &mut State) -> Vec<(Rgb, usize)> {
    let mut out = Out(vec![]);
    let t = l.trim_start();
    if !st.comment && (t.starts_with("```") || t.starts_with("~~~")) {
        st.fence = !st.fence;
        out.push(p.dim, l.len());
        return out.0;
    }
    if st.fence {
        out.push(p.code, l.len());
        return out.0;
    }
    let mut i = 0;
    if st.comment {
        match l.find("-->") {
            Some(e) => {
                st.comment = false;
                i = e + 3;
                out.push(p.dim, i);
            }
            None => {
                out.push(p.dim, l.len());
                return out.0;
            }
        }
    }
    if i == 0 {
        let hashes = t.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            out.push(p.heading, l.len());
            return out.0;
        }
        i = lead(l, p, &mut out);
    }
    inline(l, i, p, st, &mut out);
    out.0
}

/// Indent, a list, number or quote marker, and a task box. Returns where
/// the text after them starts.
fn lead(l: &str, p: &Palette, out: &mut Out) -> usize {
    let indent = l.len() - l.trim_start().len();
    out.push(p.text, indent);
    let rest = &l[indent..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let marker = if ["- ", "* ", "+ ", "> "].iter().any(|m| rest.starts_with(m)) {
        2
    } else if digits > 0 && matches!(rest.as_bytes().get(digits), Some(b'.' | b')')) {
        digits + 1
    } else {
        return indent;
    };
    out.push(p.marker, marker);
    let mut i = indent + marker;
    let after = &l[i..];
    for (b, done) in [("[ ]", false), ("[x]", true), ("[X]", true)] {
        if after.starts_with(b) {
            out.push(if done { p.done } else { p.marker }, 3);
            i += 3;
        }
    }
    i
}

fn inline(l: &str, mut i: usize, p: &Palette, st: &mut State, out: &mut Out) {
    while i < l.len() {
        let rest = &l[i..];
        if let Some(after) = rest.strip_prefix("<!--") {
            match after.find("-->") {
                Some(e) => {
                    out.push(p.dim, e + 7);
                    i += e + 7;
                }
                None => {
                    st.comment = true;
                    out.push(p.dim, rest.len());
                    return;
                }
            }
        } else if let Some(n) = rest.strip_prefix('`').and_then(|r| r.find('`')) {
            out.push(p.code, n + 2);
            i += n + 2;
        } else if let Some(n) = rest.strip_prefix("**").and_then(|r| r.find("**")).filter(|n| *n > 0) {
            out.push(p.dim, 2);
            out.push(p.strong, n);
            out.push(p.dim, 2);
            i += n + 4;
        } else if let Some((text, url)) = link(rest) {
            out.push(p.dim, 1);
            out.push(p.link, text);
            out.push(p.dim, url);
            i += 1 + text + url;
        } else {
            let n = rest.chars().next().map_or(1, char::len_utf8);
            out.push(p.text, n);
            i += n;
        }
    }
}

/// `[text](url)`: the text's length and the length of `](url)`.
fn link(rest: &str) -> Option<(usize, usize)> {
    let r = rest.strip_prefix('[')?;
    let close = r.find("](")?;
    if r[..close].contains(']') {
        return None;
    }
    let end = r[close + 2..].find(')')?;
    Some((close, end + 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colours(l: &str) -> Vec<(Rgb, String)> {
        let mut st = State::default();
        let mut at = 0;
        line(l, &DARK, &mut st)
            .into_iter()
            .map(|(c, n)| {
                let s = l[at..at + n].to_string();
                at += n;
                (c, s)
            })
            .collect()
    }

    #[test]
    fn prose_in_a_list_stays_text_coloured() {
        let c = colours("- [x] Write **the** `parser`");
        assert_eq!(c[0], (DARK.marker, "- ".into()));
        assert_eq!(c[1], (DARK.done, "[x]".into()));
        assert!(c.contains(&(DARK.text, " Write ".into())));
        assert!(c.contains(&(DARK.strong, "the".into())));
        assert!(c.contains(&(DARK.code, "`parser`".into())));
    }

    #[test]
    fn spans_cover_every_byte_and_comments_carry_over() {
        let lines: Vec<String> = ["<!-- guide", "still guide -->", "# Title", "```", "let x = 1;", "```", "see [docs](http://x) é"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let h = highlight(&lines, false);
        for (l, spans) in lines.iter().zip(&h) {
            assert_eq!(spans.iter().map(|s| s.1).sum::<usize>(), l.len(), "{l}");
        }
        assert_eq!(h[1], vec![(DARK.dim, lines[1].len())]);
        assert_eq!(h[2], vec![(DARK.heading, 7)]);
        assert_eq!(h[4], vec![(DARK.code, 10)]);
    }
}
