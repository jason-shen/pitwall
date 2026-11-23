//! Drawing.

use std::collections::HashMap;

use pitwall_core::proto::{ApprovalSource, TaskInfo, TaskStatus, Tier};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::app::{App, Focus, Hits, InputKind, Modal, Section, View};
use pitwall_core::git::LineKind;

const ACCENT: Color = Color::Rgb(0x7f, 0xc4, 0xff);
const GREEN: Color = Color::Rgb(0x98, 0xc3, 0x79);
const AMBER: Color = Color::Rgb(0xe5, 0xc0, 0x7b);
const RED: Color = Color::Rgb(0xe0, 0x6c, 0x75);
const MUTED: Color = Color::Rgb(0x6b, 0x72, 0x80);
const DIM_BG: Color = Color::Rgb(0x26, 0x2b, 0x33);
const SEL_BG: Color = Color::Rgb(0x2f, 0x3d, 0x52);
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn status_icon(t: &TaskInfo, frame: usize) -> (&'static str, Color) {
    if t.pending_approvals > 0 {
        return ("!", AMBER);
    }
    match t.status {
        TaskStatus::Running => (SPIN[frame % SPIN.len()], GREEN),
        TaskStatus::Waiting { .. } => ("◆", AMBER),
        TaskStatus::Idle if t.attention => ("●", ACCENT),
        TaskStatus::Idle => ("○", MUTED),
        TaskStatus::Exited { .. } => ("✕", RED),
    }
}

fn tier_color(t: Tier) -> Color {
    match t {
        Tier::ReadOnly => GREEN,
        Tier::Mutating => AMBER,
        Tier::Destructive => RED,
    }
}

fn trunc(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut cur = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur + cw + 1 > w {
            break;
        }
        out.push(ch);
        cur += cw;
    }
    out.push('…');
    out
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.hits = Hits::default();
    let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let right = if app.zoom || main.width < 60 {
        main
    } else {
        let w = (main.width / 4).clamp(26, 40);
        let [side, right] =
            Layout::horizontal([Constraint::Length(w), Constraint::Fill(1)]).areas(main);
        draw_sidebar(f, app, side);
        right
    };
    let terms = draw_main(f, app, right);
    draw_status(f, app, status);
    if app.modal.is_some() {
        draw_modal(f, app, area);
    }
    for (id, rows, cols) in terms {
        app.sync_term(&id, rows, cols);
    }
}

// ── sidebar ─────────────────────────────────────────────────────────────

fn section_header(title: &str, extra: &str, active: bool, w: u16) -> Line<'static> {
    let st = if active {
        Style::default().fg(Color::Black).bg(ACCENT).bold()
    } else {
        Style::default().fg(MUTED).bold()
    };
    let mut text = format!(" {title}");
    if !extra.is_empty() {
        text.push_str(&format!(" · {extra}"));
    }
    let text = trunc(&text, w as usize);
    let pad = (w as usize).saturating_sub(text.width());
    Line::from(vec![
        Span::styled(text, st),
        Span::styled(" ".repeat(pad), if active { st } else { Style::default() }),
    ])
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM_BG }));
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hits.sidebar = area;
    let w = inner.width;

    let ids = app.agent_ids();
    let h = inner.height;
    let agents_h = ((ids.len().max(1) + 1) as u16).min((h / 3).max(4));
    let changes_h = ((app.changes.len().max(1) + 1) as u16).min((h / 3).max(4));
    let [a_area, f_area, c_area] = Layout::vertical([
        Constraint::Length(agents_h),
        Constraint::Fill(1),
        Constraint::Length(changes_h),
    ])
    .areas(inner);

    // AGENTS
    let need = app.tasks.values().filter(|t| t.attention).count();
    let extra = if need > 0 {
        format!("{need} need you")
    } else {
        String::new()
    };
    let sec_active = focused && app.section == Section::Agents;
    let mut lines = vec![section_header("AGENTS", &extra, sec_active, w)];
    let rows_avail = a_area.height.saturating_sub(1) as usize;
    let off = app.agent_sel.saturating_sub(rows_avail.saturating_sub(1));
    if ids.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {} c  new agent", app.prefix_label),
            Style::default().fg(MUTED),
        )));
    }
    for (i, id) in ids.iter().enumerate().skip(off).take(rows_avail) {
        let t = &app.tasks[id];
        let (icon, color) = status_icon(t, app.frame);
        let selected = i == app.agent_sel;
        let viewing = matches!(app.active_view(), Some(View::Term(x)) if x == id);
        let agent = trunc(&t.agent, 8);
        let name_w = (w as usize).saturating_sub(4 + agent.width() + 1);
        let name = trunc(&t.name, name_w);
        let pad = name_w.saturating_sub(name.width());
        let base = if selected && sec_active {
            Style::default().bg(SEL_BG)
        } else if selected || viewing {
            Style::default().bg(DIM_BG)
        } else {
            Style::default()
        };
        let name_style = if t.attention {
            base.fg(Color::White).bold()
        } else {
            base.fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled(" ", base),
            Span::styled(icon, base.fg(color)),
            Span::styled(" ", base),
            Span::styled(name, name_style),
            Span::styled(" ".repeat(pad + 1), base),
            Span::styled(agent, base.fg(MUTED)),
            Span::styled(" ", base),
        ]));
        let r = Rect::new(a_area.x, a_area.y + 1 + (i - off) as u16, a_area.width, 1);
        app.hits.rows.push((r, Section::Agents, i));
    }
    f.render_widget(Paragraph::new(lines), a_area);

    // FILES
    let ctx = app.ctx_dir();
    let dir_name = ctx
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let sec_active = focused && app.section == Section::Files;
    let mut lines = vec![section_header("FILES", &dir_name, sec_active, w)];
    let status: HashMap<&str, char> = app
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.letter()))
        .collect();
    let rows_avail = f_area.height.saturating_sub(1) as usize;
    let off = app.file_sel.saturating_sub(rows_avail.saturating_sub(1));
    for (i, r) in app.tree.rows.iter().enumerate().skip(off).take(rows_avail) {
        let selected = i == app.file_sel;
        let base = if selected && sec_active {
            Style::default().bg(SEL_BG)
        } else if selected {
            Style::default().bg(DIM_BG)
        } else {
            Style::default()
        };
        let indent = "  ".repeat(r.depth);
        let (marker, mark_color) = if r.is_dir {
            let dirty = app
                .changes
                .iter()
                .any(|c| c.path.starts_with(&format!("{}/", r.rel)));
            (if dirty { '•' } else { ' ' }, AMBER)
        } else {
            match status.get(r.rel.as_str()) {
                Some('M') => ('M', AMBER),
                Some('A') => ('A', GREEN),
                Some('U') => ('U', GREEN),
                Some('D') => ('D', RED),
                Some(c) => (*c, MUTED),
                None => (' ', MUTED),
            }
        };
        let label = if r.is_dir {
            format!(
                "{}{} {}/",
                indent,
                if r.expanded { "▾" } else { "▸" },
                r.name
            )
        } else {
            format!("{}  {}", indent, r.name)
        };
        let label_w = (w as usize).saturating_sub(3);
        let label = trunc(&format!(" {label}"), label_w);
        let pad = label_w.saturating_sub(label.width());
        let color = if r.is_dir {
            ACCENT
        } else if marker != ' ' {
            Color::White
        } else {
            Color::Gray
        };
        lines.push(Line::from(vec![
            Span::styled(label, base.fg(color)),
            Span::styled(" ".repeat(pad), base),
            Span::styled(format!(" {marker} "), base.fg(mark_color)),
        ]));
        let rr = Rect::new(f_area.x, f_area.y + 1 + (i - off) as u16, f_area.width, 1);
        app.hits.rows.push((rr, Section::Files, i));
    }
    if app.tree.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            " (empty)",
            Style::default().fg(MUTED),
        )));
    }
    f.render_widget(Paragraph::new(lines), f_area);

    // CHANGES
    let mut ahead = app.branch.clone().unwrap_or_default();
    if let Some(n) = app.ahead.filter(|n| *n > 0) {
        ahead.push_str(&format!(" ↑{n}"));
    }
    let sec_active = focused && app.section == Section::Changes;
    let mut lines = vec![section_header("CHANGES", &ahead, sec_active, w)];
    let rows_avail = c_area.height.saturating_sub(1) as usize;
    let off = app.change_sel.saturating_sub(rows_avail.saturating_sub(1));
    if let Some(e) = &app.git_error {
        lines.push(Line::from(Span::styled(
            format!(" {}", trunc(e, w as usize - 1)),
            Style::default().fg(RED),
        )));
    } else if app.ctx_root.is_none() {
        lines.push(Line::from(Span::styled(
            " not a git repo",
            Style::default().fg(MUTED),
        )));
    } else if app.changes.is_empty() {
        lines.push(Line::from(Span::styled(
            " clean",
            Style::default().fg(MUTED),
        )));
    }
    for (i, c) in app.changes.iter().enumerate().skip(off).take(rows_avail) {
        let selected = i == app.change_sel;
        let base = if selected && sec_active {
            Style::default().bg(SEL_BG)
        } else if selected {
            Style::default().bg(DIM_BG)
        } else {
            Style::default()
        };
        let (xs, ys) = if c.untracked() {
            ("?".to_string(), "?".to_string())
        } else {
            (c.x.to_string(), c.y.to_string())
        };
        let path_w = (w as usize).saturating_sub(4);
        let path = trunc(&c.path, path_w);
        let pad = path_w.saturating_sub(path.width());
        lines.push(Line::from(vec![
            Span::styled(" ", base),
            Span::styled(xs, base.fg(if c.untracked() { MUTED } else { GREEN })),
            Span::styled(ys, base.fg(if c.untracked() { MUTED } else { RED })),
            Span::styled(" ", base),
            Span::styled(path, base.fg(Color::Gray)),
            Span::styled(" ".repeat(pad), base),
        ]));
        let rr = Rect::new(c_area.x, c_area.y + 1 + (i - off) as u16, c_area.width, 1);
        app.hits.rows.push((rr, Section::Changes, i));
    }
    f.render_widget(Paragraph::new(lines), c_area);
}

// ── main area ───────────────────────────────────────────────────────────

fn view_title(app: &App, v: &View) -> String {
    match v {
        View::Term(id) => match app.tasks.get(id) {
            Some(t) => {
                let (icon, _) = status_icon(t, app.frame);
                format!("{icon} {}", t.name)
            }
            None => format!("{id} (gone)"),
        },
        View::Editor(e) => format!(
            "{}{}{}",
            e.name(),
            if e.dirty { " ●" } else { "" },
            if e.stale { " ⚠" } else { "" }
        ),
        View::Diff(d) => d.title(),
        View::Timeline(t) => format!("undo · {}", t.task),
    }
}

#[allow(clippy::needless_range_loop)]
fn draw_main(f: &mut Frame, app: &mut App, area: Rect) -> Vec<(String, u16, u16)> {
    let [tabs_area, panes_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);

    // tab bar
    let mut spans = vec![];
    let mut x = tabs_area.x;
    for (i, v) in app.tabs.iter().enumerate() {
        let title = trunc(&view_title(app, v), 28);
        let label = format!(" {} {} ", i + 1, title);
        let in_active = app.panes.get(app.active).copied().flatten() == Some(i);
        let in_other = app
            .panes
            .iter()
            .enumerate()
            .any(|(p, t)| p != app.active && *t == Some(i));
        let st = if in_active && app.focus == Focus::Pane {
            Style::default().fg(Color::Black).bg(ACCENT).bold()
        } else if in_active {
            Style::default().fg(Color::White).bg(SEL_BG)
        } else if in_other {
            Style::default().fg(Color::White).bg(DIM_BG)
        } else {
            Style::default().fg(MUTED)
        };
        let wdt = label.width() as u16;
        if x + wdt > tabs_area.right() {
            break;
        }
        app.hits.tabs.push((Rect::new(x, tabs_area.y, wdt, 1), i));
        spans.push(Span::styled(label, st));
        spans.push(Span::raw(" "));
        x += wdt + 1;
    }
    if app.tabs.is_empty() {
        spans.push(Span::styled(" no tabs", Style::default().fg(MUTED)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), tabs_area);

    if app.tabs.is_empty() {
        draw_welcome(f, app, panes_area);
        return vec![];
    }

    let n = app.panes.len();
    let areas = if n == 2 {
        Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
            .split(panes_area)
            .to_vec()
    } else {
        vec![panes_area]
    };
    let mut terms = vec![];
    for p in 0..n {
        let Some(idx) = app.panes[p] else { continue };
        if idx >= app.tabs.len() {
            continue;
        }
        let focused = app.focus == Focus::Pane && app.active == p;
        let title = view_title(app, &app.tabs[idx]);
        let sub = match &app.tabs[idx] {
            View::Term(id) => app.tasks.get(id).map(|t| match &t.status {
                TaskStatus::Waiting { reason } => format!(" {} ", trunc(reason, 50)),
                _ => t
                    .branch
                    .clone()
                    .map(|b| format!(" {b} "))
                    .unwrap_or_default(),
            }),
            View::Editor(e) => Some(format!(" {}:{} ", e.cy + 1, e.cx + 1)),
            _ => None,
        }
        .unwrap_or_default();
        let block = Block::bordered()
            .border_type(if focused {
                BorderType::Thick
            } else {
                BorderType::Rounded
            })
            .border_style(Style::default().fg(if focused { ACCENT } else { DIM_BG }))
            .title(Line::from(Span::styled(
                format!(" {title} "),
                Style::default()
                    .fg(if focused { ACCENT } else { Color::Gray })
                    .bold(),
            )))
            .title_bottom(
                Line::from(Span::styled(sub, Style::default().fg(MUTED))).right_aligned(),
            );
        let inner = block.inner(areas[p]);
        f.render_widget(block, areas[p]);
        app.hits.panes.push((inner, p));
        match &mut app.tabs[idx] {
            View::Term(id) => {
                let id = id.clone();
                if let Some(t) = app.terms.get(&id) {
                    let screen = t.parser.screen();
                    super::term::render(screen, inner, f.buffer_mut());
                    if t.scroll > 0 {
                        let label = format!(" scrollback +{} ", t.scroll);
                        let r = Rect::new(
                            inner.right().saturating_sub(label.width() as u16),
                            inner.y,
                            label.width() as u16,
                            1,
                        );
                        f.render_widget(
                            Paragraph::new(Span::styled(
                                label,
                                Style::default().fg(Color::Black).bg(AMBER),
                            )),
                            r,
                        );
                    } else if focused && !screen.hide_cursor() {
                        let (cr, cc) = screen.cursor_position();
                        if cr < inner.height && cc < inner.width {
                            f.set_cursor_position((inner.x + cc, inner.y + cr));
                        }
                    }
                } else {
                    f.render_widget(
                        Paragraph::new(Span::styled("attaching…", Style::default().fg(MUTED))),
                        inner,
                    );
                }
                terms.push((id, inner.height, inner.width));
            }
            View::Editor(e) => draw_editor(f, e, inner, focused),
            View::Diff(d) => draw_diff(f, d, inner, focused),
            View::Timeline(t) => draw_timeline(f, t, inner, focused),
        }
    }
    terms
}

fn draw_welcome(f: &mut Frame, app: &App, area: Rect) {
    let p = &app.prefix_label;
    let k = |key: &str, what: &str| {
        Line::from(vec![
            Span::styled(format!("  {p} {key:<3}"), Style::default().fg(ACCENT)),
            Span::raw(format!(" {what}")),
        ])
    };
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  pitwall — run agents, watch them, edit alongside them",
            Style::default().bold(),
        )),
        Line::from(""),
        k("c", "new agent (own worktree + branch)"),
        k("t", "new shell in this directory"),
        k("f", "files  ·  ⏎ opens the built-in editor"),
        k("g", "git changes  ·  s stage · c commit · m merge"),
        k("d", "diff  ·  s/u stage/unstage hunk · ⏎ jump to line"),
        k("u", "undo timeline  ·  r revert any step"),
        k("?", "all keys"),
        Line::from(""),
        Line::from(Span::styled(
            "  agents keep running when you quit — run `pitwall` again to come back",
            Style::default().fg(MUTED),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_editor(f: &mut Frame, e: &mut pitwall_core::editor::Editor, area: Rect, focused: bool) {
    let gutter = format!("{}", e.lines.len()).len() + 2;
    let text_w = (area.width as usize).saturating_sub(gutter);
    let h = area.height as usize;
    if e.follow {
        e.ensure_visible(h, text_w);
        e.follow = false;
    }
    let buf = f.buffer_mut();
    for row in 0..h {
        let y = e.scroll_y + row;
        let sy = area.y + row as u16;
        if y >= e.lines.len() {
            buf.set_string(
                area.x,
                sy,
                format!("{:>w$} ", "~", w = gutter - 1),
                Style::default().fg(DIM_BG),
            );
            continue;
        }
        let cur = y == e.cy;
        let line_bg = if cur && focused {
            Style::default().bg(Color::Rgb(0x1e, 0x23, 0x2b))
        } else {
            Style::default()
        };
        buf.set_string(
            area.x,
            sy,
            format!("{:>w$} ", y + 1, w = gutter - 1),
            Style::default().fg(if cur { Color::Gray } else { MUTED }),
        );
        // fill line bg
        if cur && focused {
            buf.set_string(area.x + gutter as u16, sy, " ".repeat(text_w), line_bg);
        }
        let line = &e.lines[y];
        let spans = e.highlight.get(y);
        let mut col = 0usize;
        let mut byte = 0usize;
        let mut span_i = 0usize;
        let mut span_end = spans
            .and_then(|s| s.first())
            .map(|s| s.1)
            .unwrap_or(usize::MAX);
        for ch in line.chars() {
            while byte >= span_end {
                span_i += 1;
                span_end += spans
                    .and_then(|s| s.get(span_i))
                    .map(|s| s.1)
                    .unwrap_or(usize::MAX / 2);
            }
            let fg = spans
                .and_then(|s| s.get(span_i))
                .map(|s| Color::Rgb(s.0 .0, s.0 .1, s.0 .2))
                .unwrap_or(Color::Gray);
            let w = e.char_width(ch, col);
            if col >= e.scroll_x && col + w <= e.scroll_x + text_w {
                let x = area.x + (gutter + col - e.scroll_x) as u16;
                if ch == '\t' {
                    buf.set_string(x, sy, " ".repeat(w), line_bg);
                } else {
                    let mut s = [0u8; 4];
                    buf.set_string(x, sy, ch.encode_utf8(&mut s), line_bg.fg(fg));
                }
            }
            col += w;
            byte += ch.len_utf8();
            if col > e.scroll_x + text_w {
                break;
            }
        }
    }
    if focused {
        let col = e.col_of(e.cy, e.cx);
        if e.cy >= e.scroll_y
            && e.cy < e.scroll_y + h
            && col >= e.scroll_x
            && col < e.scroll_x + text_w
        {
            f.set_cursor_position((
                area.x + (gutter + col - e.scroll_x) as u16,
                area.y + (e.cy - e.scroll_y) as u16,
            ));
        }
    }
    if e.stale {
        let msg = " changed on disk — ^r reload (discards edits) or ^s overwrite ";
        let r = Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            (msg.width() as u16).min(area.width),
            1,
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                msg,
                Style::default().fg(Color::Black).bg(AMBER),
            )),
            r,
        );
    }
}

fn draw_diff(f: &mut Frame, d: &mut super::app::DiffView, area: Rect, focused: bool) {
    let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let mode_span = |label: &str, key: &str, on: bool| {
        Span::styled(
            format!(" {key} {label} "),
            if on {
                Style::default().fg(Color::Black).bg(ACCENT)
            } else {
                Style::default().fg(MUTED)
            },
        )
    };
    let mut bar_spans = vec![];
    if d.fixed.is_none() {
        bar_spans.push(mode_span(
            "unstaged",
            "1",
            d.mode == pitwall_core::git::DiffMode::Unstaged,
        ));
        bar_spans.push(mode_span(
            "staged",
            "2",
            d.mode == pitwall_core::git::DiffMode::Staged,
        ));
        bar_spans.push(mode_span(
            "vs base",
            "3",
            matches!(d.mode, pitwall_core::git::DiffMode::Base(_)),
        ));
        bar_spans.push(Span::styled(
            "  s/u hunk · S/U file · n/p hunk · ⏎ edit",
            Style::default().fg(MUTED),
        ));
    } else {
        bar_spans.push(Span::styled(
            " snapshot diff · n/p hunk · ⏎ open file",
            Style::default().fg(MUTED),
        ));
    }
    let files: Vec<String> = d
        .parsed
        .files
        .iter()
        .map(|(f, a, r)| format!("{f} +{a} -{r}"))
        .collect();
    if !files.is_empty() {
        bar_spans.push(Span::styled(
            format!("   {} file(s)", files.len()),
            Style::default().fg(Color::Gray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(bar_spans)), bar);

    if let Some(e) = &d.error {
        f.render_widget(
            Paragraph::new(Span::styled(e.clone(), Style::default().fg(RED)))
                .wrap(Wrap { trim: false }),
            body,
        );
        return;
    }
    if d.parsed.lines.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("  no changes", Style::default().fg(MUTED))),
            body,
        );
        return;
    }
    let h = body.height as usize;
    if d.follow {
        if d.cursor < d.scroll {
            d.scroll = d.cursor;
        } else if d.cursor >= d.scroll + h {
            d.scroll = d.cursor + 1 - h;
        }
        d.follow = false;
    }
    let cur_hunk = d.parsed.lines.get(d.cursor).and_then(|l| l.hunk);
    let mut lines = vec![];
    for (i, l) in d.parsed.lines.iter().enumerate().skip(d.scroll).take(h) {
        let mut st = match l.kind {
            LineKind::FileHeader => Style::default().fg(AMBER).bold(),
            LineKind::Meta => Style::default().fg(MUTED),
            LineKind::Hunk => Style::default().fg(ACCENT),
            LineKind::Add => Style::default().fg(GREEN),
            LineKind::Del => Style::default().fg(RED),
            LineKind::Context => Style::default().fg(Color::Gray),
        };
        let gutter = if l.hunk.is_some() && l.hunk == cur_hunk && focused {
            "▎"
        } else {
            " "
        };
        if i == d.cursor && focused {
            st = st.bg(SEL_BG);
        }
        let text = if l.kind == LineKind::FileHeader {
            format!("━━ {}", l.file.clone().unwrap_or_default())
        } else {
            l.text.replace('\t', "    ")
        };
        lines.push(Line::from(vec![
            Span::styled(gutter, Style::default().fg(ACCENT)),
            Span::styled(text, st),
        ]));
    }
    f.render_widget(Paragraph::new(lines), body);
}

fn draw_timeline(f: &mut Frame, t: &mut super::app::TimelineView, area: Rect, focused: bool) {
    let head = Line::from(vec![
        Span::styled(
            " undo timeline ",
            Style::default().fg(Color::Black).bg(ACCENT),
        ),
        Span::styled(
            "  ⏎ diff step · r revert to here · D all changes · c checkpoint",
            Style::default().fg(MUTED),
        ),
    ]);
    let mut lines = vec![head, Line::from("")];
    let h = area.height.saturating_sub(2) as usize;
    if t.sel < t.offset {
        t.offset = t.sel;
    } else if h > 0 && t.sel >= t.offset + h {
        t.offset = t.sel + 1 - h;
    }
    if t.snaps.is_empty() {
        lines.push(Line::from(Span::styled(
            "  loading… (or no snapshots yet)",
            Style::default().fg(MUTED),
        )));
    }
    let now = pitwall_core::now_unix();
    for (i, s) in t.snaps.iter().enumerate().skip(t.offset).take(h) {
        let sel = i == t.sel;
        let base = if sel && focused {
            Style::default().bg(SEL_BG)
        } else if sel {
            Style::default().bg(DIM_BG)
        } else {
            Style::default()
        };
        let age = {
            let d = (now - s.unix).max(0);
            if d < 60 {
                format!("{d}s")
            } else if d < 3600 {
                format!("{}m", d / 60)
            } else if d < 86400 {
                format!("{}h", d / 3600)
            } else {
                format!("{}d", d / 86400)
            }
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" #{:<4}", s.seq),
                base.fg(if i == 0 { ACCENT } else { Color::Gray }),
            ),
            Span::styled(format!("{age:>4} "), base.fg(MUTED)),
            Span::styled(format!("+{:<4}", s.insertions), base.fg(GREEN)),
            Span::styled(format!("-{:<4}", s.deletions), base.fg(RED)),
            Span::styled(format!("{:>3}f  ", s.files_changed), base.fg(MUTED)),
            Span::styled(
                trunc(&s.label, (area.width as usize).saturating_sub(30)),
                base.fg(Color::White),
            ),
            Span::styled(" ".repeat(area.width as usize), base),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

// ── status bar ──────────────────────────────────────────────────────────

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let p = &app.prefix_label;
    let mut left: Vec<Span> = vec![];
    if app.prefix_armed {
        left.push(Span::styled(
            " PREFIX ",
            Style::default().fg(Color::Black).bg(AMBER).bold(),
        ));
        left.push(Span::styled(" c agent · t shell · n/p tab · s split · o pane · z zoom · d diff · u undo · e $EDITOR · y/r approve/reject · q quit · ? help", Style::default().fg(Color::Gray)));
    } else if let Some(a) = app.approvals.values().next() {
        let who = match &a.source {
            ApprovalSource::Task { task_id, .. } => task_id.clone(),
            ApprovalSource::Ops { session, .. } => format!("ops #{session}"),
        };
        left.push(Span::styled(
            format!(" ⚑ {who} wants to run "),
            Style::default().fg(Color::Black).bg(AMBER).bold(),
        ));
        left.push(Span::styled(
            format!(" {} ", trunc(&a.command, 60)),
            Style::default()
                .fg(Color::White)
                .bg(DIM_BG)
                .add_modifier(Modifier::BOLD),
        ));
        left.push(Span::styled(
            format!(" {} ", a.tier.label()),
            Style::default().fg(tier_color(a.tier)),
        ));
        let more = if app.approvals.len() > 1 {
            format!(" · {p} A all ({})", app.approvals.len())
        } else {
            String::new()
        };
        left.push(Span::styled(
            format!(" {p} y approve · {p} r reject{more}"),
            Style::default().fg(Color::Gray),
        ));
    } else if let Some((msg, _, err)) = &app.notice {
        left.push(Span::styled(
            format!(" {msg}"),
            Style::default().fg(if *err { RED } else { GREEN }),
        ));
    } else {
        let hint = match (app.focus, app.active_view()) {
            (Focus::Sidebar, _) => match app.section {
                Section::Agents => {
                    "⏎ open · c new agent · t shell · K kill · D remove · m merge · Tab section"
                        .to_string()
                }
                Section::Files => {
                    "⏎ open in editor · e $EDITOR · ←/→ fold · Tab section".to_string()
                }
                Section::Changes => {
                    "⏎ diff · s stage/unstage · A stage all · c commit · m merge · x discard"
                        .to_string()
                }
            },
            (Focus::Pane, Some(View::Term(_))) => {
                format!("typing goes to the agent · {p} h sidebar · {p} ? help · shift+PgUp scroll")
            }
            (Focus::Pane, Some(View::Editor(_))) => {
                "^s save · ^z/^y undo/redo · ^f find · ^g line · ^e $EDITOR · Esc sidebar"
                    .to_string()
            }
            (Focus::Pane, Some(View::Diff(d))) if d.fixed.is_some() => {
                "j/k move · n/p hunk · ⏎ open file at line · Esc back".to_string()
            }
            (Focus::Pane, Some(View::Diff(_))) => {
                "j/k move · s/u stage/unstage hunk · S/U file · ⏎ edit at line · Esc back"
                    .to_string()
            }
            (Focus::Pane, Some(View::Timeline(_))) => {
                "⏎ diff of step · r revert · D everything · c checkpoint · Esc back".to_string()
            }
            _ => format!("{p} ? help"),
        };
        left.push(Span::styled(format!(" {hint}"), Style::default().fg(MUTED)));
    }

    let running = app
        .tasks
        .values()
        .filter(|t| t.status == TaskStatus::Running)
        .count();
    let need = app.tasks.values().filter(|t| t.attention).count();
    let mut right = vec![];
    if need > 0 {
        right.push(Span::styled(
            format!(" ◆ {need} need you "),
            Style::default().fg(AMBER).bold(),
        ));
    }
    right.push(Span::styled(
        format!(" {} {running} running ", SPIN[app.frame % SPIN.len()]),
        Style::default().fg(if running > 0 { GREEN } else { MUTED }),
    ));
    if !app.connected {
        right.push(Span::styled(
            " disconnected ",
            Style::default().fg(Color::Black).bg(RED),
        ));
    }
    let rw: u16 = right.iter().map(|s| s.width() as u16).sum();
    let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(rw)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(left)).style(Style::default().bg(Color::Rgb(0x16, 0x19, 0x1e))),
        l,
    );
    f.render_widget(
        Paragraph::new(Line::from(right)).style(Style::default().bg(Color::Rgb(0x16, 0x19, 0x1e))),
        r,
    );
}

// ── modals ──────────────────────────────────────────────────────────────

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 3,
        w,
        h,
    )
}

fn draw_modal(f: &mut Frame, app: &App, area: Rect) {
    let Some(m) = &app.modal else { return };
    let block = |title: &str| {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT))
            .title(Span::styled(
                format!(" {title} "),
                Style::default().fg(ACCENT).bold(),
            ))
    };
    match m {
        Modal::NewAgent {
            agents,
            agent,
            name,
            prompt,
            worktree,
            field,
        } => {
            let r = centered(area, 72, 12);
            f.render_widget(Clear, r);
            let b = block("New agent");
            let inner = b.inner(r);
            f.render_widget(b, r);
            let hl = |i: usize| {
                if *field == i {
                    Style::default().fg(Color::Black).bg(ACCENT)
                } else {
                    Style::default().fg(MUTED)
                }
            };
            let cur = |i: usize| if *field == i { "▏" } else { "" };
            let ag = agents.get(*agent).cloned().unwrap_or_default();
            let dir = app.ctx_dir();
            let lines = vec![
                Line::from(vec![
                    Span::styled(" Agent    ", hl(0)),
                    Span::raw("  ◀ "),
                    Span::styled(ag, Style::default().bold()),
                    Span::raw(" ▶"),
                ]),
                Line::from(vec![
                    Span::styled(" Name     ", hl(1)),
                    Span::raw(format!("  {name}{}", cur(1))),
                ]),
                Line::from(vec![
                    Span::styled(" Prompt   ", hl(2)),
                    Span::raw(format!("  {}{}", trunc(prompt, 56), cur(2))),
                ]),
                Line::from(vec![
                    Span::styled(" Worktree ", hl(3)),
                    Span::raw(format!("  [{}] ", if *worktree { "x" } else { " " })),
                    Span::styled(
                        if *worktree {
                            format!(
                                "own branch {} + worktree",
                                if name.is_empty() { "<name>" } else { name }
                            )
                        } else {
                            format!("run in {}", dir.display())
                        },
                        Style::default().fg(MUTED),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    " ⏎ start · Tab next field · ←/→ change · Esc cancel",
                    Style::default().fg(MUTED),
                )),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
        Modal::Input { kind, title, value } => {
            let r = centered(area, 70, 5);
            f.render_widget(Clear, r);
            let b = block(title);
            let inner = b.inner(r);
            f.render_widget(b, r);
            let hint = match kind {
                InputKind::Commit => "⏎ commit staged changes · Esc cancel",
                InputKind::Find => "⏎ find (^n / F3 next) · Esc cancel",
                InputKind::Goto => "⏎ go · Esc cancel",
            };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(format!(" {value}▏")),
                    Line::from(Span::styled(format!(" {hint}"), Style::default().fg(MUTED))),
                ]),
                inner,
            );
        }
        Modal::Confirm { text, .. } => {
            let lines: Vec<&str> = text.lines().collect();
            let r = centered(area, 76, lines.len() as u16 + 4);
            f.render_widget(Clear, r);
            let b = block("Confirm");
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut out: Vec<Line> = lines.iter().map(|l| Line::from(format!(" {l}"))).collect();
            out.push(Line::from(""));
            out.push(Line::from(Span::styled(
                " y yes · n no",
                Style::default().fg(AMBER),
            )));
            f.render_widget(Paragraph::new(out).wrap(Wrap { trim: false }), inner);
        }
        Modal::Approvals { sel } => {
            let r = centered(area, 90, (app.approvals.len() as u16 + 4).min(20));
            f.render_widget(Clear, r);
            let b = block("Waiting for approval");
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut out = vec![];
            for (i, a) in app.approvals.values().enumerate() {
                let who = match &a.source {
                    ApprovalSource::Task { task_id, tool } => format!("{task_id}/{tool}"),
                    ApprovalSource::Ops { session, hosts } => {
                        format!("ops#{session} {}", hosts.join(","))
                    }
                };
                let st = if i == *sel {
                    Style::default().bg(SEL_BG)
                } else {
                    Style::default()
                };
                out.push(Line::from(vec![
                    Span::styled(
                        format!(" {:<11}", a.tier.label()),
                        st.fg(tier_color(a.tier)),
                    ),
                    Span::styled(format!("{:<18} ", trunc(&who, 18)), st.fg(MUTED)),
                    Span::styled(trunc(&a.command, 55), st.fg(Color::White)),
                ]));
            }
            if app.approvals.is_empty() {
                out.push(Line::from(Span::styled(
                    " nothing pending",
                    Style::default().fg(MUTED),
                )));
            }
            out.push(Line::from(""));
            out.push(Line::from(Span::styled(
                " y approve · n reject · j/k move · Esc close",
                Style::default().fg(MUTED),
            )));
            f.render_widget(Paragraph::new(out), inner);
        }
        Modal::Help => {
            let r = centered(area, 92, 32);
            f.render_widget(Clear, r);
            let b = block("pitwall keys");
            let inner = b.inner(r);
            f.render_widget(b, r);
            let p = &app.prefix_label;
            let row = |k: &str, d: &str| {
                Line::from(vec![
                    Span::styled(format!(" {k:<18}"), Style::default().fg(ACCENT)),
                    Span::raw(d.to_string()),
                ])
            };
            let h = |t: &str| {
                Line::from(Span::styled(
                    format!(" {t}"),
                    Style::default().fg(AMBER).bold(),
                ))
            };
            let lines = vec![
                h(&format!("Anywhere (prefix {p}, then key)")),
                row("c / t", "new agent / new shell"),
                row("n p 1-9 x", "next / prev / jump to / close tab"),
                row("s o z", "split / other pane / zoom (hide sidebar)"),
                row(
                    "h l  w f g",
                    "sidebar / pane · agents / files / git changes",
                ),
                row("d u", "diff / undo timeline for the current agent"),
                row("a", "jump to next agent that needs you"),
                row("y r A", "approve / reject next command · list all"),
                row("m K", "merge agent branch / kill agent"),
                row("e", "open current file in $EDITOR"),
                row(
                    &format!("{p} {p}"),
                    "send the prefix key itself to the agent",
                ),
                row("q", "quit (agents keep running)"),
                h("Sidebar"),
                row("j k ⏎ Tab", "move / open / next section"),
                row("agents", "D remove · K kill · m merge"),
                row(
                    "changes",
                    "s stage/unstage · A stage all · c commit · x discard",
                ),
                h("Editor"),
                row("^s ^z ^y", "save / undo / redo"),
                row("^f ^n ^g", "find / next / go to line"),
                row("^k ^r ^e", "delete line / reload from disk / $EDITOR"),
                h("Diff"),
                row("1 2 3", "unstaged / staged / everything vs task base"),
                row("s u  S U", "stage / unstage hunk · whole file"),
                row("n p  ⏎", "next / prev hunk · open file at line"),
                h("Timeline"),
                row(
                    "⏎ r D c",
                    "diff step / revert to step / all changes / checkpoint",
                ),
                Line::from(""),
                Line::from(Span::styled(
                    " any key to close",
                    Style::default().fg(MUTED),
                )),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}
