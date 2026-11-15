//! A small built-in editor: open, edit, save, undo/redo, find, goto line,
//! syntax highlighting, and live reload when an agent changes the file on disk.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use anyhow::Result;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthChar;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(SyntaxSet::load_defaults_newlines)
}
fn themes() -> &'static ThemeSet {
    static T: OnceLock<ThemeSet> = OnceLock::new();
    T.get_or_init(ThemeSet::load_defaults)
}

/// Plain RGB so the editor works with any UI toolkit.
pub type Rgb = (u8, u8, u8);

const MAX_HIGHLIGHT_LINES: usize = 20_000;

#[derive(Clone)]
struct UndoState {
    lines: Vec<String>,
    cy: usize,
    cx: usize,
}

pub struct Editor {
    pub path: PathBuf,
    pub lines: Vec<String>,
    /// Cursor: line index and char index within the line.
    pub cy: usize,
    pub cx: usize,
    want_col: Option<usize>,
    pub scroll_y: usize,
    pub scroll_x: usize,
    pub dirty: bool,
    /// File changed on disk while we had unsaved edits.
    pub stale: bool,
    pub readonly: bool,
    mtime: Option<SystemTime>,
    undo: Vec<UndoState>,
    redo: Vec<UndoState>,
    last_edit: Option<(Instant, u8)>,
    pub highlight: Vec<Vec<(Rgb, usize)>>,
    hl_dirty: Option<Instant>,
    syntax: Option<String>,
    pub last_search: Option<String>,
    pub tab_width: usize,
    /// Scroll so the cursor is visible on the next draw.
    pub follow: bool,
    uses_tabs: bool,
    crlf: bool,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Editor {
    pub fn open(path: &Path, tab_width: usize) -> Result<Editor> {
        let bytes = std::fs::read(path).or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(vec![])
            } else {
                Err(e)
            }
        })?;
        let binary = bytes.iter().take(8000).any(|b| *b == 0);
        let text = if binary {
            format!("<binary file, {} bytes>", bytes.len())
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let crlf = text.contains("\r\n");
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        let uses_tabs = lines.iter().filter(|l| l.starts_with('\t')).count()
            > lines.iter().filter(|l| l.starts_with("  ")).count();
        let syntax = syntaxes()
            .find_syntax_for_file(path)
            .ok()
            .flatten()
            .map(|s| s.name.clone());
        let mut e = Editor {
            path: path.to_path_buf(),
            lines,
            cy: 0,
            cx: 0,
            want_col: None,
            scroll_y: 0,
            scroll_x: 0,
            dirty: false,
            stale: false,
            readonly: binary,
            mtime: mtime(path),
            undo: vec![],
            redo: vec![],
            last_edit: None,
            highlight: vec![],
            hl_dirty: Some(Instant::now() - std::time::Duration::from_secs(1)),
            syntax,
            last_search: None,
            tab_width: tab_width.max(1),
            follow: true,
            uses_tabs,
            crlf,
        };
        e.rehighlight_if_due("base16-ocean.dark");
        Ok(e)
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    pub fn save(&mut self) -> Result<()> {
        if self.readonly {
            anyhow::bail!("file is read-only (binary)");
        }
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self.lines.join(nl);
        s.push_str(nl);
        std::fs::write(&self.path, s)?;
        self.dirty = false;
        self.stale = false;
        self.mtime = mtime(&self.path);
        Ok(())
    }

    /// Pick up changes made on disk (e.g. by an agent). Returns true if reloaded.
    pub fn check_disk(&mut self) -> bool {
        let m = mtime(&self.path);
        if m.is_none() || m == self.mtime {
            return false;
        }
        if self.dirty {
            self.stale = true;
            self.mtime = m;
            return false;
        }
        self.reload();
        true
    }

    pub fn reload(&mut self) {
        let (cy, cx, sy) = (self.cy, self.cx, self.scroll_y);
        if let Ok(mut fresh) = Editor::open(&self.path, self.tab_width) {
            fresh.cy = cy.min(fresh.lines.len() - 1);
            fresh.cx = cx.min(fresh.lines[fresh.cy].chars().count());
            fresh.scroll_y = sy.min(fresh.lines.len().saturating_sub(1));
            fresh.last_search = self.last_search.take();
            *self = fresh;
        }
    }

    // ── highlighting ────────────────────────────────────────────────────

    pub fn rehighlight_if_due(&mut self, theme: &str) {
        let Some(t) = self.hl_dirty else { return };
        if t.elapsed().as_millis() < 150 {
            return;
        }
        self.hl_dirty = None;
        let Some(name) = &self.syntax else {
            self.highlight.clear();
            return;
        };
        if self.lines.len() > MAX_HIGHLIGHT_LINES {
            self.highlight.clear();
            return;
        }
        let ss = syntaxes();
        let Some(syntax) = ss.find_syntax_by_name(name) else {
            return;
        };
        let ts = themes();
        let theme = ts
            .themes
            .get(theme)
            .or_else(|| ts.themes.get("base16-ocean.dark"))
            .unwrap();
        let mut h = HighlightLines::new(syntax, theme);
        let mut out = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            let with_nl = format!("{line}\n");
            let spans = match h.highlight_line(&with_nl, ss) {
                Ok(r) => r
                    .into_iter()
                    .map(|(st, text)| {
                        (
                            (st.foreground.r, st.foreground.g, st.foreground.b),
                            text.len(),
                        )
                    })
                    .collect(),
                Err(_) => vec![],
            };
            out.push(spans);
        }
        self.highlight = out;
    }

    fn touched(&mut self) {
        self.dirty = true;
        self.hl_dirty = Some(Instant::now());
    }

    // ── undo ────────────────────────────────────────────────────────────

    /// Group consecutive edits of the same kind into one undo step.
    fn checkpoint(&mut self, kind: u8) {
        let now = Instant::now();
        let merge = matches!(self.last_edit, Some((t, k)) if k == kind && now.duration_since(t).as_millis() < 800);
        if !merge {
            self.undo.push(UndoState {
                lines: self.lines.clone(),
                cy: self.cy,
                cx: self.cx,
            });
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_edit = Some((now, kind));
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(UndoState {
                lines: self.lines.clone(),
                cy: self.cy,
                cx: self.cx,
            });
            self.lines = s.lines;
            self.cy = s.cy;
            self.cx = s.cx;
            self.last_edit = None;
            self.touched();
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(UndoState {
                lines: self.lines.clone(),
                cy: self.cy,
                cx: self.cx,
            });
            self.lines = s.lines;
            self.cy = s.cy;
            self.cx = s.cx;
            self.last_edit = None;
            self.touched();
        }
    }

    // ── cursor helpers ──────────────────────────────────────────────────

    fn line_len(&self, y: usize) -> usize {
        self.lines[y].chars().count()
    }

    fn byte_at(&self, y: usize, cx: usize) -> usize {
        self.lines[y]
            .char_indices()
            .nth(cx)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[y].len())
    }

    /// Display column of char index `cx` on line `y` (tabs and wide chars expanded).
    pub fn col_of(&self, y: usize, cx: usize) -> usize {
        let mut col = 0;
        for ch in self.lines[y].chars().take(cx) {
            col += self.char_width(ch, col);
        }
        col
    }

    pub fn char_width(&self, ch: char, col: usize) -> usize {
        if ch == '\t' {
            self.tab_width - (col % self.tab_width)
        } else {
            ch.width().unwrap_or(0)
        }
    }

    fn cx_for_col(&self, y: usize, target: usize) -> usize {
        let mut col = 0;
        for (i, ch) in self.lines[y].chars().enumerate() {
            let w = self.char_width(ch, col);
            if col + w > target {
                return i;
            }
            col += w;
        }
        self.line_len(y)
    }

    fn clamp(&mut self) {
        self.cy = self.cy.min(self.lines.len() - 1);
        self.cx = self.cx.min(self.line_len(self.cy));
    }

    pub fn ensure_visible(&mut self, height: usize, width: usize) {
        if self.cy < self.scroll_y {
            self.scroll_y = self.cy;
        } else if height > 0 && self.cy >= self.scroll_y + height {
            self.scroll_y = self.cy + 1 - height;
        }
        let col = self.col_of(self.cy, self.cx);
        if col < self.scroll_x {
            self.scroll_x = col;
        } else if width > 0 && col >= self.scroll_x + width {
            self.scroll_x = col + 1 - width;
        }
    }

    pub fn goto(&mut self, line: usize) {
        self.cy = line.saturating_sub(1).min(self.lines.len() - 1);
        self.cx = 0;
        self.want_col = None;
        self.scroll_y = self.cy.saturating_sub(5);
    }

    pub fn click(&mut self, row: usize, col: usize) {
        self.cy = (self.scroll_y + row).min(self.lines.len() - 1);
        self.cx = self.cx_for_col(self.cy, self.scroll_x + col);
        self.want_col = None;
    }

    pub fn scroll(&mut self, delta: isize) {
        let max = self.lines.len().saturating_sub(1) as isize;
        self.scroll_y = (self.scroll_y as isize + delta).clamp(0, max) as usize;
        if self.cy < self.scroll_y {
            self.cy = self.scroll_y;
            self.clamp();
        }
    }

    // ── movement ────────────────────────────────────────────────────────

    pub fn left(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        } else if self.cy > 0 {
            self.cy -= 1;
            self.cx = self.line_len(self.cy);
        }
        self.want_col = None;
    }
    pub fn right(&mut self) {
        if self.cx < self.line_len(self.cy) {
            self.cx += 1;
        } else if self.cy + 1 < self.lines.len() {
            self.cy += 1;
            self.cx = 0;
        }
        self.want_col = None;
    }
    pub fn vertical(&mut self, delta: isize) {
        let col = *self.want_col.get_or_insert(self.col_of(self.cy, self.cx));
        let ny = (self.cy as isize + delta).clamp(0, self.lines.len() as isize - 1) as usize;
        self.cy = ny;
        self.cx = self.cx_for_col(ny, col);
    }
    pub fn home(&mut self) {
        let indent = self.lines[self.cy]
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        self.cx = if self.cx == indent { 0 } else { indent };
        self.want_col = None;
    }
    pub fn end(&mut self) {
        self.cx = self.line_len(self.cy);
        self.want_col = None;
    }
    pub fn word_left(&mut self) {
        let chars: Vec<char> = self.lines[self.cy].chars().collect();
        if self.cx == 0 {
            return self.left();
        }
        let mut i = self.cx;
        while i > 0 && !chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        self.cx = i;
        self.want_col = None;
    }
    pub fn word_right(&mut self) {
        let chars: Vec<char> = self.lines[self.cy].chars().collect();
        if self.cx >= chars.len() {
            return self.right();
        }
        let mut i = self.cx;
        while i < chars.len() && chars[i].is_alphanumeric() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_alphanumeric() {
            i += 1;
        }
        self.cx = i;
        self.want_col = None;
    }
    pub fn top(&mut self) {
        self.cy = 0;
        self.cx = 0;
    }
    pub fn bottom(&mut self) {
        self.cy = self.lines.len() - 1;
        self.cx = self.line_len(self.cy);
    }

    // ── editing ─────────────────────────────────────────────────────────

    pub fn insert_char(&mut self, ch: char) {
        if self.readonly {
            return;
        }
        self.checkpoint(1);
        let b = self.byte_at(self.cy, self.cx);
        self.lines[self.cy].insert(b, ch);
        self.cx += 1;
        self.want_col = None;
        self.touched();
    }

    pub fn insert_str(&mut self, s: &str) {
        if self.readonly {
            return;
        }
        self.checkpoint(4);
        self.last_edit = None;
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let b = self.byte_at(self.cy, self.cx);
        let tail = self.lines[self.cy].split_off(b);
        let mut parts = s.split('\n');
        let first = parts.next().unwrap_or("");
        self.lines[self.cy].push_str(first);
        let mut y = self.cy;
        for p in parts {
            y += 1;
            self.lines.insert(y, p.to_string());
        }
        self.cx = self.lines[y].chars().count();
        self.lines[y].push_str(&tail);
        self.cy = y;
        self.want_col = None;
        self.touched();
    }

    pub fn tab(&mut self) {
        if self.uses_tabs {
            self.insert_char('\t');
        } else {
            let col = self.col_of(self.cy, self.cx);
            let n = self.tab_width - col % self.tab_width;
            for _ in 0..n {
                self.insert_char(' ');
            }
        }
    }

    pub fn newline(&mut self) {
        if self.readonly {
            return;
        }
        self.checkpoint(2);
        let indent: String = self.lines[self.cy]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let b = self.byte_at(self.cy, self.cx);
        let rest = self.lines[self.cy].split_off(b);
        let trimmed_rest = rest.trim_start_matches([' ', '\t']).to_string();
        let opens = self.lines[self.cy]
            .trim_end()
            .ends_with(['{', '(', '[', ':']);
        let extra = if opens {
            if self.uses_tabs {
                "\t".to_string()
            } else {
                " ".repeat(self.tab_width)
            }
        } else {
            String::new()
        };
        self.cy += 1;
        let new_indent = format!("{indent}{extra}");
        self.cx = new_indent.chars().count();
        self.lines
            .insert(self.cy, format!("{new_indent}{trimmed_rest}"));
        self.want_col = None;
        self.touched();
    }

    pub fn backspace(&mut self) {
        if self.readonly {
            return;
        }
        if self.cx == 0 && self.cy == 0 {
            return;
        }
        self.checkpoint(3);
        if self.cx > 0 {
            // remove a soft-tab worth of spaces if we're in leading indentation
            let line = &self.lines[self.cy];
            let before: String = line.chars().take(self.cx).collect();
            let n = if !self.uses_tabs && before.chars().all(|c| c == ' ') && !before.is_empty() {
                let r = before.len() % self.tab_width;
                if r == 0 {
                    self.tab_width
                } else {
                    r
                }
            } else {
                1
            };
            for _ in 0..n {
                let b = self.byte_at(self.cy, self.cx - 1);
                self.lines[self.cy].remove(b);
                self.cx -= 1;
            }
        } else {
            let line = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.line_len(self.cy);
            self.lines[self.cy].push_str(&line);
        }
        self.want_col = None;
        self.touched();
    }

    pub fn delete(&mut self) {
        if self.readonly {
            return;
        }
        self.checkpoint(3);
        if self.cx < self.line_len(self.cy) {
            let b = self.byte_at(self.cy, self.cx);
            self.lines[self.cy].remove(b);
        } else if self.cy + 1 < self.lines.len() {
            let next = self.lines.remove(self.cy + 1);
            self.lines[self.cy].push_str(&next);
        } else {
            return;
        }
        self.touched();
    }

    pub fn delete_line(&mut self) {
        if self.readonly {
            return;
        }
        self.checkpoint(5);
        self.last_edit = None;
        if self.lines.len() == 1 {
            self.lines[0].clear();
        } else {
            self.lines.remove(self.cy);
        }
        self.clamp();
        self.touched();
    }

    // ── search ──────────────────────────────────────────────────────────

    /// Find next occurrence (case-insensitive) after the cursor, wrapping around.
    pub fn find_next(&mut self, q: &str) -> bool {
        if q.is_empty() {
            return false;
        }
        self.last_search = Some(q.to_string());
        let needle = q.to_lowercase();
        let n = self.lines.len();
        for step in 0..=n {
            let y = (self.cy + step) % n;
            let hay = self.lines[y].to_lowercase();
            let start_byte = if step == 0 {
                self.byte_at(y, (self.cx + 1).min(self.line_len(y)))
            } else {
                0
            };
            let start_byte = start_byte.min(hay.len());
            if let Some(pos) = hay.get(start_byte..).and_then(|h| h.find(&needle)) {
                let byte = start_byte + pos;
                self.cy = y;
                self.cx = self.lines[y]
                    .get(..byte)
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                self.want_col = None;
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        let p = std::env::temp_dir().join(format!(
            "muster-ed-{}-{}.rs",
            std::process::id(),
            text.len()
        ));
        std::fs::write(&p, text).unwrap();
        Editor::open(&p, 4).unwrap()
    }

    #[test]
    fn edit_save_undo() {
        let mut e = ed("fn main() {\n}\n");
        assert_eq!(e.lines, vec!["fn main() {", "}"]);
        e.end();
        e.newline();
        assert_eq!(e.lines[1], "    ", "auto-indent after {{");
        for c in "let x = 1;".chars() {
            e.insert_char(c);
        }
        assert_eq!(e.lines[1], "    let x = 1;");
        e.save().unwrap();
        assert_eq!(
            std::fs::read_to_string(&e.path).unwrap(),
            "fn main() {\n    let x = 1;\n}\n"
        );
        e.undo();
        assert_eq!(e.lines[1], "    ");
        e.undo();
        assert_eq!(e.lines.len(), 2);
        e.redo();
        assert_eq!(e.lines.len(), 3);
        assert!(!e.highlight.is_empty(), "rust is highlighted");
        let _ = std::fs::remove_file(&e.path);
    }

    #[test]
    fn find_paste_and_reload() {
        let mut e = ed("alpha\nbeta\ngamma beta\n");
        assert!(e.find_next("BETA"));
        assert_eq!((e.cy, e.cx), (1, 0));
        assert!(e.find_next("beta"));
        assert_eq!((e.cy, e.cx), (2, 6));
        e.top();
        e.insert_str("x\ny");
        assert_eq!(e.lines[0], "x");
        assert_eq!(e.lines[1], "yalpha");
        // simulate an agent writing the file while we're clean
        e.save().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&e.path, "changed by agent\n").unwrap();
        assert!(e.check_disk());
        assert_eq!(e.lines, vec!["changed by agent"]);
        let _ = std::fs::remove_file(&e.path);
    }

    #[test]
    fn tabs_and_wide_columns() {
        let mut e = ed("\tx\n中文ab\n");
        assert_eq!(e.col_of(0, 1), 4);
        assert_eq!(e.col_of(1, 2), 4);
        e.cy = 1;
        e.cx = 0;
        e.vertical(-1);
        assert_eq!(e.cy, 0);
        let _ = std::fs::remove_file(&e.path);
    }
}
