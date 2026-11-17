//! Terminal view: a vt100 grid painted with egui, plus keyboard and mouse
//! encoding. Shared with keel (its terminal.rs); the daemon owns the PTY here,
//! so this only draws what it's fed and hands back what to send.

use crate::i18n::{tr, trf};
use crate::settings::{CursorStyle, Settings};
use crate::theme::Scheme;
use egui::{pos2, vec2, Align2, Color32, Event, FontId, Key, Modifiers, Rect, Sense, Stroke};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Callbacks {
    pub replies: Vec<u8>,
    /// The keyboard flags the program asked for (the daemon answers its
    /// queries; this side only needs to know how to send keys).
    pub keys: pitwall_core::kitty::Keys,
    pub title: Option<String>,
    pub bell: bool,
    /// Desktop-notification escapes (OSC 9 / 777 / 99) not yet shown.
    pub notifications: Vec<String>,
    /// Lines dropped off the top of a full scrollback. vt100's on_scroll only
    /// reports those, not every line scrolling in; plus the scrollback's length
    /// it gives the total scrolled, see `scrolled`.
    pub dropped: u64,
    /// The folder the shell reported with OSC 7.
    pub cwd: Option<String>,
    /// Exit statuses from OSC 133;D, oldest first.
    pub exits: Vec<i32>,
    /// Text a program asked to put on the clipboard (OSC 52: nvim, tmux,
    /// anything over ssh).
    pub clipboard: Option<String>,
}

impl vt100::Callbacks for Callbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        // vt100 hands over the base64 as sent. A megabyte is plenty.
        if data.len() <= 1 << 20 {
            if let Some(t) = base64_decode(data).and_then(|b| String::from_utf8(b).ok()) {
                self.clipboard = Some(t);
            }
        }
    }
    fn on_scroll(&mut self, _: vt100::capture::RowContents<'_>, alternate_screen: bool) {
        if !alternate_screen {
            self.dropped += 1;
        }
    }
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"7", url, ..] => self.cwd = cwd_from_osc7(&String::from_utf8_lossy(url)),
            [b"133", kind, rest @ ..] if kind.starts_with(b"D") => {
                if let Some(code) = rest
                    .first()
                    .and_then(|c| String::from_utf8_lossy(c).trim().parse().ok())
                {
                    if self.exits.len() < 64 {
                        self.exits.push(code);
                    }
                }
            }
            _ => {}
        }
        if let Some(n) = notification_from_osc(params) {
            // A runaway program shouldn't grow this without bound.
            if self.notifications.len() < 16 {
                self.notifications.push(n);
            }
        }
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).to_string());
    }
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (c, i1) {
            // Device status report / cursor position report
            ('n', None) if first == 6 => {
                let (r, col) = screen.cursor_position();
                self.replies
                    .extend_from_slice(format!("\x1b[{};{}R", r + 1, col + 1).as_bytes());
            }
            ('n', None) if first == 5 => self.replies.extend_from_slice(b"\x1b[0n"),
            // Primary / secondary device attributes
            ('c', None) if first == 0 => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            ('c', Some(b'>')) => self.replies.extend_from_slice(b"\x1b[>0;10;1c"),
            ('u', marker) => {
                let _ = self.keys.csi(marker, params);
            }
            _ => {}
        }
    }
}

fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s.iter().filter(|c| !matches!(c, b'=' | b'\n' | b'\r')) {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

pub type Parser = vt100::Parser<Callbacks>;

/// Lines ever scrolled off the top of the main screen. With a row on screen it
/// gives a line number that stays put as more output scrolls in.
fn lines_scrolled(p: &Parser) -> u64 {
    p.callbacks().dropped + p.screen().scrollback_len() as u64
}

/// The history line shown on the screen's top row right now.
fn top_line(p: &Parser) -> u64 {
    lines_scrolled(p).saturating_sub(p.screen().scrollback() as u64)
}
pub type SharedParser = Arc<Mutex<Parser>>;

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap()
}

pub fn new_parser(rows: u16, cols: u16, scrollback: usize) -> SharedParser {
    Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
        nz(rows),
        nz(cols),
        scrollback,
        Callbacks::default(),
    )))
}

/// Feed bytes into the emulator, returning any replies the terminal must send back.
pub fn feed(p: &SharedParser, data: &[u8]) -> Vec<u8> {
    let mut g = p.lock().unwrap();
    g.process(data);
    std::mem::take(&mut g.callbacks_mut().replies)
}

pub enum TermAction {
    /// Mouse reports and wheel-as-arrows. Cell coordinates are pane-specific, so these never broadcast.
    Input(Vec<u8>),
    /// Keyboard input.
    Typed(Vec<u8>),
    /// Each receiving session wraps this in its own bracketed-paste mode.
    Paste(String),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// ⌘-click on a file path in the output: open it in the editor.
    OpenFile(PathBuf, Option<usize>),
    /// Selected text for the clipboard. The app copies it, so it can say so.
    Copy(String),
}

pub struct TerminalView {
    pub parser: SharedParser,
    pub id: egui::Id,
    pub cols: u16,
    pub rows: u16,
    /// Selection ends as (line in the whole history, column), so the
    /// selection stays on its text as output scrolls in or you scroll back.
    sel_anchor: Option<(u64, u16)>,
    sel_end: Option<(u64, u16)>,
    wheel_acc: f32,
    mouse_down_btn: Option<u8>,
    last_mouse_cell: Option<(u16, u16)>,
    pub want_focus: bool,
    pub bell_flash: f64,
    /// A local shell: paths in its output open on this machine.
    pub local: bool,
    find: Option<FindState>,
    /// Where each command was entered, for jumping between commands and
    /// copying one's output.
    marks: Vec<Mark>,
    /// Text an input method is composing, shown at the cursor until committed.
    preedit: String,
}

#[derive(Clone, Copy, Debug)]
struct Mark {
    /// Line number in `Callbacks::scrolled` terms.
    line: u64,
    /// From OSC 133;D, when the shell reports it.
    exit: Option<i32>,
}

#[derive(Default)]
struct FindState {
    query: String,
    /// The query `matches` were found for.
    searched: String,
    matches: Vec<crate::find::Match>,
    current: usize,
    want_focus: bool,
}

impl TerminalView {
    pub fn new(parser: SharedParser, id: egui::Id, cols: u16, rows: u16) -> Self {
        Self {
            parser,
            id,
            cols,
            rows,
            sel_anchor: None,
            sel_end: None,
            preedit: String::new(),
            wheel_acc: 0.0,
            mouse_down_btn: None,
            last_mouse_cell: None,
            want_focus: true,
            bell_flash: 0.0,
            local: false,
            find: None,
            marks: vec![],
        }
    }

    /// Notes the cursor's line as a command being entered (Enter at a shell).
    fn mark_command(&mut self) {
        let p = self.parser.lock().unwrap();
        let line = lines_scrolled(&p) + p.screen().cursor_position().0 as u64;
        drop(p);
        if self.marks.last().is_none_or(|m| m.line != line) {
            self.marks.push(Mark { line, exit: None });
            if self.marks.len() > 2000 {
                self.marks.remove(0);
            }
        }
    }

    /// Exit statuses arrive after the command; each belongs to the oldest mark
    /// still without one.
    fn absorb_exits(&mut self) {
        let exits = std::mem::take(&mut self.parser.lock().unwrap().callbacks_mut().exits);
        for code in exits {
            if let Some(m) = self
                .marks
                .iter_mut()
                .rev()
                .take_while(|m| m.exit.is_none())
                .last()
            {
                m.exit = Some(code);
            }
        }
    }

    /// Scrolls to put the previous / next command at the top of the view.
    fn jump_mark(&mut self, up: bool) {
        let mut p = self.parser.lock().unwrap();
        let scrolled = lines_scrolled(&p);
        let sb_len = p.screen().scrollback_len() as u64;
        let top = scrolled - p.screen().scrollback() as u64;
        let oldest = scrolled - sb_len;
        let target = if up {
            self.marks
                .iter()
                .rev()
                .map(|m| m.line)
                .find(|&l| l < top && l >= oldest)
        } else {
            self.marks.iter().map(|m| m.line).find(|&l| l > top)
        };
        let offset = match target {
            Some(l) => scrolled.saturating_sub(l).min(sb_len),
            // past the last command: back to the live screen
            None if !up => 0,
            None => return,
        };
        p.screen_mut().set_scrollback(offset as usize);
    }

    /// Output of the last finished command: from the line after its mark to
    /// the line before the next one (or the prompt the cursor is on).
    pub fn last_output(&self) -> Option<String> {
        let (start, end) = {
            let p = self.parser.lock().unwrap();
            let cur = lines_scrolled(&p) + p.screen().cursor_position().0 as u64;
            let mut it = self.marks.iter().rev().filter(|m| m.line < cur);
            let last = it.next()?;
            // When the newest mark is the prompt being typed at, take the one before.
            match self.marks.last() {
                Some(m) if m.line == last.line && cur == last.line => {
                    (it.next()?.line + 1, last.line)
                }
                _ => (last.line + 1, cur),
            }
        };
        let lines = self.all_lines();
        let p = self.parser.lock().unwrap();
        let first = lines_scrolled(&p) - p.screen().scrollback_len() as u64;
        drop(p);
        let idx = |l: u64| l.checked_sub(first).map(|i| i as usize);
        let (a, b) = (idx(start)?, idx(end)?);
        let text: Vec<String> = lines
            .get(a..b.min(lines.len()))?
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        let text = text.join("\n").trim_matches('\n').to_string();
        (!text.is_empty()).then_some(text)
    }

    /// Row `r` of the visible screen, with the column each character is in.
    fn row_line(screen: &vt100::Screen, r: u16, cols: u16) -> crate::find::Line {
        let mut l = crate::find::Line::default();
        for c in 0..cols {
            let Some(cell) = screen.cell(r, c) else { break };
            if cell.is_wide_continuation() {
                continue;
            }
            let t = if cell.has_contents() {
                cell.contents()
            } else {
                " "
            };
            for ch in t.chars().take(1) {
                l.text.push(ch);
                l.cols.push(c);
            }
        }
        l
    }

    /// Every line, scrollback included, oldest first. Walks the scrollback a
    /// screenful at a time and puts the view back where it was.
    fn all_lines(&self) -> Vec<crate::find::Line> {
        let mut p = self.parser.lock().unwrap();
        let (rows, cols) = p.screen().size();
        let (rows, cols) = (rows.get(), cols.get());
        let back = p.screen().scrollback();
        let total = p.screen().scrollback_len();
        let mut lines = vec![crate::find::Line::default(); total + rows as usize];
        let mut offset = total;
        loop {
            p.screen_mut().set_scrollback(offset);
            let s = p.screen();
            for r in 0..rows {
                lines[total - offset + r as usize] = Self::row_line(s, r, cols);
            }
            if offset == 0 {
                break;
            }
            offset = offset.saturating_sub(rows as usize);
        }
        p.screen_mut().set_scrollback(back);
        lines
    }

    pub fn open_find(&mut self) {
        let f = self.find.get_or_insert_with(Default::default);
        f.want_focus = true;
        // Selected text is the likely search.
        if let Some(t) = self
            .selection_text()
            .filter(|t| !t.contains('\n') && !t.trim().is_empty())
        {
            if let Some(f) = &mut self.find {
                f.query = t.trim().to_string();
            }
        }
    }

    fn run_find(&mut self) {
        let lines = self.all_lines();
        if let Some(f) = &mut self.find {
            f.matches = crate::find::find(&lines, &f.query);
            f.searched = f.query.clone();
            // Start from the newest match, nearest the prompt.
            f.current = f.matches.len().saturating_sub(1);
        }
        self.show_match();
    }

    fn step_find(&mut self, forward: bool) {
        if let Some(f) = &mut self.find {
            if f.query != f.searched {
                self.run_find();
                return;
            }
            let n = f.matches.len();
            if n > 0 {
                f.current = if forward {
                    (f.current + 1) % n
                } else {
                    (f.current + n - 1) % n
                };
            }
        }
        self.show_match();
    }

    /// Scrolls so the current match sits mid-screen.
    fn show_match(&mut self) {
        let Some(m) = self
            .find
            .as_ref()
            .and_then(|f| f.matches.get(f.current))
            .copied()
        else {
            return;
        };
        let mut p = self.parser.lock().unwrap();
        let total = p.screen().scrollback_len();
        let rows = p.screen().size().0.get() as usize;
        let offset = (total + rows / 2).saturating_sub(m.line).min(total);
        p.screen_mut().set_scrollback(offset);
    }

    pub fn selection_text(&self) -> Option<String> {
        let (a, b) = self.sel_range()?;
        let mut p = self.parser.lock().unwrap();
        // Lines off screen are read by scrolling the history to them and
        // back; vt100 only hands out cells of what's in view.
        let was = p.screen().scrollback();
        let total = lines_scrolled(&p);
        let rows = p.screen().size().0.get() as u64;
        let kept = p.screen().scrollback_len() as u64;
        let mut out = String::new();
        for l in a.0..=b.0 {
            let (back, r) = if l >= total {
                (0, l - total)
            } else {
                (total - l, 0)
            };
            if back > kept || r >= rows {
                continue;
            }
            p.screen_mut().set_scrollback(back as usize);
            let s = p.screen();
            let r = r as u16;
            let c0 = if l == a.0 { a.1 } else { 0 };
            let c1 = if l == b.0 {
                b.1
            } else {
                self.cols.saturating_sub(1)
            };
            let mut line = String::new();
            for c in c0..=c1 {
                if let Some(cell) = s.cell(r, c) {
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    if cell.has_contents() {
                        line.push_str(cell.contents());
                    } else {
                        line.push(' ');
                    }
                }
            }
            let wrapped = s.row_wrapped(r);
            if !wrapped || l == b.0 {
                out.push_str(line.trim_end());
            } else {
                out.push_str(&line);
            }
            if l != b.0 && !wrapped {
                out.push('\n');
            }
        }
        p.screen_mut().set_scrollback(was);
        Some(out)
    }

    /// A screen cell as a position in the whole history.
    fn line_of(&self, (r, c): (u16, u16)) -> (u64, u16) {
        (top_line(&self.parser.lock().unwrap()) + r as u64, c)
    }

    fn sel_range(&self) -> Option<((u64, u16), (u64, u16))> {
        let (a, b) = (self.sel_anchor?, self.sel_end?);
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    fn in_sel(range: Option<((u64, u16), (u64, u16))>, r: u64, c: u16) -> bool {
        match range {
            Some((a, b)) => (r, c) >= a && (r, c) <= b,
            None => false,
        }
    }

    pub fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_end = None;
    }

    /// The whole history, not just what's on screen.
    pub fn select_all(&mut self) {
        let p = self.parser.lock().unwrap();
        let first = p.callbacks().dropped;
        let last = lines_scrolled(&p) + self.rows.saturating_sub(1) as u64;
        drop(p);
        self.sel_anchor = Some((first, 0));
        self.sel_end = Some((last, self.cols.saturating_sub(1)));
    }

    pub fn paste_bytes(&self, text: &str) -> Vec<u8> {
        let bracketed = self.parser.lock().unwrap().screen().bracketed_paste();
        // An end-of-paste marker inside the text would end the paste early
        // and run the rest as typed input. Taking out pieces of one can leave
        // another, so no ESC goes through at all in a bracketed paste.
        let text = if bracketed {
            text.replace('\x1b', "")
        } else {
            text.to_string()
        };
        let text = text.replace("\r\n", "\n").replace('\n', "\r");
        let mut out = Vec::new();
        if bracketed {
            out.extend_from_slice(b"\x1b[200~");
        }
        out.extend_from_slice(text.as_bytes());
        if bracketed {
            out.extend_from_slice(b"\x1b[201~");
        }
        out
    }

    /// A new screen from the daemon: what pointed into the old one (command
    /// marks, find matches, a selection) no longer lines up.
    pub fn replace_parser(&mut self, parser: SharedParser) {
        self.parser = parser;
        self.marks.clear();
        self.sel_anchor = None;
        self.sel_end = None;
        if let Some(f) = self.find.as_mut() {
            f.matches.clear();
            f.searched.clear();
        }
    }

    pub fn clear_scrollback(&mut self) {
        let mut p = self.parser.lock().unwrap();
        let alt = p.screen().alternate_screen();
        p.screen_mut().clear_scrollback();
        // The lines the marks and the selection pointed at are gone, and the
        // count restarts. The alternate screen has a history of its own; the
        // main screen's numbering is left alone then.
        if !alt {
            p.callbacks_mut().dropped = 0;
        }
        drop(p);
        self.marks.clear();
        self.clear_selection();
    }

    /// Draw the terminal and handle its input. Returns bytes to send / resizes.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        settings: &Settings,
        scheme: &Scheme,
        interactive: bool,
    ) -> (Vec<TermAction>, egui::Response) {
        let mut actions = Vec::new();
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let resp = ui.interact(rect, self.id, Sense::click_and_drag());

        let font = crate::theme::term_font(settings.font_size);
        let bold_font = crate::theme::term_bold_font(settings.font_size);
        let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, 'M')).max(1.0);
        // A row is the font's own line height, not its point size: glyphs of a
        // 21pt font are ~25px tall, and a 21px row cut them off at the edges.
        let text_h = ui.ctx().fonts_mut(|f| f.row_height(&font));
        let ch = (text_h * settings.line_height).round().max(1.0);
        let text_dy = ((ch - text_h) / 2.0).max(0.0);
        let pad = 10.0;
        let inner = rect.shrink(pad);
        let cols = ((inner.width() / cw).floor() as i32).clamp(2, 1000) as u16;
        let rows = ((inner.height() / ch).floor() as i32).clamp(1, 500) as u16;
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.parser
                .lock()
                .unwrap()
                .screen_mut()
                .set_size(nz(rows), nz(cols));
            self.clear_selection();
            actions.push(TermAction::Resize { cols, rows });
        }
        let origin = inner.min;
        let cell_at = |p: egui::Pos2| -> (u16, u16) {
            let c = ((p.x - origin.x) / cw)
                .floor()
                .clamp(0.0, (cols - 1) as f32) as u16;
            let r = ((p.y - origin.y) / ch)
                .floor()
                .clamp(0.0, (rows - 1) as f32) as u16;
            (r, c)
        };

        if interactive
            && (resp.clicked()
                || resp.drag_started()
                || resp.secondary_clicked()
                || self.want_focus)
        {
            resp.request_focus();
            self.want_focus = false;
        }
        let focused = resp.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        // Snapshot the modes we need for input encoding.
        let (app_cursor, mouse_mode, mouse_enc, alt_screen, kitty) = {
            let p = self.parser.lock().unwrap();
            let s = p.screen();
            (
                s.application_cursor(),
                s.mouse_protocol_mode(),
                s.mouse_protocol_encoding(),
                s.alternate_screen(),
                p.callbacks().keys.disambiguate(),
            )
        };
        let mods_now = ui.input(|i| i.modifiers);
        // ⌘-click (Ctrl-click elsewhere) opens links, even in apps that take the mouse.
        let link_mod = if cfg!(target_os = "macos") {
            mods_now.mac_cmd
        } else {
            mods_now.ctrl
        };
        let mouse_reporting =
            mouse_mode != vt100::MouseProtocolMode::None && !mods_now.shift && !link_mod;

        // ---------------- keyboard ----------------
        let mut find_requested = false;
        let mut mark_jump = None;
        let mut enter_pressed = false;
        if !focused {
            // A composition doesn't survive losing focus.
            self.preedit.clear();
        }
        if focused && interactive {
            let events = ui.input(|i| i.events.clone());
            let mut skip_text = false;
            // Option+key as Meta, held until we see what the layout typed:
            // an ASCII symbol (`@`, `{`) is what the user wanted, anything
            // else (`å`, `≥`) means Meta.
            let mut pending_meta: Option<Vec<u8>> = None;
            for ev in events {
                match ev {
                    Event::Text(t) => {
                        if let Some(meta) = pending_meta.take() {
                            let symbol = t.chars().count() == 1
                                && t.chars()
                                    .all(|c| c.is_ascii_graphic() && !c.is_ascii_alphanumeric());
                            self.input_typed(
                                &mut actions,
                                if symbol { t.into_bytes() } else { meta },
                            );
                            continue;
                        }
                        if skip_text {
                            skip_text = false;
                            continue;
                        }
                        self.input_typed(&mut actions, t.into_bytes());
                    }
                    Event::Ime(egui::ImeEvent::Commit(t)) => {
                        self.preedit.clear();
                        self.input_typed(&mut actions, t.into_bytes())
                    }
                    Event::Ime(egui::ImeEvent::Preedit { text, .. }) => self.preedit = text,
                    Event::Paste(t) => {
                        if cfg!(target_os = "macos") || mods_now.shift {
                            self.back_to_live();
                            actions.push(TermAction::Paste(t));
                        } else {
                            self.input_typed(&mut actions, vec![0x16]);
                        }
                    }
                    // ⌘V with only an image on the clipboard. A terminal can't
                    // carry the image itself; Claude Code and Codex read it off
                    // the clipboard when they get Ctrl+V, so that's what goes.
                    Event::PasteImage(_) => self.input_typed(&mut actions, vec![0x16]),
                    Event::Copy => {
                        if cfg!(target_os = "macos") || mods_now.shift {
                            if let Some(t) = self.selection_text() {
                                actions.push(TermAction::Copy(t));
                            }
                        } else {
                            self.input_typed(&mut actions, vec![0x03]);
                        }
                    }
                    Event::Cut => {
                        if !cfg!(target_os = "macos") {
                            self.input_typed(&mut actions, vec![0x18]);
                        }
                    }
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if key == Key::F
                            && (modifiers.mac_cmd
                                || (!cfg!(target_os = "macos")
                                    && modifiers.ctrl
                                    && modifiers.shift))
                        {
                            find_requested = true;
                            continue;
                        }
                        // ⌘↑/⌘↓ (Ctrl+Shift+↑/↓ elsewhere): previous / next command.
                        let jump_mod = if cfg!(target_os = "macos") {
                            modifiers.mac_cmd
                        } else {
                            modifiers.ctrl && modifiers.shift
                        };
                        if jump_mod && matches!(key, Key::ArrowUp | Key::ArrowDown) {
                            mark_jump = Some(key == Key::ArrowUp);
                            continue;
                        }
                        if key == Key::Enter && modifiers.is_none() && !alt_screen {
                            enter_pressed = true;
                        }
                        if modifiers.mac_cmd {
                            continue;
                        }
                        // AltGr on Windows arrives as Ctrl+Alt; the produced character comes as Text.
                        if cfg!(windows)
                            && modifiers.ctrl
                            && modifiers.alt
                            && is_letter_or_digit(key)
                        {
                            continue;
                        }
                        // Shift+PageUp/PageDown scroll the scrollback locally.
                        if modifiers.shift
                            && !alt_screen
                            && matches!(key, Key::PageUp | Key::PageDown)
                        {
                            let mut p = self.parser.lock().unwrap();
                            let cur = p.screen().scrollback() as i64;
                            let delta = if key == Key::PageUp {
                                rows as i64
                            } else {
                                -(rows as i64)
                            };
                            p.screen_mut().set_scrollback((cur + delta).max(0) as usize);
                            continue;
                        }
                        // Linux: Ctrl+Shift+C / V copy and paste (egui only
                        // turns plain Ctrl+C / V into copy and paste events).
                        if !cfg!(target_os = "macos") && modifiers.ctrl && modifiers.shift {
                            if key == Key::C {
                                if let Some(t) = self.selection_text() {
                                    actions.push(TermAction::Copy(t));
                                }
                                continue;
                            }
                            if key == Key::V {
                                if let Ok(mut c) = arboard::Clipboard::new() {
                                    if let Ok(t) = c.get_text() {
                                        self.back_to_live();
                                        actions.push(TermAction::Paste(t));
                                    } else if c.get_image().is_ok() {
                                        self.input_typed(&mut actions, vec![0x16]);
                                    }
                                }
                                continue;
                            }
                        }
                        let mac_option = cfg!(target_os = "macos")
                            && modifiers.alt
                            && !modifiers.ctrl
                            && !modifiers.command;
                        // Option+←/→ jump words, as readline and zsh read
                        // them; full-screen programs get the arrow as is.
                        if mac_option && !modifiers.shift && !alt_screen {
                            match key {
                                Key::ArrowLeft => {
                                    self.input_typed(&mut actions, b"\x1bb".to_vec());
                                    continue;
                                }
                                Key::ArrowRight => {
                                    self.input_typed(&mut actions, b"\x1bf".to_vec());
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        if mac_option {
                            let ch = if is_letter_or_digit(key) {
                                key.name().chars().next()
                            } else {
                                Some(key.symbol_or_name())
                                    .filter(|s| s.len() == 1 && s.is_ascii())
                                    .and_then(|s| s.chars().next())
                            };
                            if let Some(c) = ch {
                                if settings.option_meta {
                                    let c = if modifiers.shift {
                                        c.to_ascii_uppercase()
                                    } else {
                                        c.to_ascii_lowercase()
                                    };
                                    pending_meta = Some(vec![0x1b, c as u8]);
                                }
                                // Off: the layout's character arrives as text.
                                continue;
                            }
                        }
                        // Shift+Return: its own key for a program that asked for
                        // distinct keys (Codex, Claude Code, full screen or
                        // not); plain Return in other full-screen programs
                        // (vim, less); a newline (Esc+CR) at a line editor.
                        if key == Key::Enter && modifiers.shift && !modifiers.ctrl {
                            let b: &[u8] = if kitty {
                                b"\x1b[13;2u"
                            } else if alt_screen {
                                b"\r"
                            } else {
                                b"\x1b\r"
                            };
                            self.input_typed(&mut actions, b.to_vec());
                            continue;
                        }
                        if let Some(b) = encode_key(key, modifiers, app_cursor) {
                            if modifiers.alt && !modifiers.ctrl && is_letter_or_digit(key) {
                                skip_text = true;
                            }
                            self.input_typed(&mut actions, b);
                        }
                    }
                    _ => {}
                }
            }
            // Option+key with no text after it: Meta.
            if let Some(meta) = pending_meta {
                self.input_typed(&mut actions, meta);
            }
        }

        // ---------------- mouse ----------------
        let pointer = ui.input(|i| i.pointer.clone());
        let hover_pos = resp.hover_pos();
        if let Some(pos) = hover_pos {
            let scroll_lines = ui.input(|i| {
                let mut d = 0.0;
                for e in &i.events {
                    if let Event::MouseWheel { unit, delta, .. } = e {
                        d += match unit {
                            egui::MouseWheelUnit::Point => delta.y / ch,
                            egui::MouseWheelUnit::Line => delta.y,
                            egui::MouseWheelUnit::Page => delta.y * rows as f32,
                        };
                    }
                }
                d
            });
            if scroll_lines != 0.0 {
                self.wheel_acc += scroll_lines;
                let n = self.wheel_acc.trunc() as i32;
                self.wheel_acc -= n as f32;
                if n != 0 {
                    let cell = cell_at(pos);
                    if mouse_reporting {
                        let btn = if n > 0 { 64 } else { 65 };
                        for _ in 0..n.abs() {
                            actions.push(TermAction::Input(mouse_seq(mouse_enc, btn, cell, true)));
                        }
                    } else if alt_screen {
                        let seq: &[u8] = match (n > 0, app_cursor) {
                            (true, true) => b"\x1bOA",
                            (true, false) => b"\x1b[A",
                            (false, true) => b"\x1bOB",
                            (false, false) => b"\x1b[B",
                        };
                        for _ in 0..(n.abs() * 3) {
                            actions.push(TermAction::Input(seq.to_vec()));
                        }
                    } else {
                        let mut p = self.parser.lock().unwrap();
                        let cur = p.screen().scrollback() as i64;
                        p.screen_mut()
                            .set_scrollback((cur + (n as i64) * 3).max(0) as usize);
                    }
                }
            }
        }

        if mouse_reporting && interactive {
            // Alt and Ctrl held with a click, as xterm reports them (Shift
            // is pitwall's own: it bypasses the program for local selection).
            let mb = 8 * mods_now.alt as u8 + 16 * mods_now.ctrl as u8;
            if let Some(pos) = pointer
                .interact_pos()
                .filter(|p| rect.contains(*p) || self.mouse_down_btn.is_some())
            {
                let cell = cell_at(pos);
                for (btn, eb) in [
                    (egui::PointerButton::Primary, 0u8),
                    (egui::PointerButton::Middle, 1),
                    (egui::PointerButton::Secondary, 2),
                ] {
                    if pointer.button_pressed(btn) && rect.contains(pos) {
                        self.mouse_down_btn = Some(eb);
                        actions.push(TermAction::Input(mouse_seq(mouse_enc, eb + mb, cell, true)));
                    }
                    if pointer.button_released(btn) && self.mouse_down_btn == Some(eb) {
                        self.mouse_down_btn = None;
                        let code = if mouse_enc == vt100::MouseProtocolEncoding::Sgr {
                            eb
                        } else {
                            3
                        };
                        actions.push(TermAction::Input(mouse_seq(
                            mouse_enc,
                            code + mb,
                            cell,
                            false,
                        )));
                    }
                }
                let motion = matches!(
                    mouse_mode,
                    vt100::MouseProtocolMode::ButtonMotion | vt100::MouseProtocolMode::AnyMotion
                );
                if motion && self.last_mouse_cell != Some(cell) {
                    if let Some(b) = self.mouse_down_btn {
                        actions.push(TermAction::Input(mouse_seq(
                            mouse_enc,
                            b + 32 + mb,
                            cell,
                            true,
                        )));
                    } else if mouse_mode == vt100::MouseProtocolMode::AnyMotion
                        && rect.contains(pos)
                    {
                        actions.push(TermAction::Input(mouse_seq(mouse_enc, 35 + mb, cell, true)));
                    }
                }
                self.last_mouse_cell = Some(cell);
            }
        } else {
            // Local selection
            if resp.drag_started_by(egui::PointerButton::Primary) {
                if let Some(p) = resp.interact_pointer_pos() {
                    let c = self.line_of(cell_at(p));
                    self.sel_anchor = Some(c);
                    self.sel_end = Some(c);
                }
            }
            if resp.dragged_by(egui::PointerButton::Primary) {
                if let Some(p) = resp.interact_pointer_pos() {
                    // Dragging past the top or bottom scrolls, so a selection
                    // can run longer than a screen.
                    let step = if p.y < rect.top() {
                        1
                    } else if p.y > rect.bottom() {
                        -1
                    } else {
                        0
                    };
                    if step != 0 {
                        let mut g = self.parser.lock().unwrap();
                        let at = g.screen().scrollback() as i64;
                        g.screen_mut().set_scrollback((at + step).max(0) as usize);
                        drop(g);
                        ui.ctx().request_repaint();
                    }
                    self.sel_end = Some(self.line_of(cell_at(p)));
                }
            }
            if resp.drag_stopped() && settings.copy_on_select {
                if let Some(t) = self.selection_text() {
                    actions.push(TermAction::Copy(t));
                }
            }
            if resp.double_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.select_word(cell_at(p));
                }
            } else if resp.clicked() {
                self.clear_selection();
            }
            if resp.triple_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (l, _) = self.line_of(cell_at(p));
                    self.sel_anchor = Some((l, 0));
                    self.sel_end = Some((l, cols - 1));
                }
            }
        }

        if find_requested {
            self.open_find();
        }
        if enter_pressed {
            self.mark_command();
        }
        if let Some(up) = mark_jump {
            self.jump_mark(up);
        }
        self.absorb_exits();

        // The link under the pointer while the link modifier is held.
        let mut hover_link: Option<(u16, u16, u16, crate::find::Link)> = None;
        if link_mod {
            if let Some(pos) = resp.hover_pos() {
                let (r, c) = cell_at(pos);
                let line = Self::row_line(self.parser.lock().unwrap().screen(), r, cols);
                if let Some((link, c0, c1)) = crate::find::link_at(&line, c) {
                    // Paths only mean something for a shell on this machine.
                    let usable = match &link {
                        crate::find::Link::Url(_) => true,
                        crate::find::Link::Path(p) => self.local && local_path(p).is_some(),
                    };
                    if usable {
                        hover_link = Some((r, c0, c1, link));
                    }
                }
            }
        }
        if let Some((_, _, _, link)) = &hover_link {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                match link {
                    crate::find::Link::Url(u) => ui.ctx().open_url(egui::OpenUrl::new_tab(u)),
                    crate::find::Link::Path(p) => {
                        if let Some(path) = local_path(p) {
                            // Files go to pitwall's editor, at the line compiler output names.
                            if path.is_file() {
                                actions.push(TermAction::OpenFile(path, line_of(p)));
                            } else {
                                open_on_system(&path);
                            }
                        }
                    }
                }
            }
        }

        // ---------------- paint ----------------
        let painter = ui.painter_at(rect);
        // Only the default background fades; cells with their own colour stay solid.
        painter.rect_filled(rect, 0.0, crate::theme::term_bg(scheme.bg));
        let time = ui.input(|i| i.time);
        let p = self.parser.lock().unwrap();
        let screen = p.screen();
        let bell = p.callbacks().bell;
        let sel = self.sel_range();
        let top = top_line(&p);
        let scrolled = screen.scrollback() > 0;

        for r in 0..rows {
            let y = origin.y + r as f32 * ch;
            // background runs
            let mut c = 0u16;
            while c < cols {
                let Some(cell) = screen.cell(r, c) else { break };
                let (_, bg) = cell_colors(cell, scheme, settings.bold_is_bright);
                let bg = if Self::in_sel(sel, top + r as u64, c) {
                    scheme.selection
                } else {
                    bg
                };
                let start = c;
                c += 1;
                while c < cols {
                    let Some(n) = screen.cell(r, c) else { break };
                    let (_, nbg) = cell_colors(n, scheme, settings.bold_is_bright);
                    let nbg = if Self::in_sel(sel, top + r as u64, c) {
                        scheme.selection
                    } else {
                        nbg
                    };
                    if nbg != bg {
                        break;
                    }
                    c += 1;
                }
                if bg != scheme.bg {
                    let x0 = origin.x + start as f32 * cw;
                    let x1 = origin.x + c as f32 * cw;
                    painter.rect_filled(Rect::from_min_max(pos2(x0, y), pos2(x1, y + ch)), 0.0, bg);
                }
            }
            // find matches on this row
            if let Some(f) = &self.find {
                let line = screen.scrollback_len() - screen.scrollback() + r as usize;
                // Matches are in line order: jump to this row's instead of
                // walking all of them for every row of every frame.
                let from = f.matches.partition_point(|m| m.line < line);
                let here = f.matches[from..].iter().take_while(|m| m.line == line);
                for (i, m) in here.enumerate().map(|(k, m)| (from + k, m)) {
                    let x0 = origin.x + m.col as f32 * cw;
                    let col = if i == f.current {
                        Color32::from_rgba_unmultiplied(240, 161, 60, 200)
                    } else {
                        Color32::from_rgba_unmultiplied(240, 161, 60, 70)
                    };
                    painter.rect_filled(
                        Rect::from_min_size(pos2(x0, y), vec2(m.width as f32 * cw, ch)),
                        2.0,
                        col,
                    );
                }
            }
            // command marks in the left padding: green / red when the shell
            // reported an exit status, a faint tick when it didn't
            let abs = lines_scrolled(&p) - screen.scrollback() as u64 + r as u64;
            if let Some(m) = self.marks.iter().rev().find(|m| m.line == abs) {
                let col = match m.exit {
                    Some(0) => Color32::from_rgb(0x8c, 0xd0, 0x5c),
                    Some(_) => Color32::from_rgb(0xec, 0x5f, 0x5a),
                    None => scheme.fg.gamma_multiply(0.35),
                };
                painter.rect_filled(
                    Rect::from_min_size(pos2(rect.left() + 3.0, y + 2.0), vec2(3.0, ch - 4.0)),
                    1.5,
                    col,
                );
            }
            if let Some((hr, c0, c1, _)) = &hover_link {
                if *hr == r {
                    let x0 = origin.x + *c0 as f32 * cw;
                    painter.line_segment(
                        [
                            pos2(x0, y + ch - 1.0),
                            pos2(origin.x + *c1 as f32 * cw, y + ch - 1.0),
                        ],
                        Stroke::new(1.0, scheme.fg),
                    );
                }
            }
            // text runs: consecutive ASCII cells with identical style share one galley;
            // everything else is placed per cell so columns always line up.
            let mut run = String::new();
            let mut run_start = 0u16;
            let mut run_style: Option<(Color32, bool, bool, bool)> = None;
            let flush =
                |run: &mut String, start: u16, style: Option<(Color32, bool, bool, bool)>| {
                    if let Some((fg, bold, underline, italic)) = style {
                        if !run.trim().is_empty() {
                            let pos = pos2(origin.x + start as f32 * cw, y + text_dy);
                            let face = match (&bold_font, bold) {
                                (Some(b), true) => b.clone(),
                                _ => font.clone(),
                            };
                            if italic {
                                // egui slants text laid out with `italics`; the
                                // monospace advance stays the same.
                                let job = egui::text::LayoutJob::single_section(
                                    run.clone(),
                                    egui::TextFormat {
                                        font_id: face,
                                        color: fg,
                                        italics: true,
                                        ..Default::default()
                                    },
                                );
                                painter.galley(pos, painter.layout_job(job), fg);
                            } else {
                                match (&bold_font, bold) {
                                    (Some(b), true) => {
                                        painter.text(
                                            pos,
                                            Align2::LEFT_TOP,
                                            run.as_str(),
                                            b.clone(),
                                            fg,
                                        );
                                    }
                                    // No bold face: thicken by drawing twice, a hair apart.
                                    (None, true) => {
                                        painter.text(
                                            pos,
                                            Align2::LEFT_TOP,
                                            run.as_str(),
                                            font.clone(),
                                            fg,
                                        );
                                        painter.text(
                                            pos + vec2(0.6, 0.0),
                                            Align2::LEFT_TOP,
                                            run.as_str(),
                                            font.clone(),
                                            fg,
                                        );
                                    }
                                    _ => {
                                        painter.text(
                                            pos,
                                            Align2::LEFT_TOP,
                                            run.as_str(),
                                            font.clone(),
                                            fg,
                                        );
                                    }
                                }
                            }
                        }
                        if underline {
                            let x0 = origin.x + start as f32 * cw;
                            let n = run.chars().count() as f32;
                            painter.line_segment(
                                [pos2(x0, y + ch - 1.5), pos2(x0 + n * cw, y + ch - 1.5)],
                                Stroke::new(1.0, fg),
                            );
                        }
                    }
                    run.clear();
                };
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { break };
                if cell.is_wide_continuation() {
                    continue;
                }
                let (fg, _) = cell_colors(cell, scheme, settings.bold_is_bright);
                let style = (fg, cell.bold(), cell.underline(), cell.italic());
                let txt = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                let ascii = txt.len() == 1 && txt.as_bytes()[0].is_ascii_graphic() || txt == " ";
                if ascii && run_style == Some(style) && !cell.is_wide() {
                    run.push_str(txt);
                    continue;
                }
                flush(&mut run, run_start, run_style);
                if ascii && !cell.is_wide() {
                    run_start = c;
                    run_style = Some(style);
                    run.push_str(txt);
                } else {
                    run_style = None;
                    let width = if cell.is_wide() { 2.0 } else { 1.0 };
                    let x = origin.x + c as f32 * cw;
                    let pos = pos2(x + (width * cw) / 2.0, y + text_dy);
                    let f = match (&bold_font, cell.bold()) {
                        (Some(b), true) => b.clone(),
                        _ => font.clone(),
                    };
                    let mut chars = txt.chars();
                    let first = chars.next().unwrap_or(' ');
                    let cell_rect = Rect::from_min_size(pos2(x, y), vec2(width * cw, ch));
                    // Emoji in colour where the platform can draw them, as
                    // Terminal.app does; otherwise the font's (monochrome) glyph.
                    // A skin-tone modifier lands in the next cell of its own;
                    // draw it with the emoji it modifies, and not on its own.
                    let next = screen.cell(r, c + width as u16).map(|n| n.contents());
                    let joined;
                    let txt = match next {
                        Some(m) if crate::emoji::is_skin_tone(m) && crate::emoji::is_emoji(txt) => {
                            joined = format!("{txt}{m}");
                            joined.as_str()
                        }
                        _ if crate::emoji::is_skin_tone(txt) => "",
                        _ => txt,
                    };
                    let emoji = (!txt.is_empty() && crate::emoji::is_emoji(txt))
                        .then(|| {
                            let ppp = ui.ctx().pixels_per_point();
                            crate::emoji::texture(ui.ctx(), txt, (ch * ppp).round() as u32)
                                .map(|t| (t, ppp))
                        })
                        .flatten();
                    if let Some((tex, ppp)) = emoji {
                        let size = tex.size_vec2() / ppp;
                        let scale = (cell_rect.width() / size.x).min(1.0);
                        let r = Rect::from_center_size(cell_rect.center(), size * scale);
                        painter.image(
                            tex.id(),
                            r,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else if txt.is_empty() {
                        // the modifier, already drawn with its emoji
                    } else if chars.next().is_some()
                        || !crate::term::draw_box(&painter, first, cell_rect, fg)
                    {
                        painter.text(pos, Align2::CENTER_TOP, txt, f, fg);
                    }
                    if cell.underline() {
                        painter.line_segment(
                            [pos2(x, y + ch - 1.5), pos2(x + width * cw, y + ch - 1.5)],
                            Stroke::new(1.0, fg),
                        );
                    }
                }
            }
            flush(&mut run, run_start, run_style);
        }

        // Input methods (Chinese, Japanese, Korean, dead keys) stay on while
        // the view has focus, wherever the cursor is and whether a program
        // hides it (Claude Code does): turning it off and on with the cursor
        // would cancel a composition halfway.
        {
            let (cr, cc) = screen.cursor_position();
            let at = pos2(
                origin.x + cc.min(cols.saturating_sub(1)) as f32 * cw,
                origin.y + cr.min(rows.saturating_sub(1)) as f32 * ch,
            );
            if focused && interactive {
                ui.ctx().output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        purpose: egui::IMEPurpose::Terminal,
                        rect,
                        cursor_rect: Rect::from_min_size(at, vec2(cw, ch)),
                        should_interrupt_composition: false,
                    })
                });
            }
            if !self.preedit.is_empty() {
                let g = painter.layout_no_wrap(self.preedit.clone(), font.clone(), scheme.bg);
                let r = Rect::from_min_size(at, vec2(g.size().x.max(cw), ch));
                painter.rect_filled(r, 2.0, scheme.cursor);
                painter.galley(at + vec2(0.0, text_dy), g, scheme.bg);
            }
        }

        // cursor
        if !screen.hide_cursor() && !scrolled {
            let (cr, cc) = screen.cursor_position();
            if cr < rows && cc < cols {
                let blink_on = !settings.cursor_blink || !focused || (time * 1.6) as i64 % 2 == 0;
                let x = origin.x + cc as f32 * cw;
                let y = origin.y + cr as f32 * ch;
                let wide = screen.cell(cr, cc).is_some_and(|c| c.is_wide());
                let cell_rect =
                    Rect::from_min_size(pos2(x, y), vec2(if wide { 2.0 * cw } else { cw }, ch));
                // The shape a program asked for (nvim's bar in insert mode),
                // else the one in Settings.
                let shape = match screen.cursor_style() {
                    vt100::CursorStyle::BlinkBlock | vt100::CursorStyle::SteadyBlock => {
                        CursorStyle::Block
                    }
                    vt100::CursorStyle::BlinkUnderline | vt100::CursorStyle::SteadyUnderline => {
                        CursorStyle::Underline
                    }
                    vt100::CursorStyle::BlinkVerticalBar
                    | vt100::CursorStyle::SteadyVerticalBar => CursorStyle::Bar,
                    _ => settings.cursor,
                };
                if !focused {
                    painter.rect_stroke(
                        cell_rect.shrink(0.5),
                        1.0,
                        Stroke::new(1.0, scheme.cursor),
                        egui::StrokeKind::Inside,
                    );
                } else if blink_on && self.preedit.is_empty() {
                    match shape {
                        CursorStyle::Block => {
                            painter.rect_filled(cell_rect, 1.0, scheme.cursor);
                            if let Some(cell) = screen.cell(cr, cc) {
                                if cell.has_contents() {
                                    painter.text(
                                        pos2(x, y + text_dy),
                                        Align2::LEFT_TOP,
                                        cell.contents(),
                                        font.clone(),
                                        scheme.bg,
                                    );
                                }
                            }
                        }
                        CursorStyle::Underline => {
                            painter.rect_filled(
                                Rect::from_min_size(pos2(x, y + ch - 2.0), vec2(cw, 2.0)),
                                0.0,
                                scheme.cursor,
                            );
                        }
                        CursorStyle::Bar => {
                            painter.rect_filled(
                                Rect::from_min_size(pos2(x, y), vec2(2.0, ch)),
                                0.0,
                                scheme.cursor,
                            );
                        }
                    }
                }
                if settings.cursor_blink && focused {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(400));
                }
            }
        }
        if scrolled {
            let txt = trf!("⬆ {n} lines", n = screen.scrollback());
            // Left of the pane's corner buttons (split, snippets), not on top of them.
            let pos = rect.right_top() + vec2(-86.0, 8.0);
            let g = painter.layout_no_wrap(txt, FontId::proportional(12.0), Color32::WHITE);
            let r = Rect::from_min_size(
                pos - vec2(g.size().x + 16.0, 0.0),
                g.size() + vec2(16.0, 8.0),
            );
            painter.rect_filled(r, 6.0, Color32::from_black_alpha(170));
            painter.galley(r.min + vec2(8.0, 4.0), g, Color32::WHITE);
        }
        drop(p);
        if bell {
            self.parser.lock().unwrap().callbacks_mut().bell = false;
            self.bell_flash = time + 0.15;
        }
        if time < self.bell_flash {
            painter.rect_filled(rect, 0.0, Color32::from_white_alpha(18));
            ui.ctx().request_repaint();
        }
        self.find_bar(ui, rect);
        (actions, resp)
    }

    /// The ⌘F bar over the pane's top-right corner. Searching is live as you
    /// type; Enter / Shift+Enter step through matches, Esc closes it.
    fn find_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        use crate::theme::c;
        let Some(f) = &mut self.find else { return };
        enum Act {
            Search,
            Step(bool),
            Close,
        }
        let mut act = None;
        let bar = Rect::from_min_size(
            pos2(rect.right() - 380.0, rect.top() + 40.0),
            vec2(360.0, 36.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        egui::Frame::new()
            .fill(c::PANEL)
            .stroke(Stroke::new(1.0, c::BORDER))
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut f.query)
                            .hint_text(tr("Find in scrollback"))
                            .desired_width(170.0),
                    );
                    if f.want_focus {
                        r.request_focus();
                        f.want_focus = false;
                    }
                    if r.changed() {
                        act = Some(Act::Search);
                    }
                    // A single-line edit drops focus on Enter and Esc; tell them apart.
                    if r.lost_focus() {
                        if ui.input(|i| i.key_pressed(Key::Escape)) {
                            act = Some(Act::Close);
                        } else if ui.input(|i| i.key_pressed(Key::Enter)) {
                            act = Some(Act::Step(!ui.input(|i| i.modifiers.shift)));
                            r.request_focus();
                        }
                    }
                    let count = match (f.matches.len(), f.query.is_empty()) {
                        (_, true) => String::new(),
                        (0, false) => tr("No matches").into(),
                        (n, false) => trf!("{i} of {n}", i = f.current + 1, n = n),
                    };
                    ui.label(egui::RichText::new(count).small().color(c::TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("×")
                            .on_hover_text(tr("Close (Esc)"))
                            .clicked()
                        {
                            act = Some(Act::Close);
                        }
                        if ui
                            .small_button("⏷")
                            .on_hover_text(tr("Next (Enter)"))
                            .clicked()
                        {
                            act = Some(Act::Step(true));
                        }
                        if ui
                            .small_button("⏶")
                            .on_hover_text(tr("Previous (Shift+Enter)"))
                            .clicked()
                        {
                            act = Some(Act::Step(false));
                        }
                    });
                });
            });
        match act {
            Some(Act::Search) => self.run_find(),
            Some(Act::Step(fwd)) => self.step_find(fwd),
            Some(Act::Close) => {
                self.find = None;
                self.want_focus = true;
            }
            None => {}
        }
    }

    fn input_typed(&mut self, actions: &mut Vec<TermAction>, b: Vec<u8>) {
        if b.is_empty() {
            return;
        }
        self.back_to_live();
        actions.push(TermAction::Typed(b));
    }

    /// Typing returns to the live screen and drops the selection.
    pub fn back_to_live(&mut self) {
        {
            let mut p = self.parser.lock().unwrap();
            if p.screen().scrollback() > 0 {
                p.screen_mut().set_scrollback(0);
            }
        }
        self.clear_selection();
    }

    fn select_word(&mut self, (r, c): (u16, u16)) {
        let p = self.parser.lock().unwrap();
        let s = p.screen();
        let is_word = |c: u16| {
            s.cell(r, c)
                .map(|cell| {
                    let t = cell.contents();
                    !t.is_empty()
                        && t.chars()
                            .all(|ch| ch.is_alphanumeric() || "-_./~:@%+".contains(ch))
                })
                .unwrap_or(false)
        };
        if !is_word(c) {
            return;
        }
        let mut a = c;
        while a > 0 && is_word(a - 1) {
            a -= 1;
        }
        let mut b = c;
        while b + 1 < self.cols && is_word(b + 1) {
            b += 1;
        }
        drop(p);
        self.sel_anchor = Some(self.line_of((r, a)));
        self.sel_end = Some(self.line_of((r, b)));
    }
}

fn is_letter_or_digit(k: Key) -> bool {
    let n = k.name();
    n.len() == 1 && n.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The text of a desktop-notification escape: OSC 9 (iTerm2, not its 9;4
/// progress reports), OSC 777;notify (urxvt, Ghostty) or OSC 99 (kitty).
/// Claude Code sends these when `preferredNotifChannel` is iterm2 or ghostty.
pub fn notification_from_osc(params: &[&[u8]]) -> Option<String> {
    let s = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    match params {
        [b"9", rest @ ..] if !rest.is_empty() && rest[0] != b"4" => {
            Some(rest.iter().map(|p| s(p)).collect::<Vec<_>>().join(";"))
        }
        [b"777", b"notify", title, body @ ..] => {
            let body = body.iter().map(|p| s(p)).collect::<Vec<_>>().join(";");
            Some(if body.is_empty() { s(title) } else { body })
        }
        [b"99", .., body] => Some(s(body)),
        _ => None,
    }
}

/// The path in an OSC 7 "file://host/path" URL, percent-decoded.
pub fn cwd_from_osc7(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let mut out = Vec::with_capacity(path.len());
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok().filter(|p| !p.is_empty())
}

/// A path from terminal output as a file on this machine, if it is one:
/// "~/" expanded, and a trailing ":line" or ":line:col" (compiler output) dropped.
fn local_path(raw: &str) -> Option<std::path::PathBuf> {
    let mut s = raw;
    for _ in 0..2 {
        if let Some((head, tail)) = s.rsplit_once(':') {
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                s = head;
            }
        }
    }
    let p = match s.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => std::path::PathBuf::from(s),
    };
    p.exists().then_some(p)
}

/// The line in a "path:line" or "path:line:col" reference.
fn line_of(raw: &str) -> Option<usize> {
    let mut parts = raw.rsplitn(3, ':').collect::<Vec<_>>();
    parts.reverse();
    parts.iter().skip(1).find_map(|p| p.parse().ok())
}

/// Opens a file or folder with the system's default app.
fn open_on_system(path: &std::path::Path) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

fn cell_colors(cell: &vt100::Cell, scheme: &Scheme, bold_bright: bool) -> (Color32, Color32) {
    let mut fg = match cell.fgcolor() {
        vt100::Color::Default => scheme.fg,
        vt100::Color::Idx(i) => {
            let i = if bold_bright && cell.bold() && i < 8 {
                i + 8
            } else {
                i
            };
            scheme.indexed(i)
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    };
    let mut bg = match cell.bgcolor() {
        vt100::Color::Default => scheme.bg,
        vt100::Color::Idx(i) => scheme.indexed(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    };
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.dim() {
        fg = fg.gamma_multiply(0.6);
    }
    (fg, bg)
}

fn mouse_seq(
    enc: vt100::MouseProtocolEncoding,
    button: u8,
    (r, c): (u16, u16),
    press: bool,
) -> Vec<u8> {
    match enc {
        vt100::MouseProtocolEncoding::Sgr => format!(
            "\x1b[<{};{};{}{}",
            button,
            c + 1,
            r + 1,
            if press { 'M' } else { 'm' }
        )
        .into_bytes(),
        _ => {
            let cb = 32u8.saturating_add(button);
            let cx = (c.min(222) as u8).saturating_add(33);
            let cy = (r.min(222) as u8).saturating_add(33);
            vec![0x1b, b'[', b'M', cb, cx, cy]
        }
    }
}

/// A path as iTerm and Terminal.app paste a dropped file: backslash-escaped.
pub fn shell_escape_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for c in p.chars() {
        if c.is_whitespace() || "\\'\"`$&|;<>()[]{}*?!#~^".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// xterm-compatible encoding of a key press.
pub fn encode_key(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let modp = 1 + (m.shift as u8) + 2 * (m.alt as u8) + 4 * (m.ctrl as u8);
    let csi_mod = |final_: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{final_}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_}").into_bytes()
        } else {
            format!("\x1b[{final_}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[{n};{modp}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let alt_prefix = |mut v: Vec<u8>| -> Vec<u8> {
        if m.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    Some(match key {
        // Shift+Enter is a newline in Claude Code and Codex, where a plain
        // Return submits; Esc+CR is what they (and zsh) take as one.
        Key::Enter if m.shift && !m.ctrl => vec![0x1b, b'\r'],
        Key::Enter => alt_prefix(vec![b'\r']),
        Key::Tab => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        Key::Backspace => alt_prefix(if m.ctrl { vec![0x08] } else { vec![0x7f] }),
        Key::Escape => vec![0x1b],
        Key::ArrowUp => csi_mod('A'),
        Key::ArrowDown => csi_mod('B'),
        Key::ArrowRight => csi_mod('C'),
        Key::ArrowLeft => csi_mod('D'),
        Key::Home => csi_mod('H'),
        Key::End => csi_mod('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => {
            if modp > 1 {
                format!("\x1b[1;{modp}P").into_bytes()
            } else {
                b"\x1bOP".to_vec()
            }
        }
        Key::F2 => {
            if modp > 1 {
                format!("\x1b[1;{modp}Q").into_bytes()
            } else {
                b"\x1bOQ".to_vec()
            }
        }
        Key::F3 => {
            if modp > 1 {
                format!("\x1b[1;{modp}R").into_bytes()
            } else {
                b"\x1bOR".to_vec()
            }
        }
        Key::F4 => {
            if modp > 1 {
                format!("\x1b[1;{modp}S").into_bytes()
            } else {
                b"\x1bOS".to_vec()
            }
        }
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ if m.ctrl => {
            let name = key.name();
            let b = match key {
                Key::Space => 0x00,
                Key::OpenBracket => 0x1b,
                Key::Backslash => 0x1c,
                Key::CloseBracket => 0x1d,
                Key::Slash => 0x1f,
                Key::Minus => 0x1f,
                _ if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() => {
                    name.as_bytes()[0].to_ascii_lowercase() & 0x1f
                }
                _ if name == "2" => 0x00,
                _ if name == "6" => 0x1e,
                _ => return None,
            };
            alt_prefix(vec![b])
        }
        _ if m.alt && is_letter_or_digit(key) => {
            let c = key.name().chars().next()?;
            let c = if m.shift {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            };
            vec![0x1b, c as u8]
        }
        _ => return None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_are_escaped() {
        assert_eq!(
            shell_escape_path("/Users/me/Desktop/Screen Shot (2).png"),
            "/Users/me/Desktop/Screen\\ Shot\\ \\(2\\).png"
        );
        assert_eq!(shell_escape_path("/tmp/plain.png"), "/tmp/plain.png");
    }

    #[test]
    fn command_marks_exit_codes_and_output() {
        let p = new_parser(6, 40, 200);
        let mut v = TerminalView::new(p.clone(), egui::Id::unique("t"), 40, 6);
        let run = |v: &mut TerminalView, cmd: &str, out: &str, code: i32| {
            feed(&p, format!("$ {cmd}").as_bytes());
            v.mark_command();
            feed(&p, format!("\r\n{out}\x1b]133;D;{code}\x07").as_bytes());
            v.absorb_exits();
        };
        run(&mut v, "ls", "a\r\nb\r\n", 0);
        // enough output to push the first command into the scrollback
        run(&mut v, "seq 3", "1\r\n2\r\n3\r\n", 0);
        run(&mut v, "false", "", 1);
        feed(&p, b"$ ");
        assert_eq!(
            v.marks.iter().map(|m| m.exit).collect::<Vec<_>>(),
            [Some(0), Some(0), Some(1)]
        );
        // `false` printed nothing, so the last output with text is checked through `seq 3`
        v.marks.pop();
        assert_eq!(v.last_output().as_deref(), Some("1\n2\n3\n$ false"));
        // jumping up lands on the earlier command, scrolled into view
        v.jump_mark(true);
        let top = p
            .lock()
            .unwrap()
            .screen()
            .contents()
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        assert!(top.starts_with("$ "), "top line after jump: {top:?}");
    }

    // With a tiny scrollback the oldest lines fall off; marks must still
    // point at the right lines.
    #[test]
    fn marks_survive_lines_dropping_off_the_scrollback() {
        let p = new_parser(4, 30, 3);
        let mut v = TerminalView::new(p.clone(), egui::Id::unique("t2"), 30, 4);
        for i in 0..6 {
            feed(&p, format!("$ cmd{i}").as_bytes());
            v.mark_command();
            feed(&p, format!("\r\nout{i}a\r\nout{i}b\r\n").as_bytes());
        }
        feed(&p, b"$ ");
        assert!(
            p.lock().unwrap().callbacks().dropped > 0,
            "lines must have dropped off"
        );
        assert_eq!(v.last_output().as_deref(), Some("out5a\nout5b"));
    }

    #[test]
    fn osc7_folders() {
        assert_eq!(
            cwd_from_osc7("file://mac.local/Users/jason/My%20Code").as_deref(),
            Some("/Users/jason/My Code")
        );
        assert_eq!(cwd_from_osc7("file:///tmp").as_deref(), Some("/tmp"));
        assert_eq!(cwd_from_osc7("http://x/y"), None);
        let p = new_parser(4, 20, 10);
        feed(&p, b"\x1b]7;file://h/srv/app\x07");
        assert_eq!(
            p.lock().unwrap().callbacks().cwd.as_deref(),
            Some("/srv/app")
        );
    }

    #[test]
    fn keys() {
        let none = Modifiers::NONE;
        assert_eq!(encode_key(Key::ArrowUp, none, false).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(Key::ArrowUp, none, true).unwrap(), b"\x1bOA");
        assert_eq!(
            encode_key(Key::ArrowLeft, Modifiers::CTRL, false).unwrap(),
            b"\x1b[1;5D"
        );
        assert_eq!(
            encode_key(Key::C, Modifiers::CTRL, false).unwrap(),
            vec![0x03]
        );
        assert_eq!(encode_key(Key::Backspace, none, false).unwrap(), vec![0x7f]);
        assert_eq!(
            encode_key(Key::Tab, Modifiers::SHIFT, false).unwrap(),
            b"\x1b[Z"
        );
        assert_eq!(encode_key(Key::F5, none, false).unwrap(), b"\x1b[15~");
        assert_eq!(encode_key(Key::B, Modifiers::ALT, false).unwrap(), b"\x1bb");
        assert!(
            encode_key(Key::A, none, false).is_none(),
            "plain letters come via Text events"
        );
    }

    #[test]
    fn device_status_replies() {
        let p = new_parser(24, 80, 100);
        feed(&p, b"hello\r\n  ");
        assert_eq!(feed(&p, b"\x1b[6n"), b"\x1b[2;3R");
        assert_eq!(feed(&p, b"\x1b[c"), b"\x1b[?62;22c");
        feed(&p, b"\x1b]0;my title\x07");
        assert_eq!(
            p.lock().unwrap().callbacks_mut().title.take().as_deref(),
            Some("my title")
        );
    }

    #[test]
    fn selection_and_wide_chars() {
        let p = new_parser(5, 20, 100);
        feed(&p, "ab中文cd\r\nline two".as_bytes());
        let mut v = TerminalView::new(p, egui::Id::unique("t"), 20, 5);
        v.sel_anchor = Some((0, 0));
        v.sel_end = Some((1, 3));
        assert_eq!(v.selection_text().unwrap(), "ab中文cd\nline");
    }

    #[test]
    fn a_program_asking_for_distinct_keys_is_tracked() {
        let p = new_parser(3, 20, 10);
        feed(&p, b"\x1b[>1u");
        assert!(p.lock().unwrap().callbacks().keys.disambiguate());
        feed(&p, b"\x1b[<u");
        assert!(!p.lock().unwrap().callbacks().keys.disambiguate());
    }

    #[test]
    fn osc52_puts_text_on_the_clipboard() {
        let p = new_parser(3, 20, 10);
        // "hello, clipboard" in base64.
        feed(&p, b"\x1b]52;c;aGVsbG8sIGNsaXBib2FyZA==\x07");
        assert_eq!(
            p.lock()
                .unwrap()
                .callbacks_mut()
                .clipboard
                .take()
                .as_deref(),
            Some("hello, clipboard")
        );
    }

    #[test]
    fn shift_enter_is_a_newline_and_pastes_cant_break_out() {
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            encode_key(Key::Enter, shift, false),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(
            encode_key(Key::Enter, Modifiers::default(), false),
            Some(b"\r".to_vec())
        );
        let p = new_parser(3, 20, 10);
        feed(&p, b"\x1b[?2004h");
        let v = TerminalView::new(p, egui::Id::unique("t"), 20, 3);
        let out = v.paste_bytes("safe\x1b[20\x1b[201~1~rm -rf ~\n");
        // The inner end marker is gone; the only one left closes the paste.
        assert_eq!(out, b"\x1b[200~safe[20[201~1~rm -rf ~\r\x1b[201~".to_vec());
    }

    #[test]
    fn a_selection_stays_on_its_text_as_output_scrolls() {
        let p = new_parser(3, 20, 100);
        feed(&p, b"keep me\r\nb\r\nc");
        let mut v = TerminalView::new(p.clone(), egui::Id::unique("t"), 20, 3);
        v.sel_anchor = Some(v.line_of((0, 0)));
        v.sel_end = Some(v.line_of((0, 6)));
        // Five more lines push "keep me" into the scrollback.
        feed(&p, b"\r\nd\r\ne\r\nf\r\ng\r\nh");
        assert_eq!(v.selection_text().as_deref(), Some("keep me"));
        // And select-all reaches back into the history.
        v.select_all();
        assert!(v.selection_text().unwrap().starts_with("keep me\nb\nc"));
    }
}

#[cfg(test)]
mod pitwall_tests {
    #[test]
    fn notification_escapes() {
        let p = super::new_parser(4, 40, 10);
        super::feed(&p, b"\x1b]9;Claude needs your permission\x07");
        super::feed(&p, b"\x1b]777;notify;Claude Code;Task finished\x07");
        super::feed(&p, b"\x1b]9;4;1;50\x07");
        let got = std::mem::take(&mut p.lock().unwrap().callbacks_mut().notifications);
        assert_eq!(got, ["Claude needs your permission", "Task finished"]);
    }

    #[test]
    fn line_numbers_in_compiler_output() {
        assert_eq!(super::line_of("src/app.rs:42"), Some(42));
        assert_eq!(super::line_of("src/app.rs:42:7"), Some(42));
        assert_eq!(super::line_of("src/app.rs"), None);
    }
}
