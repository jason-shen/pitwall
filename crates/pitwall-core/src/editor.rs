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

/// Colour runs for a snippet that isn't a file, such as a command waiting
/// on approval: one list of (colour, byte length) per line, in the editor's
/// theme. `ext` picks the syntax ("sh"); unknown ones come back plain.
pub fn highlight_code(text: &str, ext: &str, theme: &str) -> Vec<Vec<(Rgb, usize)>> {
    let ss = syntaxes();
    let Some(syntax) = ss.find_syntax_by_extension(ext) else {
        return vec![];
    };
    let ts = themes();
    let Some(theme) = ts
        .themes
        .get(theme)
        .or_else(|| ts.themes.get("base16-ocean.dark"))
    else {
        return vec![];
    };
    let mut h = HighlightLines::new(syntax, theme);
    text.split('\n')
        .map(|line| {
            h.highlight_line(&format!("{line}\n"), ss)
                .map(|r| {
                    r.into_iter()
                        .map(|(st, t)| ((st.foreground.r, st.foreground.g, st.foreground.b), t.len()))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

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
    /// The other end of the selection (line, char); the cursor is one end.
    pub anchor: Option<(usize, usize)>,
    want_col: Option<usize>,
    pub scroll_y: usize,
    pub scroll_x: usize,
    pub dirty: bool,
    /// File changed on disk while we had unsaved edits.
    pub stale: bool,
    /// ⌘S was pressed once on a stale buffer; the next one overwrites.
    overwrite_ok: bool,
    /// A binary file: never edited.
    pub readonly: bool,
    /// Locked for now (a brief while Finish with AI writes it); kept across
    /// reloads, unlike `readonly`, which a reload works out afresh.
    pub locked: bool,
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

/// A Markdown list item's or quote's lead: `  - [ ] `, `3. `, `> `.
struct Lead {
    /// Bytes of the lead, task box included.
    len: usize,
    /// Where the marker ends and a task box would go.
    marker_end: usize,
    /// Some(done) when it's a task.
    task: Option<bool>,
    /// What the next line starts with.
    next: String,
}

fn md_lead(line: &str) -> Option<Lead> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, rest) = line.split_at(indent_len);
    let (marker, next_marker) = if let Some(m) = ["- ", "* ", "+ ", "> "]
        .into_iter()
        .find(|m| rest.starts_with(m))
    {
        (m.len(), m.to_string())
    } else {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let sep = rest.as_bytes().get(digits).copied();
        if !(1..=9).contains(&digits)
            || !matches!(sep, Some(b'.' | b')'))
            || !rest[digits + 1..].starts_with(' ')
        {
            return None;
        }
        let n: u64 = rest[..digits].parse().ok()?;
        (digits + 2, format!("{}{} ", n + 1, sep? as char))
    };
    let marker_end = indent_len + marker;
    let after = &line[marker_end..];
    // "- [ ]" with its trailing space trimmed away is still a task.
    let task = ["[ ]", "[x]", "[X]"]
        .into_iter()
        .find(|b| after.starts_with(b) && (after.len() == 3 || after[3..].starts_with(' ')))
        .map(|b| b != "[ ]");
    let len = match task {
        Some(_) => (marker_end + 4).min(line.len()),
        None => marker_end,
    };
    let task_next = if task.is_some() { "[ ] " } else { "" };
    Some(Lead {
        len,
        marker_end,
        task,
        next: format!("{indent}{next_marker}{task_next}"),
    })
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
            anchor: None,
            want_col: None,
            scroll_y: 0,
            scroll_x: 0,
            dirty: false,
            stale: false,
            overwrite_ok: false,
            readonly: binary,
            locked: false,
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
        if self.frozen() {
            anyhow::bail!("{} is read-only right now", self.name());
        }
        // Someone else (an agent) wrote it since: saving now would throw
        // their version away, so the first ⌘S only warns.
        if self.stale && !self.overwrite_ok {
            self.overwrite_ok = true;
            anyhow::bail!(
                "{} changed on disk: ⌘S again to overwrite it with yours, ⌘R to load theirs",
                self.name()
            );
        }
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self.lines.join(nl);
        s.push_str(nl);
        std::fs::write(&self.path, s)?;
        self.dirty = false;
        self.stale = false;
        self.overwrite_ok = false;
        self.mtime = mtime(&self.path);
        Ok(())
    }

    /// Pick up changes made on disk (e.g. by an agent). Returns true if reloaded.
    pub fn check_disk(&mut self) -> bool {
        self.check_disk_with(true)
    }

    /// `record`: whether the reload is its own undo step. A file being
    /// rewritten many times a second only wants the first.
    pub fn check_disk_with(&mut self, record: bool) -> bool {
        let m = mtime(&self.path);
        if m.is_none() || m == self.mtime {
            return false;
        }
        if self.dirty {
            self.stale = true;
            self.mtime = m;
            return false;
        }
        self.reload_with(record);
        true
    }

    pub fn reload(&mut self) {
        self.reload_with(true)
    }

    pub fn reload_with(&mut self, record: bool) {
        let (cy, cx, sy) = (self.cy, self.cx, self.scroll_y);
        if let Ok(mut fresh) = Editor::open(&self.path, self.tab_width) {
            fresh.cy = cy.min(fresh.lines.len() - 1);
            fresh.cx = cx.min(fresh.lines[fresh.cy].chars().count());
            fresh.scroll_y = sy.min(fresh.lines.len().saturating_sub(1));
            fresh.last_search = self.last_search.take();
            // Whoever locked it (a brief being written into) still wants it
            // locked; a fresh open only knows about binary files.
            fresh.locked = self.locked;
            // What was on screen before an agent rewrote the file is one ⌘Z
            // away; briefs aren't in git, so this is the only copy.
            fresh.undo = std::mem::take(&mut self.undo);
            if record && fresh.lines != self.lines {
                fresh.undo.push(UndoState {
                    lines: std::mem::take(&mut self.lines),
                    cy,
                    cx,
                });
                if fresh.undo.len() > 200 {
                    fresh.undo.remove(0);
                }
            }
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
        if self.is_markdown() && self.lines.len() <= MAX_HIGHLIGHT_LINES {
            self.highlight = crate::mdhl::highlight(&self.lines, theme.contains("light"));
            return;
        }
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
        if self.frozen() {
            return;
        }
        self.anchor = None;
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
        if self.frozen() {
            return;
        }
        self.anchor = None;
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
        self.anchor = None;
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
        // Scrolling drags the cursor along when it would leave the screen;
        // a selection mustn't silently grow with it.
        if self.cy < self.scroll_y {
            self.anchor = None;
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
        if self.frozen() {
            return;
        }
        self.delete_selection();
        self.checkpoint(1);
        let b = self.byte_at(self.cy, self.cx);
        self.lines[self.cy].insert(b, ch);
        self.cx += 1;
        self.want_col = None;
        self.touched();
    }

    pub fn insert_str(&mut self, s: &str) {
        if self.frozen() {
            return;
        }
        self.delete_selection();
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
        if self.selection().is_some() {
            return self.indent_lines(false);
        }
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
        if self.frozen() {
            return;
        }
        self.delete_selection();
        let md = self.is_markdown();
        if md && self.md_newline() {
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
        // "- In:" in a brief isn't a block opener.
        let opens = !md
            && self.lines[self.cy]
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

    /// No edits right now: a binary file, or one that's locked.
    pub fn frozen(&self) -> bool {
        self.readonly || self.locked
    }

    // ── selection ───────────────────────────────────────────────────────

    /// The selected range, start first, when anything is selected.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let a = (
            a.0.min(self.lines.len() - 1),
            a.1.min(self.line_len(a.0.min(self.lines.len() - 1))),
        );
        let c = (self.cy, self.cx);
        match a.cmp(&c) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((a, c)),
            std::cmp::Ordering::Greater => Some((c, a)),
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let ((y0, x0), (y1, x1)) = self.selection()?;
        let part = |y: usize, from: usize, to: usize| -> String {
            self.lines[y]
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect()
        };
        if y0 == y1 {
            return Some(part(y0, x0, x1));
        }
        let mut out = part(y0, x0, usize::MAX);
        for y in y0 + 1..y1 {
            out.push('\n');
            out.push_str(&self.lines[y]);
        }
        out.push('\n');
        out.push_str(&part(y1, 0, x1));
        Some(out)
    }

    /// Remove the selection and put the cursor where it began. False when
    /// nothing was selected.
    pub fn delete_selection(&mut self) -> bool {
        let Some(((y0, x0), (y1, x1))) = self.selection() else {
            self.anchor = None;
            return false;
        };
        if self.frozen() {
            return false;
        }
        self.checkpoint(6);
        self.last_edit = None;
        let (b0, b1) = (self.byte_at(y0, x0), self.byte_at(y1, x1));
        let tail = self.lines[y1][b1..].to_string();
        self.lines[y0].truncate(b0);
        self.lines[y0].push_str(&tail);
        self.lines.drain(y0 + 1..=y1);
        (self.cy, self.cx) = (y0, x0);
        self.anchor = None;
        self.want_col = None;
        self.touched();
        true
    }

    pub fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        self.cy = self.lines.len() - 1;
        self.cx = self.line_len(self.cy);
    }

    /// Indent, or with `out` dedent, every line the selection touches (or
    /// just this one), keeping the selection on the same text.
    pub fn indent_lines(&mut self, out: bool) {
        if self.frozen() {
            return;
        }
        let (y0, y1) = match self.selection() {
            Some(((a, _), (b, _))) => (a, b),
            None => (self.cy, self.cy),
        };
        self.checkpoint(7);
        self.last_edit = None;
        let unit = if self.uses_tabs {
            "\t".to_string()
        } else {
            " ".repeat(self.tab_width)
        };
        for y in y0..=y1 {
            let delta: isize = if out {
                let l = &self.lines[y];
                let n = if l.starts_with('\t') {
                    1
                } else {
                    l.chars()
                        .take(self.tab_width)
                        .take_while(|c| *c == ' ')
                        .count()
                };
                self.lines[y].drain(..n);
                -(n as isize)
            } else if self.lines[y].is_empty() {
                0
            } else {
                self.lines[y].insert_str(0, &unit);
                unit.chars().count() as isize
            };
            // A selection from the start of a line keeps the new indent in it.
            let shift = |x: usize| {
                if x == 0 && delta > 0 {
                    0
                } else {
                    (x as isize + delta).max(0) as usize
                }
            };
            if self.cy == y {
                self.cx = shift(self.cx);
            }
            if let Some((ay, ax)) = self.anchor.filter(|a| a.0 == y) {
                self.anchor = Some((ay, shift(ax)));
            }
        }
        self.want_col = None;
        self.touched();
    }

    // ── markdown ────────────────────────────────────────────────────────

    pub fn is_markdown(&self) -> bool {
        self.path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
    }

    /// Enter in a list item, task or quote starts the next one; Enter on an
    /// empty one ends the list. False when the line isn't one.
    fn md_newline(&mut self) -> bool {
        let Some(lead) = md_lead(&self.lines[self.cy]) else {
            return false;
        };
        if self.cx < self.lines[self.cy][..lead.len].chars().count() {
            return false;
        }
        self.checkpoint(2);
        if self.lines[self.cy][lead.len..].trim().is_empty() {
            self.lines[self.cy].clear();
            self.cx = 0;
        } else {
            let b = self.byte_at(self.cy, self.cx);
            let rest = self.lines[self.cy].split_off(b);
            self.cy += 1;
            self.cx = lead.next.chars().count();
            self.lines
                .insert(self.cy, format!("{}{}", lead.next, rest.trim_start()));
        }
        self.want_col = None;
        self.touched();
        true
    }

    /// Tick or untick the task on this line, making it a task first if it
    /// isn't one.
    pub fn toggle_task(&mut self) {
        if self.frozen() {
            return;
        }
        self.anchor = None;
        self.checkpoint(5);
        self.last_edit = None;
        let line = &mut self.lines[self.cy];
        let (at, insert) = match md_lead(line) {
            Some(l) if l.task.is_some() => {
                let mark = if l.task == Some(true) { " " } else { "x" };
                line.replace_range(l.marker_end + 1..l.marker_end + 2, mark);
                self.touched();
                return;
            }
            Some(l) => (l.marker_end, "[ ] "),
            None => (
                line.len() - line.trim_start_matches([' ', '\t']).len(),
                "- [ ] ",
            ),
        };
        line.insert_str(at, insert);
        if self.cx >= line[..at].chars().count() {
            self.cx += insert.len();
        }
        self.want_col = None;
        self.touched();
    }

    /// Wrap the word at the cursor in `mark` (`**`, `` ` ``), or unwrap it if
    /// it's wrapped already. With no word there, insert the pair and put the
    /// cursor between.
    pub fn wrap_word(&mut self, mark: &str) {
        if self.frozen() {
            return;
        }
        self.anchor = None;
        self.checkpoint(5);
        self.last_edit = None;
        let mut chars: Vec<char> = self.lines[self.cy].chars().collect();
        let m: Vec<char> = mark.chars().collect();
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let cx = self.cx.min(chars.len());
        let mut start = cx;
        while start > 0 && word(chars[start - 1]) {
            start -= 1;
        }
        let mut end = cx;
        while end < chars.len() && word(chars[end]) {
            end += 1;
        }
        let n = m.len();
        let wrapped = start < end
            && start >= n
            && chars[start - n..start] == m[..]
            && chars.get(end..end + n) == Some(&m[..]);
        if wrapped {
            chars.drain(end..end + n);
            chars.drain(start - n..start);
            self.cx = cx - n;
        } else {
            chars.splice(end..end, m.iter().copied());
            chars.splice(start..start, m.iter().copied());
            self.cx = cx + n;
        }
        self.lines[self.cy] = chars.into_iter().collect();
        self.want_col = None;
        self.touched();
    }

    pub fn backspace(&mut self) {
        if self.frozen() || self.delete_selection() {
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
        if self.frozen() || self.delete_selection() {
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
        if self.frozen() {
            return;
        }
        self.anchor = None;
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
        self.anchor = None;
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

    fn md(text: &str) -> Editor {
        // A file of its own per call: tests run in parallel.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("pitwall-md-{}-{n}.md", std::process::id()));
        std::fs::write(&p, text).unwrap();
        Editor::open(&p, 4).unwrap()
    }

    #[test]
    fn markdown_lists_continue_and_end_on_enter() {
        let mut e = md("  - [x] done");
        e.cx = e.line_len(0);
        e.newline();
        assert_eq!(e.lines[1], "  - [ ] ");
        e.newline();
        assert_eq!(e.lines, ["  - [x] done", ""]);
        let mut e = md("9. nine");
        e.cx = e.line_len(0);
        e.newline();
        assert_eq!(e.lines[1], "10. ");
        // A trailing colon doesn't indent the next line.
        let mut e = md("Scope:");
        e.cx = e.line_len(0);
        e.newline();
        assert_eq!(e.lines, ["Scope:", ""]);
        let mut e = md("**Ask:**");
        e.cx = e.line_len(0);
        e.newline();
        assert_eq!(e.lines, ["**Ask:**", ""]);
    }

    #[test]
    fn a_selection_is_copied_replaced_and_indented() {
        let mut e = md("alpha\nbeta\ngamma");
        e.anchor = Some((0, 2));
        (e.cy, e.cx) = (2, 3);
        assert_eq!(e.selected_text().as_deref(), Some("pha\nbeta\ngam"));
        e.insert_char('X');
        assert_eq!(e.lines, ["alXma"]);
        assert_eq!((e.cy, e.cx, e.anchor), (0, 3, None));
        e.undo();
        e.undo();
        assert_eq!(e.lines, ["alpha", "beta", "gamma"]);

        e.anchor = Some((0, 0));
        (e.cy, e.cx) = (1, 2);
        e.tab();
        assert_eq!(e.lines, ["    alpha", "    beta", "gamma"]);
        assert_eq!(e.selected_text().as_deref(), Some("    alpha\n    be"));
        e.indent_lines(true);
        assert_eq!(e.lines, ["alpha", "beta", "gamma"]);
        e.backspace();
        assert_eq!(e.lines, ["ta", "gamma"]);
        e.select_all();
        assert_eq!(e.selected_text().as_deref(), Some("ta\ngamma"));
    }

    #[test]
    fn a_locked_buffer_stays_locked_through_reloads_and_undo() {
        let mut e = md("my locked brief text");
        e.locked = true;
        std::fs::write(&e.path, "the agent's").unwrap();
        e.reload();
        assert!(e.locked, "a reload unlocked it");
        e.insert_char('x');
        e.undo();
        assert_eq!(e.lines, ["the agent's"]);
        assert!(!e.dirty);
        e.locked = false;
        e.undo();
        assert_eq!(e.lines, ["my locked brief text"]);
    }

    #[test]
    fn saving_over_someone_elses_change_takes_two_tries() {
        let mut e = md("stale save test text");
        e.insert_char('x');
        std::fs::write(&e.path, "the agent wrote this").unwrap();
        // A different mtime, as a real write later on would have.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&e.path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!e.check_disk());
        assert!(e.stale);
        assert!(e.save().is_err());
        assert_eq!(
            std::fs::read_to_string(&e.path).unwrap(),
            "the agent wrote this"
        );
        e.save().unwrap();
        assert!(std::fs::read_to_string(&e.path).unwrap().starts_with('x'));
    }

    #[test]
    fn a_binary_file_rewritten_as_text_can_be_edited() {
        let mut e = md("placeholder for a binary");
        std::fs::write(&e.path, b"\0\x01binary").unwrap();
        e.reload();
        assert!(e.readonly);
        std::fs::write(&e.path, "text now").unwrap();
        e.reload();
        assert!(!e.frozen());
    }

    #[test]
    fn moving_away_drops_the_selection() {
        let mut e = md("one two\nthree");
        e.anchor = Some((0, 0));
        e.cx = 3;
        e.find_next("three");
        assert_eq!((e.anchor, e.cy), (None, 1));
        e.anchor = Some((1, 0));
        e.goto(1);
        assert_eq!(e.anchor, None);
    }

    #[test]
    fn a_reload_from_disk_can_be_undone() {
        let mut e = md("mine");
        std::fs::write(&e.path, "the agent's").unwrap();
        e.reload();
        assert_eq!(e.lines, ["the agent's"]);
        e.undo();
        assert_eq!(e.lines, ["mine"]);
        assert!(e.dirty);
    }

    #[test]
    fn markdown_tasks_toggle_and_words_wrap() {
        let mut e = md("buy milk");
        e.toggle_task();
        assert_eq!(e.lines[0], "- [ ] buy milk");
        e.toggle_task();
        assert_eq!(e.lines[0], "- [x] buy milk");
        e.toggle_task();
        assert_eq!(e.lines[0], "- [ ] buy milk");
        let mut e = md("say hello now");
        e.cx = 6;
        e.wrap_word("**");
        assert_eq!(e.lines[0], "say **hello** now");
        e.wrap_word("**");
        assert_eq!(e.lines[0], "say hello now");
        assert_eq!(e.cx, 6);
    }

    fn ed(text: &str) -> Editor {
        let p = std::env::temp_dir().join(format!(
            "pitwall-ed-{}-{}.rs",
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
