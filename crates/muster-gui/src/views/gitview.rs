//! The git view: lazygit's layout and keys, scoped to the tab's directory,
//! plus the agent-only parts (diff vs the agent's base, merge, snapshots).

use super::*;
use crate::app::Panel;

const TITLES: [&str; 5] = [
    "Status",
    "Changed files",
    "Branches",
    "Commits",
    "Undo history",
];

/// What selecting row `i` of `panel` shows on the diff side.
fn source(app: &App, panel: Panel, i: usize, is_agent: bool) -> Option<MainSrc> {
    match panel {
        Panel::Status => Some(match base_of(app) {
            Some(b) if is_agent => MainSrc::Changes {
                path: None,
                mode: DiffMode::Base(b),
            },
            _ => MainSrc::Changes {
                path: None,
                mode: DiffMode::Unstaged,
            },
        }),
        Panel::Files => app.changes.get(i).map(|c| MainSrc::Changes {
            path: Some(c.path.clone()),
            mode: if c.staged() && !c.unstaged() {
                DiffMode::Staged
            } else {
                DiffMode::Unstaged
            },
        }),
        Panel::Branches => app.branches.get(i).map(|b| MainSrc::Commit(b.name.clone())),
        Panel::Commits => app.commits.get(i).map(|c| MainSrc::Commit(c.hash.clone())),
        Panel::Snapshots => app
            .tabs
            .get(app.cur)
            .and_then(|t| t.snaps.get(i))
            .map(|s| MainSrc::Snapshot(s.clone())),
        Panel::Main => None,
    }
}

fn base_of(app: &App) -> Option<String> {
    app.tabs
        .get(app.cur)
        .and_then(|t| app.tasks.get(&t.id))
        .and_then(|t| t.base.clone())
}

fn len_of(app: &App, panel: Panel) -> usize {
    match panel {
        Panel::Status => 1,
        Panel::Files => app.changes.len(),
        Panel::Branches => app.branches.len(),
        Panel::Commits => app.commits.len(),
        Panel::Snapshots => app.tabs.get(app.cur).map_or(0, |t| t.snaps.len()),
        Panel::Main => 0,
    }
}

pub(super) fn body(
    ui: &mut Ui,
    app: &mut App,
    p: usize,
    body: Rect,
    focused: bool,
    acts: &mut Vec<Action>,
) {
    let Some(item) = app.tabs.get(app.cur) else {
        return;
    };
    let id = item.id.clone();
    let is_agent = app.tasks.get(&id).is_some_and(|t| t.agent != "shell");
    if app.ctx_root.is_none() {
        ui.painter().text(
            body.center(),
            Align2::CENTER_CENTER,
            "not a git repository",
            FontId::proportional(14.0),
            MUTED,
        );
        return;
    }
    if focused {
        keys(ui, app, is_agent, acts);
    }

    // Side by side when there's room; stacked like lazygit in a narrow pane
    // (half of a split), so the diff isn't squeezed to a sliver.
    let (left, main) = if body.width() >= 640.0 {
        let left_w = (body.width() * 0.38).clamp(260.0, 440.0);
        let left = Rect::from_min_max(body.min, pos2(body.min.x + left_w, body.max.y));
        (
            left,
            Rect::from_min_max(pos2(left.max.x + 4.0, body.min.y), body.max),
        )
    } else {
        let top_h = (body.height() * 0.45).max(220.0).min(body.height() - 120.0);
        let top = Rect::from_min_max(body.min, pos2(body.max.x, body.min.y + top_h));
        (
            top,
            Rect::from_min_max(pos2(body.min.x, top.max.y + 4.0), body.max),
        )
    };

    // Panel heights: status is short, files get the most room.
    let panels: Vec<Panel> = Panel::LISTS
        .into_iter()
        .filter(|p| *p != Panel::Snapshots || is_agent)
        .collect();
    let weights = |p: Panel| match p {
        Panel::Status => 0.0,
        Panel::Files => 3.0,
        Panel::Branches => 1.5,
        Panel::Commits => 2.5,
        Panel::Snapshots => 2.0,
        Panel::Main => 0.0,
    };
    let status_h = 64.0;
    let gap = 4.0;
    let flex = left.height() - status_h - gap * (panels.len() - 1) as f32;
    let total: f32 = panels.iter().map(|p| weights(*p)).sum();
    let mut y = left.min.y;
    for panel in panels {
        let h = if panel == Panel::Status {
            status_h
        } else {
            (flex * weights(panel) / total).max(40.0)
        };
        let r = Rect::from_min_size(pos2(left.min.x, y), vec2(left.width(), h));
        y += h + gap;
        list_panel(ui, app, p, panel, r, focused, is_agent, acts);
    }
    main_panel(ui, app, p, main, focused, acts);
}

fn frame_for(ui: &mut Ui, r: Rect, title: &str, active: bool) -> Rect {
    let painter = ui.painter();
    painter.rect_stroke(
        r.shrink(0.5),
        CornerRadius::same(4),
        Stroke::new(1.0, if active { ACCENT } else { SEL }),
        egui::StrokeKind::Inside,
    );
    painter.text(
        r.min + vec2(8.0, 3.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(11.5),
        if active { ACCENT } else { MUTED },
    );
    Rect::from_min_max(r.min + vec2(4.0, 19.0), r.max - vec2(4.0, 3.0))
}

#[allow(clippy::too_many_arguments)]
fn list_panel(
    ui: &mut Ui,
    app: &mut App,
    p: usize,
    panel: Panel,
    r: Rect,
    focused: bool,
    is_agent: bool,
    acts: &mut Vec<Action>,
) {
    let gv = &app.tabs[app.cur].git;
    let active = gv.panel == panel;
    let pi = panel.index();
    let sel = gv.sel[pi];
    let n = len_of(app, panel);
    let title = match panel {
        Panel::Files => format!("[2] Changed files  {n}"),
        _ => format!("[{}] {}", pi + 1, TITLES[pi]),
    };
    let inner = frame_for(ui, r, &title, active && focused);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut child;
    ui.set_clip_rect(inner);

    if panel == Panel::Status {
        status_panel(ui, app, is_agent, acts);
        return;
    }
    if n == 0 {
        let empty = match panel {
            Panel::Files => "working tree clean",
            Panel::Snapshots => "no snapshots yet",
            _ => "",
        };
        ui.label(RichText::new(empty).small().color(MUTED));
        return;
    }
    let row_h = 18.0;
    let mut clicked: Option<(usize, bool)> = None;
    let mut sa = ScrollArea::vertical()
        .id_salt(("git", p, pi))
        .auto_shrink([false, false]);
    if active {
        // Keep the selection in view as j/k move it.
        let top = ui.ctx().memory(|m| {
            m.data
                .get_temp::<f32>(Id::unique(("git-scroll", p, pi)))
                .unwrap_or(0.0)
        });
        let want = sel as f32 * (row_h + ui.spacing().item_spacing.y);
        let view_h = inner.height();
        let next = if want < top {
            want
        } else if want + row_h > top + view_h {
            want + row_h - view_h
        } else {
            top
        };
        if next != top {
            sa = sa.vertical_scroll_offset(next);
        }
    }
    let out = sa.show_rows(ui, row_h, n, |ui, range| {
        for i in range {
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            let painter = ui.painter();
            if i == sel {
                painter.rect_filled(rect, 3.0, if active { SEL } else { PANEL_2 });
            } else if resp.hovered() {
                painter.rect_filled(rect, 3.0, PANEL_2);
            }
            let y = rect.center().y;
            let x = rect.left() + 4.0;
            let mono = FontId::monospace(12.0);
            let prop = FontId::proportional(13.0);
            match panel {
                Panel::Files => {
                    let c = &app.changes[i];
                    let (a, b) = if c.untracked() {
                        ('?', '?')
                    } else {
                        (c.x, c.y)
                    };
                    let ac = if c.untracked() { MUTED } else { GREEN };
                    let bc = if c.untracked() { MUTED } else { RED };
                    painter.text(pos2(x, y), Align2::LEFT_CENTER, a, mono.clone(), ac);
                    painter.text(pos2(x + 9.0, y), Align2::LEFT_CENTER, b, mono, bc);
                    painter.text(pos2(x + 24.0, y), Align2::LEFT_CENTER, &c.path, prop, TEXT);
                    let cc = c.clone();
                    resp.context_menu(|ui| {
                        if ui.button("Open in editor").clicked() {
                            acts.push(Action::OpenFile(cc.path.clone(), None));
                            ui.close();
                        }
                        if ui.button("Stage").clicked() {
                            acts.push(Action::Stage(cc.path.clone()));
                            ui.close();
                        }
                        if ui.button("Unstage").clicked() {
                            acts.push(Action::Unstage(cc.path.clone()));
                            ui.close();
                        }
                        if ui
                            .button(RichText::new("Discard changes…").color(RED))
                            .clicked()
                        {
                            acts.push(Action::Discard(cc.clone()));
                            ui.close();
                        }
                    });
                }
                Panel::Branches => {
                    let b = &app.branches[i];
                    painter.text(
                        pos2(x, y),
                        Align2::LEFT_CENTER,
                        if b.current { "*" } else { " " },
                        mono,
                        GREEN,
                    );
                    let g = painter.layout_no_wrap(
                        b.name.clone(),
                        prop.clone(),
                        if b.current { GREEN } else { TEXT },
                    );
                    let w = g.size().x;
                    painter.galley(pos2(x + 12.0, y - g.size().y / 2.0), g, Color32::WHITE);
                    painter.text(
                        pos2(x + 20.0 + w, y),
                        Align2::LEFT_CENTER,
                        &b.when,
                        FontId::proportional(11.0),
                        MUTED,
                    );
                }
                Panel::Commits => {
                    let c = &app.commits[i];
                    painter.text(pos2(x, y), Align2::LEFT_CENTER, &c.short, mono, AMBER);
                    painter.text(
                        pos2(x + 64.0, y),
                        Align2::LEFT_CENTER,
                        &c.subject,
                        prop,
                        TEXT,
                    );
                }
                Panel::Snapshots => {
                    let s = &app.tabs[app.cur].snaps[i];
                    painter.text(
                        pos2(x, y),
                        Align2::LEFT_CENTER,
                        format!("#{:<3}", s.seq),
                        mono.clone(),
                        if i == 0 { ACCENT } else { MUTED },
                    );
                    painter.text(
                        pos2(x + 38.0, y),
                        Align2::LEFT_CENTER,
                        format!("{:>4}", ago(s.unix)),
                        mono.clone(),
                        MUTED,
                    );
                    painter.text(
                        pos2(x + 78.0, y),
                        Align2::LEFT_CENTER,
                        format!("+{} −{}", s.insertions, s.deletions),
                        mono,
                        GREEN,
                    );
                    painter.text(
                        pos2(x + 150.0, y),
                        Align2::LEFT_CENTER,
                        &s.label,
                        prop,
                        TEXT,
                    );
                    let (task, snap) = (app.tabs[app.cur].id.clone(), s.clone());
                    resp.context_menu(|ui| {
                        if i > 0 && ui.button("Revert files to here…").clicked() {
                            acts.push(Action::Revert(task.clone(), snap.clone()));
                            ui.close();
                        }
                    });
                }
                Panel::Status | Panel::Main => {}
            }
            if resp.clicked() {
                clicked = Some((i, false));
            }
            if resp.double_clicked() {
                clicked = Some((i, true));
            }
        }
    });
    ui.ctx().memory_mut(|m| {
        m.data.insert_temp(
            Id::unique(("git-scroll", p, pi)),
            out.state.clamped_offset().y,
        )
    });
    if let Some((i, double)) = clicked {
        acts.push(Action::FocusPane(p));
        let gv = &mut app.tabs[app.cur].git;
        gv.panel = panel;
        gv.sel[pi] = i;
        if let Some(src) = source(app, panel, i, is_agent) {
            acts.push(Action::ShowMain(src));
        }
        if double {
            match panel {
                Panel::Files => acts.push(Action::OpenFile(app.changes[i].path.clone(), None)),
                Panel::Branches => acts.push(Action::Checkout(app.branches[i].name.clone())),
                _ => app.tabs[app.cur].git.panel = Panel::Main,
            }
        }
    }
}

fn status_panel(ui: &mut Ui, app: &App, is_agent: bool, acts: &mut Vec<Action>) {
    let mut b = app.branch.clone().unwrap_or_else(|| "detached".into());
    if let Some(n) = app.ahead.filter(|n| *n > 0) {
        b.push_str(&format!("  ↑{n} since start"));
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(b).color(GREEN));
        ui.add(
            egui::Label::new(RichText::new(tilde(&app.ctx_dir())).small().color(MUTED)).truncate(),
        );
    });
    ui.horizontal(|ui| {
        if is_agent
            && base_of(app).is_some()
            && ui
                .small_button("vs base")
                .on_hover_text("Everything this agent changed (b)")
                .clicked()
        {
            acts.push(Action::ShowMain(MainSrc::Changes {
                path: None,
                mode: DiffMode::Base(base_of(app).unwrap()),
            }));
        }
        if ui.small_button("all changes").clicked() {
            acts.push(Action::ShowMain(MainSrc::Changes {
                path: None,
                mode: DiffMode::Unstaged,
            }));
        }
        if ui.small_button("commit").on_hover_text("c").clicked() {
            acts.push(Action::StartCommit);
        }
        let has_branch = app
            .tabs
            .get(app.cur)
            .and_then(|t| app.tasks.get(&t.id))
            .and_then(|t| t.branch.as_ref())
            .is_some();
        let id = app
            .tabs
            .get(app.cur)
            .map(|t| t.id.clone())
            .unwrap_or_default();
        if let Some(list) = app.remarks.get(&id).filter(|l| !l.is_empty()) {
            let all = list
                .iter()
                .map(|r| format!("{}:{}  {}", r.file, r.line.unwrap_or(0), r.text.trim()))
                .collect::<Vec<_>>()
                .join("\n");
            if ui
                .small_button(format!("💬 send {} to agent", list.len()))
                .on_hover_text(all)
                .clicked()
            {
                acts.push(Action::SendRemarks(id.clone()));
            }
            if ui.small_button("clear").clicked() {
                acts.push(Action::ClearRemarks(id.clone()));
            }
        }
        if has_branch
            && ui
                .small_button("✓ accept…")
                .on_hover_text(
                    "Commit the agent's work and merge it into the branch it came from (m)",
                )
                .clicked()
        {
            acts.push(Action::Accept);
        }
    });
}

fn main_panel(
    ui: &mut Ui,
    app: &mut App,
    p: usize,
    r: Rect,
    focused: bool,
    acts: &mut Vec<Action>,
) {
    let cur = app.cur;
    let active = app.tabs[cur].git.panel == Panel::Main;
    let id = &app.tabs[cur].id;
    let is_agent = app.tasks.get(id).is_some_and(|t| t.agent != "shell");
    let remarks = app.remarks.get(id).cloned().unwrap_or_default();
    let font = theme::term_font(app.settings.font_size - 0.5);
    let gv = &mut app.tabs[cur].git;
    let src = gv.main.clone();
    let d = &mut gv.diff;
    let title = if d.loading && d.title.is_empty() {
        "loading…".to_string()
    } else {
        d.title.clone()
    };
    let inner = frame_for(
        ui,
        r,
        &format!("{title}{}", if d.loading { "  ⟳" } else { "" }),
        active && focused,
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut child;
    ui.set_clip_rect(inner);

    let cur_line = d.parsed.lines.get(d.cursor).cloned();
    let cur_hunk = cur_line
        .as_ref()
        .and_then(|l| l.hunk)
        .and_then(|h| d.parsed.hunks.get(h))
        .cloned();
    let mode = match &src {
        Some(MainSrc::Changes { mode, .. }) => Some(mode.clone()),
        _ => None,
    };

    // Toolbar.
    ui.horizontal(|ui| {
        if let Some(MainSrc::Changes { path, mode }) = &src {
            for (m, label) in [
                (DiffMode::Unstaged, "Unstaged"),
                (DiffMode::Staged, "Staged"),
            ] {
                if ui.selectable_label(*mode == m, label).clicked() {
                    acts.push(Action::ShowMain(MainSrc::Changes {
                        path: path.clone(),
                        mode: m,
                    }));
                }
            }
            ui.separator();
        }
        match (&mode, &cur_hunk) {
            (Some(DiffMode::Unstaged), Some(h))
                if ui.small_button("stage hunk").on_hover_text("s").clicked() =>
            {
                acts.push(Action::StageHunk(h.clone()));
            }
            (Some(DiffMode::Staged), Some(h))
                if ui.small_button("unstage hunk").on_hover_text("u").clicked() =>
            {
                acts.push(Action::UnstageHunk(h.clone()));
            }
            _ => {}
        }
        // Against the base the new side is the working tree too, so one of
        // the agent's hunks can be thrown away on its own.
        if let (Some(DiffMode::Unstaged | DiffMode::Base(_)), Some(h)) = (&mode, &cur_hunk) {
            if ui
                .small_button("discard hunk")
                .on_hover_text("d: undo this hunk in the working tree")
                .clicked()
            {
                acts.push(Action::DiscardHunk(h.clone()));
            }
        }
        if let Some(l) = &cur_line {
            if let Some(f) = &l.file {
                if ui
                    .small_button("edit")
                    .on_hover_text("e: open at this line")
                    .clicked()
                {
                    acts.push(Action::OpenFile(f.clone(), Some(l.new_line.unwrap_or(1))));
                }
                if is_agent
                    && ui
                        .small_button("💬 comment")
                        .on_hover_text(
                            "r: a review comment on this line, sent to the agent with the others",
                        )
                        .clicked()
                {
                    acts.push(Action::Remark);
                }
            }
        }
        let files = d.parsed.files.len();
        let (a, rm) = d
            .parsed
            .files
            .iter()
            .fold((0, 0), |acc, f| (acc.0 + f.1, acc.1 + f.2));
        ui.label(
            RichText::new(format!("{files} file(s)  +{a} −{rm}"))
                .small()
                .color(MUTED),
        );
    });

    if let Some(err) = &d.error {
        ui.label(RichText::new(err).color(RED));
        return;
    }
    if d.parsed.lines.is_empty() {
        if !d.loading {
            ui.label(RichText::new("No changes").color(MUTED));
        }
        return;
    }

    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 1.0;
    let mut sa = ScrollArea::both()
        .id_salt(("gitmain", p))
        .auto_shrink([false, false]);
    if d.scroll_to_cursor {
        sa = sa.vertical_scroll_offset(
            (d.cursor as f32 * row_h - ui.available_height() / 2.0).max(0.0),
        );
        d.scroll_to_cursor = false;
    }
    let cur_h = d.parsed.lines.get(d.cursor).and_then(|l| l.hunk);
    let total = d.parsed.lines.len();
    let commented: std::collections::HashSet<(String, Option<usize>)> =
        remarks.iter().map(|r| (r.file.clone(), r.line)).collect();
    let mut click: Option<(usize, bool)> = None;
    sa.show_rows(ui, row_h, total, |ui, range| {
        let width = ui.available_width().max(800.0);
        for i in range {
            let l = &d.parsed.lines[i];
            let (rect, r) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
            let painter = ui.painter();
            let (fg, bg) = match l.kind {
                LineKind::FileHeader => (AMBER, Some(PANEL_2)),
                LineKind::Meta => (MUTED, None),
                LineKind::Hunk => (ACCENT, Some(Color32::from_rgb(0x1a, 0x22, 0x2e))),
                LineKind::Add => (GREEN, Some(Color32::from_rgb(0x16, 0x26, 0x18))),
                LineKind::Del => (RED, Some(Color32::from_rgb(0x2a, 0x17, 0x19))),
                LineKind::Context => (MUTED, None),
            };
            if let Some(b) = bg {
                painter.rect_filled(rect, 0.0, b);
            }
            if i == d.cursor && active {
                painter.rect_filled(rect, 0.0, SEL.gamma_multiply(0.9));
            }
            if l.hunk.is_some() && l.hunk == cur_h {
                painter.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, row_h)), 0.0, ACCENT);
            }
            let text = if l.kind == LineKind::FileHeader {
                format!("━━ {}", l.file.clone().unwrap_or_default())
            } else {
                l.text.replace('\t', "    ")
            };
            painter.text(
                rect.min + vec2(8.0, 0.0),
                Align2::LEFT_TOP,
                text,
                font.clone(),
                fg,
            );
            if l.kind != LineKind::FileHeader
                && l.file
                    .as_ref()
                    .is_some_and(|f| commented.contains(&(f.clone(), l.new_line)))
            {
                painter.text(
                    pos2(ui.clip_rect().right() - 8.0, rect.top()),
                    Align2::RIGHT_TOP,
                    "💬",
                    font.clone(),
                    AMBER,
                );
            }
            if r.clicked() {
                click = Some((i, false));
            }
            if r.double_clicked() {
                click = Some((i, true));
            }
        }
    });
    if let Some((i, double)) = click {
        d.cursor = i;
        gv.panel = Panel::Main;
        acts.push(Action::FocusPane(p));
        if double {
            if let Some(f) = &d.parsed.lines[i].file {
                acts.push(Action::OpenFile(
                    f.clone(),
                    Some(d.parsed.lines[i].new_line.unwrap_or(1)),
                ));
            }
        }
    }
}

/// lazygit's keys. Only plain keys; ⌘/Ctrl chords stay app shortcuts.
fn keys(ui: &mut Ui, app: &mut App, is_agent: bool, acts: &mut Vec<Action>) {
    let events = ui.input(|i| i.events.clone());
    for ev in events {
        let Event::Key {
            key,
            pressed: true,
            modifiers: m,
            ..
        } = ev
        else {
            continue;
        };
        if m.command || m.ctrl || m.alt {
            continue;
        }
        let cur = app.cur;
        let panel = app.tabs[cur].git.panel;
        let lists: Vec<Panel> = Panel::LISTS
            .into_iter()
            .filter(|p| *p != Panel::Snapshots || is_agent)
            .collect();
        let jump = |p: Panel, app: &mut App, acts: &mut Vec<Action>| {
            let gv = &mut app.tabs[cur].git;
            gv.panel = p;
            let i = gv.sel[p.index()];
            if let Some(src) = source(app, p, i, is_agent) {
                acts.push(Action::ShowMain(src));
            }
        };
        match key {
            Key::Num1 => jump(Panel::Status, app, acts),
            Key::Num2 => jump(Panel::Files, app, acts),
            Key::Num3 => jump(Panel::Branches, app, acts),
            Key::Num4 => jump(Panel::Commits, app, acts),
            Key::Num5 if is_agent => jump(Panel::Snapshots, app, acts),
            Key::Tab | Key::L | Key::H | Key::ArrowRight | Key::ArrowLeft
                if panel != Panel::Main =>
            {
                let back = m.shift || matches!(key, Key::H | Key::ArrowLeft);
                let at = lists.iter().position(|p| *p == panel).unwrap_or(0) as isize;
                let next = lists
                    [(at + if back { -1 } else { 1 }).rem_euclid(lists.len() as isize) as usize];
                jump(next, app, acts);
            }
            Key::Escape if panel == Panel::Main => app.tabs[cur].git.panel = Panel::Files,
            _ if panel == Panel::Main => main_key(app, key, m, acts),
            Key::J | Key::K | Key::ArrowDown | Key::ArrowUp => {
                let n = len_of(app, panel);
                if n == 0 {
                    continue;
                }
                let gv = &mut app.tabs[cur].git;
                let s = &mut gv.sel[panel.index()];
                let before = *s;
                *s = if matches!(key, Key::J | Key::ArrowDown) {
                    (*s + 1).min(n - 1)
                } else {
                    s.saturating_sub(1)
                };
                let now = *s;
                if now != before {
                    if let Some(src) = source(app, panel, now, is_agent) {
                        acts.push(Action::ShowMain(src));
                    }
                }
            }
            Key::Enter => {
                let i = app.tabs[cur].git.sel[panel.index()];
                if let Some(src) = source(app, panel, i, is_agent) {
                    acts.push(Action::ShowMain(src));
                }
                app.tabs[cur].git.panel = Panel::Main;
            }
            Key::C if !m.shift => acts.push(Action::StartCommit),
            Key::M if is_agent => acts.push(Action::Accept),
            Key::B if is_agent => {
                if let Some(b) = base_of(app) {
                    acts.push(Action::ShowMain(MainSrc::Changes {
                        path: None,
                        mode: DiffMode::Base(b),
                    }));
                }
            }
            _ => match panel {
                Panel::Files => {
                    let i = app.tabs[cur].git.sel[1];
                    let Some(c) = app.changes.get(i).cloned() else {
                        if key == Key::A {
                            acts.push(Action::StageAll);
                        }
                        continue;
                    };
                    match key {
                        Key::Space => acts.push(if c.staged() && !c.unstaged() {
                            Action::Unstage(c.path.clone())
                        } else {
                            Action::Stage(c.path.clone())
                        }),
                        Key::A => {
                            acts.push(if app.changes.iter().all(|c| c.staged() && !c.unstaged()) {
                                Action::UnstageAll
                            } else {
                                Action::StageAll
                            })
                        }
                        Key::D => acts.push(Action::Discard(c)),
                        Key::E => acts.push(Action::OpenFile(c.path.clone(), None)),
                        _ => {}
                    }
                }
                Panel::Branches if key == Key::Space => {
                    let i = app.tabs[cur].git.sel[2];
                    if let Some(b) = app.branches.get(i) {
                        acts.push(Action::Checkout(b.name.clone()));
                    }
                }
                Panel::Snapshots => {
                    let i = app.tabs[cur].git.sel[4];
                    let t = &app.tabs[cur];
                    match key {
                        Key::R => {
                            if let Some(s) = t.snaps.get(i).filter(|_| i > 0) {
                                acts.push(Action::Revert(t.id.clone(), s.clone()));
                            }
                        }
                        Key::N => acts.push(Action::Checkpoint(t.id.clone())),
                        _ => {}
                    }
                }
                _ => {}
            },
        }
    }
}

/// Keys while the diff side has focus: move, jump hunks, stage hunks, edit.
fn main_key(app: &mut App, key: Key, m: egui::Modifiers, acts: &mut Vec<Action>) {
    let cur = app.cur;
    let mode = match &app.tabs[cur].git.main {
        Some(MainSrc::Changes { mode, .. }) => Some(mode.clone()),
        _ => None,
    };
    let d = &mut app.tabs[cur].git.diff;
    let n = d.parsed.lines.len();
    if n == 0 {
        return;
    }
    let line = d.parsed.lines.get(d.cursor).cloned();
    let hunk = line
        .as_ref()
        .and_then(|l| l.hunk)
        .and_then(|h| d.parsed.hunks.get(h))
        .cloned();
    let starts: Vec<usize> = d
        .parsed
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind == LineKind::Hunk)
        .map(|(i, _)| i)
        .collect();
    let before = d.cursor;
    match key {
        Key::J | Key::ArrowDown => d.cursor = (d.cursor + 1).min(n - 1),
        Key::K | Key::ArrowUp => d.cursor = d.cursor.saturating_sub(1),
        Key::PageDown | Key::Space => d.cursor = (d.cursor + 25).min(n - 1),
        Key::PageUp => d.cursor = d.cursor.saturating_sub(25),
        Key::N | Key::CloseBracket => {
            if let Some(&h) = starts.iter().find(|&&h| h > d.cursor) {
                d.cursor = h;
            }
        }
        Key::P | Key::OpenBracket => {
            if let Some(&h) = starts.iter().rev().find(|&&h| h < d.cursor) {
                d.cursor = h;
            }
        }
        Key::S => {
            if m.shift {
                if let Some(f) = line.and_then(|l| l.file) {
                    acts.push(Action::StageFileFromDiff(f));
                }
            } else if let (Some(DiffMode::Unstaged), Some(h)) = (mode, hunk) {
                acts.push(Action::StageHunk(h));
            }
        }
        Key::U => {
            if m.shift {
                if let Some(f) = line.and_then(|l| l.file) {
                    acts.push(Action::UnstageFileFromDiff(f));
                }
            } else if let (Some(DiffMode::Staged), Some(h)) = (mode, hunk) {
                acts.push(Action::UnstageHunk(h));
            }
        }
        Key::R => acts.push(Action::Remark),
        Key::D => {
            if let (Some(DiffMode::Unstaged | DiffMode::Base(_)), Some(h)) = (mode, hunk) {
                acts.push(Action::DiscardHunk(h));
            }
        }
        Key::E | Key::Enter => {
            if let Some(l) = line {
                if let Some(f) = l.file {
                    acts.push(Action::OpenFile(f, Some(l.new_line.unwrap_or(1))));
                }
            }
        }
        _ => {}
    }
    if d.cursor != before {
        d.scroll_to_cursor = true;
    }
}
