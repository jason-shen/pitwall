//! Text helpers for the terminal view: searching the scrollback, and finding the
//! URL or path under the pointer. Kept free of egui and vt100 so they're testable.

use regex::Regex;
use std::sync::OnceLock;

/// One row of terminal text, with the screen column each character sits in;
/// wide (CJK) characters take two columns, so char index != column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub text: String,
    pub cols: Vec<u16>,
}

impl Line {
    pub fn char_at_col(&self, col: u16) -> Option<usize> {
        self.cols.iter().rposition(|&c| c <= col)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// Line number counted from the oldest scrollback line.
    pub line: usize,
    pub col: u16,
    /// Columns covered.
    pub width: u16,
}

/// Case-insensitive unless the query has a capital, the way editors and
/// `less -i` behave.
pub fn find(lines: &[Line], query: &str) -> Vec<Match> {
    if query.is_empty() {
        return vec![];
    }
    let fold = !query.chars().any(char::is_uppercase);
    let q: Vec<char> = if fold {
        query.to_lowercase().chars().collect()
    } else {
        query.chars().collect()
    };
    let mut out = vec![];
    for (n, l) in lines.iter().enumerate() {
        let chars: Vec<char> = if fold {
            l.text.chars().flat_map(char::to_lowercase).collect()
        } else {
            l.text.chars().collect()
        };
        // to_lowercase can change the char count (rare); fall back to no folding then.
        let chars = if chars.len() == l.cols.len() {
            chars
        } else {
            l.text.chars().collect()
        };
        let mut i = 0;
        while i + q.len() <= chars.len() {
            if chars[i..i + q.len()] == q[..] {
                let col = l.cols[i];
                let end = l
                    .cols
                    .get(i + q.len())
                    .copied()
                    .unwrap_or_else(|| l.cols[i + q.len() - 1] + 1);
                out.push(Match {
                    line: n,
                    col,
                    width: end.saturating_sub(col).max(1),
                });
                i += q.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Paste into ~/.zshrc or ~/.bashrc: reports each command's exit status (OSC
/// 133;D) so the command marks turn green or red.
pub const SHELL_INTEGRATION: &str = r#"# pitwall shell integration: exit codes for command marks
_pitwall_prompt() { local s=$?; printf '\033]133;D;%s\007' "$s"; return $s; }
if [ -n "$ZSH_VERSION" ]; then precmd_functions+=(_pitwall_prompt); else PROMPT_COMMAND="_pitwall_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"; fi"#;

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Url(String),
    Path(String),
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\b(?:https?|ftp|file)://[^\s<>"'`|\\^{}]+"#).unwrap())
}

fn path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?:~|\.{1,2})?/[\w.@%+~:,-]+(?:/[\w.@%+~:,-]*)*"#).unwrap())
}

/// The link under column `col`, and the columns it spans. Trailing punctuation
/// that's almost always sentence, not URL ("see https://x.dev/a." or "(at /tmp)")
/// is left off.
pub fn link_at(line: &Line, col: u16) -> Option<(Link, u16, u16)> {
    let at = line.char_at_col(col)?;
    // Regex offsets are bytes; convert to char indices.
    let to_char = |b: usize| line.text[..b].chars().count();
    let trim = |s: &str| -> usize {
        let mut s = s;
        loop {
            let t = s.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"']);
            let t = if t.ends_with(')') && t.matches('(').count() < t.matches(')').count() {
                &t[..t.len() - 1]
            } else {
                t
            };
            if t.len() == s.len() {
                return s.len();
            }
            s = t;
        }
    };
    let span = |m: regex::Match| {
        let start = to_char(m.start());
        let end = to_char(m.start() + trim(m.as_str()));
        (start, end)
    };
    for m in url_re().find_iter(&line.text) {
        let (s, e) = span(m);
        if (s..e).contains(&at) {
            let text: String = line.text.chars().skip(s).take(e - s).collect();
            return Some((Link::Url(text), line.cols[s], line.cols[e - 1] + 1));
        }
    }
    for m in path_re().find_iter(&line.text) {
        let (s, e) = span(m);
        // A lone "/" or "//" is a separator, not a path.
        if (s..e).contains(&at) && e - s > 2 {
            let text: String = line.text.chars().skip(s).take(e - s).collect();
            return Some((Link::Path(text), line.cols[s], line.cols[e - 1] + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Line {
        let mut cols = vec![];
        let mut c = 0u16;
        for ch in s.chars() {
            cols.push(c);
            // good enough for the tests: CJK takes two columns
            c += if ch > '\u{2e7f}' { 2 } else { 1 };
        }
        Line {
            text: s.to_string(),
            cols,
        }
    }

    #[test]
    fn finds_across_lines_case_insensitively() {
        let lines = [
            line("error: one"),
            line("no match"),
            line("ERROR and Error"),
        ];
        let m = find(&lines, "error");
        assert_eq!(
            m.iter().map(|m| (m.line, m.col)).collect::<Vec<_>>(),
            [(0, 0), (2, 0), (2, 10)]
        );
        // a capital makes it exact
        assert_eq!(find(&lines, "Error").len(), 1);
        assert!(find(&lines, "").is_empty());
    }

    #[test]
    fn columns_account_for_wide_characters() {
        let lines = [line("中文 error")];
        let m = find(&lines, "error");
        assert_eq!((m[0].col, m[0].width), (5, 5));
    }

    #[test]
    fn links_under_the_pointer() {
        let l = line("see https://pitwall.dev/docs/x?y=1. and (/usr/local/bin) ok");
        assert_eq!(
            link_at(&l, 10),
            Some((Link::Url("https://pitwall.dev/docs/x?y=1".into()), 4, 34))
        );
        assert_eq!(link_at(&l, 2), None);
        let (p, s, _) = link_at(&l, 42).unwrap();
        assert_eq!((p, s), (Link::Path("/usr/local/bin".into()), 41));
        assert_eq!(
            link_at(&line("~/dev/pitwall/src/app.rs:42"), 3).unwrap().0,
            Link::Path("~/dev/pitwall/src/app.rs:42".into())
        );
        assert_eq!(link_at(&line("a / b"), 2), None);
    }
}
