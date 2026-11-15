//! Everything that draws. Interactions are collected as `Action`s and applied
//! after drawing, so drawing code never fights the borrow checker over `App`.

use std::path::PathBuf;

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, Event, FontId, Frame, Id, Key, Layout,
    Margin, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};
use muster_core::git::{self, Change, DiffMode, Hunk, LineKind};
use muster_core::proto::{ApprovalSource, SnapshotInfo, TaskInfo, TaskStatus};
use muster_core::{now_unix, paths};

mod gitview;

use crate::app::{
    agent_kind, is_agent, item_project, project_item, task_dir, App, Child, CmdBar, Confirm,
    MainSrc, SettingsUi, Target,
};
use crate::settings::{self as cfg, CursorStyle};
use crate::{term, termview, theme};

// Shared with keel: warm graphite with a sea-teal accent (see theme::c).
pub const ACCENT: Color32 = theme::c::ACCENT;
pub const GREEN: Color32 = theme::c::SUCCESS;
pub const AMBER: Color32 = theme::c::WARNING;
pub const RED: Color32 = theme::c::DANGER;
pub const MUTED: Color32 = theme::c::TEXT_DIM;
pub const FAINT: Color32 = theme::c::TEXT_FAINT;
pub const TEXT: Color32 = theme::c::TEXT;
pub const PANEL: Color32 = theme::c::SIDEBAR;
pub const PANEL_2: Color32 = theme::c::CARD;
pub const SEL: Color32 = theme::c::CARD_HOVER;

pub fn style(ctx: &egui::Context) {
    theme::apply_style(ctx);
}

enum Action {
    OpenTerm(String),
    OpenFile(String, Option<usize>),
    OpenDiff(Option<String>),
    ShowMain(MainSrc),
    Show(Target),
    StartRename(String),
    StartRenameTab(String),
    ToggleChild(Child),
    Rename(String, String),
    SetProject(String),
    OpenBar(bool),
    CloseSide(usize),
    CloseTerminal(String),
    CloseFile(String, PathBuf),
    FocusPane(usize),
    Split,
    NewShell,
    NewShellIn(String),
    FoldProject(String),
    ToggleAgentSort,
    /// A terminal for an agent, beside it.
    NewCompanion(String),
    OpenBeside(String),
    NewAgent,
    NewProject,
    StartAgent(String),
    Kill(String),
    Remove(String),
    Merge,
    Stage(String),
    Unstage(String),
    StageAll,
    UnstageAll,
    StartCommit,
    Commit,
    Discard(Change),
    Checkout(String),
    Decide(u64, bool),
    NextAttention,
    Revert(String, SnapshotInfo),
    Checkpoint(String),
    StageHunk(Hunk),
    UnstageHunk(Hunk),
    StageFileFromDiff(String),
    UnstageFileFromDiff(String),
    Input(String, Vec<u8>),
    /// Keyboard input or a paste: goes to every terminal card while broadcasting.
    Typed(String, Vec<u8>),
    Paste(String, String),
    RunSnippet(usize),
    /// Send a prompt choice ("1", Enter, Esc) to an agent.
    Answer(String, Vec<u8>),
    Resize(String, u16, u16),
    SwapCards(usize, usize),
    /// A tab dropped on another tab: tile the first beside the second.
    TileOnTab(String, String),
    /// A tab dropped on a card: tile it beside that card.
    TileOnCard(String, usize),
    ToggleZoom,
    ToggleBroadcast,
    Copy(String),
    External(PathBuf, usize),
    ToggleDir(String),
    Settings,
    Shortcuts,
}

fn status_mark(t: &TaskInfo, frame: usize) -> (&'static str, Color32) {
    if t.pending_approvals > 0 {
        return ("!", AMBER);
    }
    if !is_agent(t) && !matches!(t.status, TaskStatus::Exited { .. }) {
        return ("❯", MUTED);
    }
    match t.status {
        TaskStatus::Running => (if frame % 4 < 2 { "●" } else { "○" }, GREEN),
        TaskStatus::Waiting { .. } => ("◆", AMBER),
        TaskStatus::Idle if t.attention => ("●", ACCENT),
        TaskStatus::Idle => ("○", MUTED),
        TaskStatus::Exited { .. } => ("✕", RED),
    }
}

fn ago(unix: i64) -> String {
    let s = (now_unix() - unix).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn section_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(MUTED));
}

/// App-level shortcut: Cmd+key on macOS, Ctrl+Shift+key elsewhere. Consumes the event.
/// Consume a key press with exactly these modifiers. Shared conventions with
/// keel: app commands are ⌘⇧ on macOS and Ctrl+Shift elsewhere (`chord`), so
/// plain ⌘ keys and every Ctrl key stay with the terminal; only a few plain ⌘
/// keys are the app's (`cmd`).
fn press(ctx: &egui::Context, key: Key, shift: bool) -> bool {
    let mac = cfg!(target_os = "macos");
    ctx.input_mut(|i| {
        let pos = i.events.iter().position(|e| match e {
            Event::Key {
                key: k,
                pressed: true,
                modifiers: m,
                ..
            } if *k == key => {
                let primary = if mac { m.mac_cmd && !m.ctrl } else { m.ctrl };
                primary && m.shift == shift && !m.alt
            }
            _ => false,
        });
        if let Some(p) = pos {
            i.events.remove(p);
            true
        } else {
            false
        }
    })
}

fn chord(ctx: &egui::Context, key: Key) -> bool {
    press(ctx, key, true)
}

fn cmd(ctx: &egui::Context, key: Key) -> bool {
    press(ctx, key, false)
}

/// Label prefix for a `chord`.
pub fn mod_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘⇧"
    } else {
        "Ctrl+Shift+"
    }
}

/// Label prefix for a `cmd` key.
pub fn cmd_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

// ── top-level ───────────────────────────────────────────────────────────

pub fn draw(ui: &mut Ui, app: &mut App) {
    let ctx = ui.ctx().clone();
    let mut acts: Vec<Action> = vec![];
    // MUSTER_KEYLOG=1 prints every key as egui sees it, for when a shortcut
    // seems not to arrive (another app may hold it system-wide).
    if std::env::var_os("MUSTER_KEYLOG").is_some() {
        ctx.input(|i| {
            for e in &i.events {
                if let Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = e
                {
                    eprintln!("KEYLOG {key:?} {modifiers:?}");
                }
            }
        });
    }

    shortcuts(&ctx, app, &mut acts);

    let need = app
        .tasks
        .values()
        .filter(|t| t.attention && is_agent(t))
        .count()
        + app.approvals.len();
    let here = project_title(app, app.project.as_deref().unwrap_or(""));
    let title = if need > 0 {
        format!("muster — {here} · {need} need you")
    } else {
        format!("muster — {here}")
    };
    if title != app.title {
        app.title = title.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    let scheme_bg = theme::scheme(&app.settings.scheme).bg;
    let accent = project_color(app.project.as_deref().unwrap_or(""));
    let ground = theme::surface(theme::c::SIDEBAR, scheme_bg);
    if app.sidebar {
        let tint = theme::surface(blend(PANEL, accent, 0.07), scheme_bg);
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(280.0)
            .size_range(220.0..=480.0)
            .frame(
                Frame::new()
                    .fill(tint)
                    .inner_margin(Margin::symmetric(8, 8)),
            )
            .show(ui, |ui| sidebar(ui, app, &mut acts));
    }
    egui::CentralPanel::default()
        .frame(Frame::new().fill(ground).inner_margin(Margin::same(8)))
        .show(ui, |ui| stage(ui, app, accent, &mut acts));
    overlays(&ctx, app, &mut acts);
    dialogs(&ctx, app, &mut acts);
    command_bar(&ctx, app, &mut acts);
    shortcuts_sheet(&ctx, app);
    settings_window(&ctx, app);

    for a in acts {
        apply(app, a);
    }
}

/// Mix `b` into `a` by `t` (0..1).
fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// A project's name: the one you gave it, or its folder's.
fn project_title(app: &App, root: &str) -> String {
    app.settings
        .project_names
        .get(root)
        .cloned()
        .unwrap_or_else(|| project_name(root))
}

fn project_name(root: &str) -> String {
    if root.is_empty() {
        return "other".into();
    }
    if std::env::var_os("HOME")
        .is_some_and(|h| std::path::Path::new(root) == std::path::Path::new(&h))
    {
        return "~".into();
    }
    std::path::Path::new(root)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string())
}

/// A stable colour per project, so each one is recognisable at a glance.
fn project_color(root: &str) -> Color32 {
    const COLORS: [Color32; 6] = [
        Color32::from_rgb(0x7a, 0xa2, 0xf7),
        Color32::from_rgb(0x9e, 0xce, 0x6a),
        Color32::from_rgb(0xe0, 0xaf, 0x68),
        Color32::from_rgb(0xbb, 0x9a, 0xf7),
        Color32::from_rgb(0x7d, 0xcf, 0xff),
        Color32::from_rgb(0xf7, 0x76, 0x8e),
    ];
    if root.is_empty() {
        return MUTED;
    }
    let h = root
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    COLORS[h as usize % COLORS.len()]
}

fn shortcuts(ctx: &egui::Context, app: &mut App, acts: &mut Vec<Action>) {
    use crate::tiling::{Dir, TileCmd};
    for c in crate::tiling::take_cmds(ctx, app.settings.tile_mod) {
        let dir = match c {
            TileCmd::Focus(d) | TileCmd::Swap(d) => Some(d),
            _ => None,
        };
        let (dx, dy) = match dir {
            Some(Dir::Left) => (-1.0, 0.0),
            Some(Dir::Right) => (1.0, 0.0),
            Some(Dir::Up) => (0.0, -1.0),
            Some(Dir::Down) => (0.0, 1.0),
            None => (0.0, 0.0),
        };
        match c {
            TileCmd::Focus(_) => {
                if let Some(i) = app.neighbour(dx, dy) {
                    acts.push(Action::FocusPane(i));
                }
            }
            TileCmd::Swap(_) => {
                if let Some(i) = app.neighbour(dx, dy) {
                    app.swap_card(i);
                }
            }
            TileCmd::NewPane => app.new_terminal_card(),
            TileCmd::ClosePane => acts.push(Action::CloseSide(app.active)),
            TileCmd::Maximize => {
                if app.split() {
                    app.zoom = !app.zoom;
                }
            }
        }
    }
    // ⌘P too on macOS (VS Code's quick open); Ctrl+P elsewhere is the
    // shell's previous command.
    if chord(ctx, Key::P) || (cfg!(target_os = "macos") && cmd(ctx, Key::P)) {
        acts.push(Action::OpenBar(false));
    }
    if chord(ctx, Key::T) {
        acts.push(new_terminal_action(app));
    }
    if chord(ctx, Key::N) {
        acts.push(Action::NewAgent);
    }
    if chord(ctx, Key::W) {
        if app.split() {
            acts.push(Action::CloseSide(app.active));
        } else if let Some(Target {
            item,
            child: Child::File(p),
        }) = app.target().cloned()
        {
            acts.push(Action::CloseFile(item, p));
        }
    }
    if chord(ctx, Key::D) {
        acts.push(Action::Split);
    }
    if chord(ctx, Key::E) {
        app.sidebar = !app.sidebar;
    }
    if chord(ctx, Key::G) {
        app.toggle_child(Child::Changes);
    }
    if chord(ctx, Key::O) {
        app.toggle_child(Child::Files);
    }
    if chord(ctx, Key::U) {
        app.show_git();
        if let Some(it) = app.item() {
            it.git.panel = crate::app::Panel::Snapshots;
        }
    }
    if chord(ctx, Key::A) {
        acts.push(Action::NextAttention);
    }
    if chord(ctx, Key::Y) {
        app.decide_first(true);
    }
    if chord(ctx, Key::R) {
        app.decide_first(false);
    }
    if chord(ctx, Key::B) {
        app.broadcast = !app.broadcast;
        app.notify(
            if app.broadcast {
                "broadcasting: typing goes to every terminal card"
            } else {
                "broadcast off"
            },
            false,
        );
    }
    if chord(ctx, Key::Slash) {
        app.show_keys = !app.show_keys;
    }
    if cmd(ctx, Key::Comma) {
        toggle_settings(app);
    }
    // ⌘= also arrives as ⌘⇧= (⌘+) on most layouts.
    if cmd(ctx, Key::Equals)
        || cmd(ctx, Key::Plus)
        || chord(ctx, Key::Equals)
        || chord(ctx, Key::Plus)
    {
        app.settings.bump_font(1.0);
    }
    if cmd(ctx, Key::Minus) {
        app.settings.bump_font(-1.0);
    }
    if cmd(ctx, Key::Num0) {
        app.settings.font_size = cfg::FONT_SIZE_DEFAULT;
    }
    // ⌘K clears the terminal like Terminal.app and iTerm2. Ctrl+K belongs to
    // the shell elsewhere, so this is macOS only.
    if cfg!(target_os = "macos") && cmd(ctx, Key::K) {
        if let Some(Target {
            item,
            child: Child::Terminal,
        }) = app.target().cloned()
        {
            if let Some(t) = app.terms.get_mut(&item) {
                t.view.clear_scrollback();
            }
            acts.push(Action::Input(item, vec![0x0c]));
        }
    }

    // Next / previous agent or terminal: Ctrl+Tab, or ⌘⇧] / ⌘⇧[ like keel.
    let ctab = ctx.input_mut(|i| {
        let p = i.events.iter().position(|e| {
            matches!(e, Event::Key { key: Key::Tab, pressed: true, modifiers, .. } if modifiers.ctrl)
        });
        p.map(|p| {
            let back = matches!(&i.events[p], Event::Key { modifiers, .. } if modifiers.shift);
            i.events.remove(p);
            back
        })
    });
    let mut step = ctab.map(|back| if back { -1isize } else { 1 });
    if chord(ctx, Key::CloseBracket) {
        step = Some(1);
    }
    if chord(ctx, Key::OpenBracket) {
        step = Some(-1);
    }
    if let Some(d) = step {
        let ids = tab_ids(app);
        if !ids.is_empty() {
            let at = app
                .target()
                .and_then(|t| ids.iter().position(|i| *i == t.item))
                .unwrap_or(0) as isize;
            let next = ids[(at + d).rem_euclid(ids.len() as isize) as usize].clone();
            acts.push(Action::OpenTerm(next));
        }
    }

    // Projects: ⌘1–9, or step with ⌘⇧J / ⌘⇧K like keel's workspaces.
    let roots = project_list(app);
    for (k, n) in [
        (Key::Num1, 0),
        (Key::Num2, 1),
        (Key::Num3, 2),
        (Key::Num4, 3),
        (Key::Num5, 4),
        (Key::Num6, 5),
        (Key::Num7, 6),
        (Key::Num8, 7),
        (Key::Num9, 8),
    ] {
        if cmd(ctx, k) {
            if let Some(r) = roots.get(n) {
                acts.push(Action::SetProject(r.clone()));
            }
        }
    }
    for (key, d) in [(Key::J, -1isize), (Key::K, 1)] {
        if chord(ctx, key) && !roots.is_empty() {
            let here = app.project.clone().unwrap_or_default();
            let at = roots.iter().position(|r| *r == here).unwrap_or(0) as isize;
            let next = roots[(at + d).rem_euclid(roots.len() as isize) as usize].clone();
            acts.push(Action::SetProject(next));
        }
    }
}

/// New terminal: beside the focused agent, in its directory, when an agent
/// is focused (to run what it's working on); a plain one otherwise.
fn new_terminal_action(app: &App) -> Action {
    match app.target() {
        Some(t) if app.tasks.get(&t.item).is_some_and(is_agent) => {
            Action::NewCompanion(t.item.clone())
        }
        Some(t) => match app.parent_of.get(&t.item) {
            // In an agent's terminal: another one for the same agent.
            Some(p) if app.tasks.contains_key(p) => Action::NewCompanion(p.clone()),
            _ => Action::NewShell,
        },
        None => Action::NewShell,
    }
}

fn item_title(app: &App, id: &str) -> String {
    if let Some(root) = item_project(id) {
        return project_title(app, root);
    }
    match app.tasks.get(id) {
        Some(t) => app.label(t),
        None => format!("{id} (ended)"),
    }
}

/// Projects, in a stable order, with the one you launched in first.
fn project_list(app: &App) -> Vec<String> {
    app.project_roots()
}

/// The current project's tasks, most urgent first, agents before terminals.
fn project_ids(app: &App) -> Vec<String> {
    project_tasks(app, app.project.as_deref().unwrap_or(""))
}

fn project_tasks(app: &App, here: &str) -> Vec<String> {
    let mut v: Vec<&TaskInfo> = app
        .tasks
        .values()
        .filter(|t| !crate::editors::is_editor_task(&t.id))
        .filter(|t| app.project_of(t) == here)
        .collect();
    v.sort_by_key(|t| (!is_agent(t), urgency(t), t.created_unix));
    v.into_iter().map(|t| t.id.clone()).collect()
}

fn sidebar(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    egui::Panel::bottom("side_foot")
        .frame(Frame::new().inner_margin(Margin {
            left: 0,
            right: 0,
            top: 6,
            bottom: 0,
        }))
        .show(ui, |ui| side_footer(ui, app, acts));
    egui::Panel::bottom("side_agents")
        .resizable(true)
        .default_size(280.0)
        .size_range(80.0..=700.0)
        .frame(Frame::new().inner_margin(Margin {
            left: 0,
            right: 0,
            top: 6,
            bottom: 4,
        }))
        .show(ui, |ui| agents_section(ui, app, acts));

    egui::CentralPanel::default()
        .frame(Frame::new())
        .show(ui, |ui| {
            let omni = ui.add(
                egui::Button::new(
                    RichText::new(format!("⌕  Search or start a task      {}P", mod_label()))
                        .color(MUTED),
                )
                .min_size(vec2(ui.available_width(), 30.0))
                .fill(Color32::from_white_alpha(12))
                .corner_radius(CornerRadius::same(8)),
            );
            if omni.clicked() {
                acts.push(Action::OpenBar(false));
            }
            if app.daemon_stale() {
                ui.add_space(4.0);
                Frame::new()
                    .fill(Color32::from_rgb(0x2c, 0x25, 0x17))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("The daemon is running an older build of muster. Restart it so terminals follow cd and agents started in them (like claude) show as agents.")
                                .small()
                                .color(AMBER),
                        );
                        if ui.small_button("Restart daemon…").clicked() {
                            app.confirm = Some(Confirm::RestartDaemon {
                                text: "Restart the muster daemon? Every running terminal and agent ends (worktrees and snapshots are kept).".into(),
                            });
                        }
                    });
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                section_label(ui, "PROJECTS");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("+").color(MUTED)).frame(false))
                        .on_hover_text("New project: open a folder")
                        .clicked()
                    {
                        acts.push(Action::NewProject);
                    }
                });
            });
            ScrollArea::vertical()
                .id_salt("side_list")
                .auto_shrink([false, false])
                .show(ui, |ui| side_list(ui, app, acts));
        });
}

/// Every project as a branch of one tree. Each keeps the fold you gave it,
/// so the tree only moves when you move it.
fn side_list(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    let here = app.project.clone().unwrap_or_default();
    for (i, root) in project_list(app).into_iter().enumerate() {
        let current = root == here;
        let open = !app.settings.folded_projects.contains(&root);
        project_header(ui, app, &root, i, current, open, acts);
        if open {
            ui.scope(|ui| {
                ui.spacing_mut().indent = 14.0;
                ui.indent(("project-body", &root), |ui| {
                    project_body(ui, app, &root, acts)
                });
            });
            ui.add_space(6.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn project_header(
    ui: &mut Ui,
    app: &App,
    root: &str,
    index: usize,
    current: bool,
    open: bool,
    acts: &mut Vec<Action>,
) {
    let need = app
        .tasks
        .values()
        .filter(|t| !crate::editors::is_editor_task(&t.id))
        .filter(|t| app.project_of(t) == root && urgency(t) == 0)
        .count();
    let name = project_title(app, root);
    let color = project_color(root);
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let chevron = Rect::from_min_size(rect.min, vec2(20.0, rect.height()));
    let fold = ui.interact(chevron, Id::unique(("project-fold", root)), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    let p = ui.painter();
    if current {
        p.rect_filled(rect, 6.0, color.gamma_multiply(0.16));
        p.rect_filled(
            Rect::from_min_size(
                rect.left_top() + vec2(0.0, 6.0),
                vec2(3.0, rect.height() - 12.0),
            ),
            1.5,
            color,
        );
    } else if hovered {
        p.rect_filled(rect, 6.0, PANEL_2);
    }
    let y = rect.center().y;
    p.text(
        pos2(rect.left() + 10.0, y),
        Align2::LEFT_CENTER,
        if open { "▾" } else { "▸" },
        FontId::proportional(11.0),
        if fold.hovered() { TEXT } else { MUTED },
    );
    p.circle_filled(pos2(rect.left() + 26.0, y), 4.0, color);
    let mut right = rect.right() - 8.0;
    if need > 0 {
        let g = p.text(
            pos2(right, y),
            Align2::RIGHT_CENTER,
            format!("{need} waiting"),
            FontId::proportional(11.5),
            AMBER,
        );
        right = g.left() - 6.0;
    }
    let name_rect = Rect::from_min_max(
        pos2(rect.left() + 36.0, rect.top()),
        pos2(right, rect.bottom()),
    );
    p.with_clip_rect(name_rect).text(
        pos2(name_rect.left(), y),
        Align2::LEFT_CENTER,
        &name,
        FontId::proportional(14.0),
        if current { TEXT } else { MUTED },
    );
    if fold.clicked() {
        acts.push(Action::FoldProject(root.to_string()));
    }
    let r = r.on_hover_text(format!(
        "{}   {}{}",
        if root.is_empty() {
            "Terminals outside any repo".to_string()
        } else {
            tilde(std::path::Path::new(root))
        },
        cmd_label(),
        index + 1
    ));
    if r.clicked() && !fold.clicked() {
        acts.push(Action::SetProject(root.to_string()));
    }
    let root = root.to_string();
    r.context_menu(|ui| {
        if ui.button(format!("New agent   {}N", mod_label())).clicked() {
            acts.push(Action::SetProject(root.clone()));
            acts.push(Action::NewAgent);
            ui.close();
        }
        if ui
            .button(format!("New terminal   {}T", mod_label()))
            .clicked()
        {
            acts.push(Action::SetProject(root.clone()));
            acts.push(Action::NewShellIn(root.clone()));
            ui.close();
        }
        ui.separator();
        if ui.button("Copy path").clicked() {
            acts.push(Action::Copy(root.clone()));
            ui.close();
        }
    });
}

/// A project's terminals, in the order they were opened, the ones running an
/// agent included: starting claude in one doesn't move it out of the tree,
/// it's just listed under AGENTS as well.
fn project_body(ui: &mut Ui, app: &mut App, root: &str, acts: &mut Vec<Action>) {
    let mut shells: Vec<&TaskInfo> = app
        .tasks
        .values()
        .filter(|t| !crate::editors::is_editor_task(&t.id))
        .filter(|t| app.project_of(t) == root)
        .collect();
    shells.sort_by_key(|t| (t.created_unix, t.id.clone()));
    let shells: Vec<String> = shells.into_iter().map(|t| t.id.clone()).collect();
    project_rows(ui, app, root, acts);
    if shells.is_empty() {
        ui.label(RichText::new("No terminals yet.").small().color(MUTED));
    }
    for id in &shells {
        task_rows(ui, app, id, acts);
    }
    let here = app.project.as_deref() == Some(root);
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (label, tip, act) in [
            ("+ Agent", format!("{}N", mod_label()), Action::NewAgent),
            (
                "+ Terminal",
                format!("{}T", mod_label()),
                Action::NewShellIn(root.to_string()),
            ),
        ] {
            let b = egui::Button::new(RichText::new(label).size(12.5).color(MUTED))
                .frame_when_inactive(false)
                .min_size(vec2(0.0, 24.0));
            if ui.add(b).on_hover_text(tip).clicked() {
                if !here {
                    acts.push(Action::SetProject(root.to_string()));
                }
                acts.push(act);
            }
        }
    });
}

/// A project's Changes and Files, and the files opened from them.
fn project_rows(ui: &mut Ui, app: &App, root: &str, acts: &mut Vec<Action>) {
    let item = project_item(root);
    let here = app.project.as_deref() == Some(root);
    // The count is for whatever folder is loaded; it's this project's when
    // that folder's repo is this project.
    let count = (app.ctx_root.as_deref() == Some(std::path::Path::new(root))).then(|| {
        let n = app.changes.len();
        if n == 0 {
            ("clean".to_string(), MUTED)
        } else {
            (format!("{n} file{}", if n == 1 { "" } else { "s" }), AMBER)
        }
    });
    let row = |ui: &mut Ui,
               child: Child,
               icon: &str,
               label: &str,
               right: Option<(String, Color32)>,
               acts: &mut Vec<Action>| {
        let on = is_shown(app, &item, &child);
        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
        let p = ui.painter();
        if on {
            p.rect_filled(rect, 5.0, SEL);
        } else if r.hovered() {
            p.rect_filled(rect, 5.0, PANEL_2);
        }
        let y = rect.center().y;
        p.text(
            pos2(rect.left() + 10.0, y),
            Align2::CENTER_CENTER,
            icon,
            FontId::monospace(12.0),
            MUTED,
        );
        p.with_clip_rect(rect.shrink2(vec2(0.0, 0.0))).text(
            pos2(rect.left() + 24.0, y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            if on { TEXT } else { MUTED },
        );
        if let Some((txt, c)) = right {
            p.text(
                pos2(rect.right() - 6.0, y),
                Align2::RIGHT_CENTER,
                txt,
                FontId::monospace(11.0),
                c,
            );
        }
        if r.clicked() {
            if !here {
                acts.push(Action::SetProject(root.to_string()));
            }
            acts.push(Action::Show(Target {
                item: item.clone(),
                child,
            }));
        }
        r
    };
    row(ui, Child::Changes, "±", "Changes", count, acts);
    row(ui, Child::Files, "▤", "Files", None, acts);
    if let Some(it) = app.tabs.iter().find(|i| i.id == item) {
        for path in &it.opened {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let r = row(ui, Child::File(path.clone()), "·", &name, None, acts)
                .on_hover_text(tilde(path));
            let (item, path) = (item.clone(), path.clone());
            r.context_menu(|ui| {
                if ui.button("Close").clicked() {
                    acts.push(Action::CloseFile(item.clone(), path.clone()));
                    ui.close();
                }
                if ui.button("Copy path").clicked() {
                    acts.push(Action::Copy(path.display().to_string()));
                    ui.close();
                }
            });
        }
    }
}

/// Where a task is, when that's not just its project's folder: a subfolder
/// as a relative path, anywhere else in full.
fn where_in(app: &App, t: &TaskInfo) -> String {
    let dir = task_dir(t);
    let root = app.project_of(t);
    match dir.strip_prefix(&root) {
        Ok(rel) if rel.as_os_str().is_empty() => String::new(),
        Ok(rel) => rel.display().to_string(),
        Err(_) => tilde(&dir),
    }
}

/// The current project's agents and terminals as tabs, oldest first so a tab
/// stays where you left it.
fn tab_ids(app: &App) -> Vec<String> {
    let here = app.project.clone().unwrap_or_default();
    let mut v: Vec<&TaskInfo> = app
        .tasks
        .values()
        .filter(|t| !crate::editors::is_editor_task(&t.id))
        .filter(|t| app.project_of(t) == here)
        .collect();
    v.sort_by_key(|t| (t.created_unix, t.id.clone()));
    v.into_iter().map(|t| t.id.clone()).collect()
}

const TAB_H: f32 = 36.0;

fn tab_strip(ui: &mut Ui, app: &App, strip: Rect, acts: &mut Vec<Action>) {
    let bottom = app.settings.tabs_bottom;
    let edge = if bottom { strip.top() } else { strip.bottom() };
    ui.painter().line_segment(
        [pos2(strip.left(), edge), pos2(strip.right(), edge)],
        Stroke::new(1.0, Color32::from_white_alpha(14)),
    );

    // Changes and Files for whatever the focused card shows, at the far end.
    let target = app.target().cloned();
    let changes = match &target {
        Some(_) if app.ctx_root.is_some() && !app.changes.is_empty() => {
            format!("± Changes  {}", app.changes.len())
        }
        _ => "± Changes".to_string(),
    };
    let mut right = strip.right();
    let mut toggles = vec![];
    for (label, child, key) in [
        ("▤ Files".to_string(), Child::Files, "O"),
        (changes, Child::Changes, "G"),
    ] {
        let w = ui
            .painter()
            .layout_no_wrap(label.clone(), FontId::proportional(12.5), TEXT)
            .size()
            .x
            + ui.spacing().button_padding.x * 2.0
            + 4.0;
        let r = Rect::from_min_max(
            pos2(right - w, strip.center().y - 12.0),
            pos2(right, strip.center().y + 12.0),
        );
        right = r.left() - 4.0;
        toggles.push((r, label, child, key));
    }
    for (r, label, child, key) in toggles {
        let on = target.as_ref().is_some_and(|t| t.child == child);
        let b = ui.put(
            r,
            egui::Button::selectable(on, RichText::new(label).size(12.5))
                .wrap_mode(egui::TextWrapMode::Extend),
        );
        if b.on_hover_text(format!("{}{key}", mod_label())).clicked() && target.is_some() {
            acts.push(Action::ToggleChild(child));
        }
    }

    let area = Rect::from_min_max(strip.min, pos2(right - 8.0, strip.max.y));
    let mut tabs = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("tab-strip")
            .max_rect(area)
            .layout(Layout::left_to_right(Align::Center)),
    );
    ScrollArea::horizontal()
        .id_salt("tabs")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut tabs, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // keel's 180px tabs, narrowed to keep them and the + in view
            // until they'd get too small to read.
            let ids = tab_ids(app);
            let room = area.width() - 36.0 - 2.0 * ids.len() as f32;
            let width = (room / ids.len().max(1) as f32).clamp(110.0, 180.0);
            for id in ids {
                tab(ui, app, &id, width, acts);
            }
            let root = app.project.clone().unwrap_or_default();
            if ui
                .add(
                    egui::Button::new(RichText::new("+").size(15.0).color(MUTED))
                        .frame_when_inactive(false)
                        .min_size(vec2(28.0, TAB_H - 6.0)),
                )
                .on_hover_text(format!("New terminal   {}T", mod_label()))
                .clicked()
            {
                acts.push(Action::NewShellIn(root));
            }
        });
}

/// A tab drawn as keel draws its session tabs: fixed width, a status dot,
/// no box, and an accent underline on the one you're in.
fn tab(ui: &mut Ui, app: &App, id: &str, width: f32, acts: &mut Vec<Action>) {
    use theme::c;
    let t = &app.tasks[id];
    let label = app.label(t);
    let shell = t.agent == "shell";
    let active = app.target().is_some_and(|c| c.item == id);
    let (rect, _) = ui.allocate_exact_size(vec2(width, ui.available_height()), Sense::hover());
    let r = ui.interact(rect, Id::unique(("tab", id)), Sense::click_and_drag());
    let close =
        Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(20.0, 20.0));
    let cr = shell.then(|| ui.interact(close, Id::unique(("tab_close", id)), Sense::click()));
    // Drag a tab onto another tab, or onto a card, to tile it there.
    r.dnd_set_drag_payload(TabDrag(id.to_string()));
    if r.dragged() {
        drag_ghost(ui.ctx(), &label);
    }
    let dropping = r.dnd_hover_payload::<TabDrag>().is_some_and(|p| p.0 != id);
    if let Some(p) = r.dnd_release_payload::<TabDrag>() {
        if p.0 != id {
            acts.push(Action::TileOnTab(p.0.clone(), id.to_string()));
        }
    }
    let close_hovered = cr.as_ref().is_some_and(|c| c.hovered());
    let hovered = r.hovered() || close_hovered;
    let p = ui.painter();
    let body = rect.shrink2(vec2(0.0, 6.0));
    if hovered && !active {
        p.rect_filled(
            body,
            3.0,
            theme::surface(c::CARD, theme::scheme(&app.settings.scheme).bg),
        );
    }
    if dropping {
        p.rect_stroke(
            body,
            3.0,
            Stroke::new(2.0, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    if active {
        // Under the tab, or over it when the strip runs along the bottom.
        let y = if app.settings.tabs_bottom {
            rect.top()
        } else {
            rect.bottom() - 2.0
        };
        // In the project's colour, like its marker in the sidebar.
        p.rect_filled(
            Rect::from_min_max(
                pos2(rect.left() + 6.0, y),
                pos2(rect.right() - 6.0, y + 2.0),
            ),
            0.0,
            project_color(app.project.as_deref().unwrap_or("")),
        );
    }
    p.circle_filled(
        pos2(rect.left() + 14.0, rect.center().y),
        4.0,
        tab_dot(t, app.frame),
    );
    let renaming = app.rename_in_tab && app.renaming.as_ref().is_some_and(|(rid, _)| rid == id);
    // The name box takes the close mark's room while it's open.
    let text_end = if shell && !renaming {
        close.left() - 4.0
    } else {
        rect.right() - 8.0
    };
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 26.0, rect.top()),
        pos2(text_end, rect.bottom()),
    );
    if renaming {
        let mut text = app
            .renaming
            .as_ref()
            .map(|r| r.1.clone())
            .unwrap_or_default();
        let e = ui.put(
            text_rect.shrink2(vec2(0.0, 7.0)),
            TextEdit::singleline(&mut text)
                .hint_text(&label)
                .desired_width(text_rect.width() - 8.0),
        );
        e.request_focus();
        let (enter, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if enter {
            acts.push(Action::Rename(id.to_string(), text));
        } else if esc {
            acts.push(Action::Rename(id.to_string(), label.clone()));
        } else if e.changed() {
            acts.push(Action::StartRename(format!("{id}\u{0}{text}")));
        } else if e.lost_focus() {
            acts.push(Action::Rename(id.to_string(), text));
        }
    } else {
        ui.painter().with_clip_rect(text_rect).text(
            pos2(text_rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            &label,
            FontId::proportional(13.5),
            if active { c::TEXT } else { c::TEXT_DIM },
        );
    }
    if shell && (hovered || active) && !renaming {
        if close_hovered {
            ui.painter().rect_filled(close, 3.0, c::BORDER);
        }
        ui.painter().text(
            close.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(11.0),
            c::TEXT_DIM,
        );
    }
    if cr.is_some_and(|c| c.on_hover_text("Close terminal").clicked())
        || (shell && r.middle_clicked())
    {
        acts.push(Action::CloseTerminal(id.to_string()));
    }
    let r = r.on_hover_text(match agent_kind(t) {
        Some(kind) => format!("{label} · {kind} · {}", tilde(&task_dir(t))),
        None => format!("{label} · {}", tilde(&task_dir(t))),
    });
    if r.double_clicked() {
        acts.push(Action::StartRenameTab(id.to_string()));
    } else if r.clicked() {
        acts.push(Action::OpenTerm(id.to_string()));
    }
    let tid = id.to_string();
    r.context_menu(|ui| {
        if ui.button("Rename…").clicked() {
            acts.push(Action::StartRenameTab(tid.clone()));
            ui.close();
        }
        if ui.button("Open beside").clicked() {
            acts.push(Action::OpenBeside(tid.clone()));
            ui.close();
        }
        if shell {
            ui.separator();
            if ui.button("Close terminal").clicked() {
                acts.push(Action::CloseTerminal(tid.clone()));
                ui.close();
            }
        }
    });
}

/// The tab's dot: keel's green for a live terminal, an agent's state colour,
/// red once it has exited.
fn tab_dot(t: &TaskInfo, frame: usize) -> Color32 {
    if matches!(t.status, TaskStatus::Exited { .. }) {
        return RED;
    }
    if is_agent(t) {
        return status_mark(t, frame).1;
    }
    theme::c::SUCCESS
}

/// Every project's agents, one row each as in herdr: status, `project · name`
/// and what the agent says it's working on. Clicking one goes to its project
/// and shows it there.
fn agents_section(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    ui.painter().line_segment(
        [ui.max_rect().left_top(), ui.max_rect().right_top()],
        Stroke::new(1.0, Color32::from_white_alpha(10)),
    );
    ui.add_space(4.0);
    let mut agents: Vec<&TaskInfo> = app
        .tasks
        .values()
        .filter(|t| !crate::editors::is_editor_task(&t.id))
        .filter(|t| is_agent(t))
        .collect();
    if app.settings.agents_by_recent {
        agents.sort_by_key(|t| std::cmp::Reverse(t.last_activity_unix));
    } else {
        agents.sort_by_key(|t| (urgency(t), std::cmp::Reverse(t.last_activity_unix)));
    }
    let ids: Vec<String> = agents.into_iter().map(|t| t.id.clone()).collect();
    let here = project_title(app, app.project.as_deref().unwrap_or(""));
    ui.horizontal(|ui| {
        section_label(ui, "AGENTS");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(egui::Button::new(RichText::new("+").color(MUTED)).frame(false))
                .on_hover_text(format!("New agent in {here}   {}N", mod_label()))
                .clicked()
            {
                acts.push(Action::NewAgent);
            }
            let by = if app.settings.agents_by_recent {
                "recent"
            } else {
                "priority"
            };
            if ui
                .add(egui::Button::new(RichText::new(by).small().color(MUTED)).frame(false))
                .on_hover_text("Sort by what needs you, or by latest activity")
                .clicked()
            {
                acts.push(Action::ToggleAgentSort);
            }
        });
    });
    ScrollArea::vertical()
        .id_salt("side_agents")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if ids.is_empty() {
                ui.label(
                    RichText::new("No agents running. + starts one in this project.")
                        .small()
                        .color(MUTED),
                );
            }
            for id in &ids {
                agent_row(ui, app, id, acts);
            }
        });
}

fn agent_row(ui: &mut Ui, app: &App, id: &str, acts: &mut Vec<Action>) {
    let t = &app.tasks[id];
    let (mark, color) = status_mark(t, app.frame);
    let on = app.target().is_some_and(|c| c.item == id);
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 6.0, SEL);
    } else if r.hovered() {
        p.rect_filled(rect, 6.0, PANEL_2);
    }
    let top = rect.top() + 12.0;
    let bottom = rect.bottom() - 11.0;
    p.text(
        pos2(rect.left() + 12.0, top),
        Align2::CENTER_CENTER,
        mark,
        FontId::monospace(12.0),
        color,
    );
    let text = Rect::from_min_max(
        pos2(rect.left() + 24.0, rect.top()),
        pos2(rect.right() - 6.0, rect.bottom()),
    );
    let clip = p.with_clip_rect(text);
    let mut line = egui::text::LayoutJob::default();
    line.append(
        &project_title(app, &app.project_of(t)),
        0.0,
        egui::TextFormat::simple(FontId::proportional(13.0), if on { TEXT } else { MUTED }),
    );
    line.append(
        &format!(" · {}", app.label(t)),
        0.0,
        egui::TextFormat::simple(FontId::proportional(13.0), if on { TEXT } else { MUTED }),
    );
    let galley = ui.fonts_mut(|f| f.layout_job(line));
    clip.galley(pos2(text.left(), top - galley.size().y / 2.0), galley, TEXT);
    let what = match &t.status {
        TaskStatus::Waiting { reason } => Some((reason.clone(), AMBER)),
        _ if t.pending_approvals > 0 => Some(("wants to run a command".into(), AMBER)),
        _ => t.title.clone().map(|s| (s, FAINT)),
    };
    if let Some((what, c)) = what {
        clip.text(
            pos2(text.left(), bottom),
            Align2::LEFT_CENTER,
            what,
            FontId::proportional(12.0),
            c,
        );
    }
    if r.clicked() {
        acts.push(Action::OpenTerm(id.to_string()));
    }
    let tid = id.to_string();
    r.context_menu(|ui| {
        if ui.button("Rename…").clicked() {
            acts.push(Action::OpenTerm(tid.clone()));
            acts.push(Action::StartRenameTab(tid.clone()));
            ui.close();
        }
        if ui.button("Open beside").clicked() {
            acts.push(Action::OpenBeside(tid.clone()));
            ui.close();
        }
        if ui.button("Changes").clicked() {
            acts.push(Action::OpenTerm(tid.clone()));
            acts.push(Action::Show(Target {
                item: tid.clone(),
                child: Child::Changes,
            }));
            ui.close();
        }
        if t.branch.is_some() && ui.button("Merge branch…").clicked() {
            acts.push(Action::OpenTerm(tid.clone()));
            acts.push(Action::Merge);
            ui.close();
        }
        ui.separator();
        if t.agent == "shell" {
            if ui.button("Close terminal").clicked() {
                acts.push(Action::CloseTerminal(tid.clone()));
                ui.close();
            }
        } else {
            if ui.button("Kill process").clicked() {
                acts.push(Action::Kill(tid.clone()));
                ui.close();
            }
            if ui.button(RichText::new("Remove…").color(RED)).clicked() {
                acts.push(Action::Remove(tid.clone()));
                ui.close();
            }
        }
    });
}

/// How urgent a task is, herdr-style: lower sorts first.
fn urgency(t: &TaskInfo) -> u8 {
    match &t.status {
        _ if t.pending_approvals > 0 => 0,
        TaskStatus::Waiting { .. } => 0,
        TaskStatus::Idle if t.attention => 1,
        TaskStatus::Running if is_agent(t) => 2,
        TaskStatus::Exited { .. } => 4,
        _ => 3,
    }
}

fn state_label(t: &TaskInfo) -> (&'static str, Color32) {
    match &t.status {
        _ if t.pending_approvals > 0 => ("approve", AMBER),
        TaskStatus::Waiting { .. } => ("blocked", AMBER),
        TaskStatus::Running => ("working", GREEN),
        TaskStatus::Idle if t.attention => ("done", ACCENT),
        TaskStatus::Exited { .. } => ("exited", MUTED),
        _ => ("idle", MUTED),
    }
}

fn is_shown(app: &App, item: &str, child: &Child) -> bool {
    app.cards
        .iter()
        .any(|t| t.item == item && t.child == *child)
}

/// An agent or terminal row, and its Changes / Files / open file rows.
fn task_rows(ui: &mut Ui, app: &App, id: &str, acts: &mut Vec<Action>) {
    let t = &app.tasks[id];
    let shell = t.agent == "shell";
    let (mark, color) = status_mark(t, app.frame);
    let (state, scolor) = state_label(t);
    let on = is_shown(app, id, &Child::Terminal);
    let urgent = urgency(t) == 0;
    let mut toggled = false;
    let resp = Frame::new()
        .fill(if on { SEL } else { Color32::TRANSPARENT })
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(4, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(mark).color(color).monospace());
                match &app.renaming {
                    Some((rid, text)) if rid == id && !app.rename_in_tab => {
                        let mut text = text.clone();
                        let r = ui.add(
                            TextEdit::singleline(&mut text)
                                .desired_width(ui.available_width() - 60.0),
                        );
                        r.request_focus();
                        let (enter, esc) =
                            ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                        if enter {
                            acts.push(Action::Rename(id.to_string(), text));
                        } else if esc {
                            acts.push(Action::Rename(id.to_string(), app.label(t)));
                        } else if r.changed() {
                            acts.push(Action::StartRename(format!("{id}\u{0}{text}")));
                        } else if r.lost_focus() {
                            acts.push(Action::Rename(id.to_string(), text));
                        }
                    }
                    _ => {
                        let name = RichText::new(app.label(t));
                        let r = ui
                            .add(
                                egui::Label::new(if urgent {
                                    name.strong().color(TEXT)
                                } else {
                                    name.color(TEXT)
                                })
                                .truncate()
                                .sense(Sense::click()),
                            )
                            .on_hover_text("Double-click to rename");
                        if r.double_clicked() {
                            toggled = true;
                            acts.push(Action::StartRename(id.to_string()));
                        } else if r.clicked() {
                            toggled = true;
                            acts.push(Action::OpenTerm(id.to_string()));
                        }
                    }
                }
                if is_agent(t) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(state).small().color(scolor));
                    });
                }
            });
            let sub = match &t.status {
                TaskStatus::Waiting { reason } => reason.clone(),
                _ if t.pending_approvals > 0 => "wants to run a command".into(),
                // An agent typed into a terminal: say which, and where.
                _ if shell => match agent_kind(t) {
                    Some(kind) => [kind.to_string(), where_in(app, t)]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · "),
                    None => where_in(app, t),
                },
                _ => t.branch.clone().unwrap_or_else(|| where_in(app, t)),
            };
            if !sub.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(sub).small().color(if urgent {
                        AMBER
                    } else {
                        MUTED
                    }))
                    .truncate(),
                );
            }
        })
        .response
        .interact(Sense::click());
    if resp.clicked() && !toggled {
        acts.push(Action::OpenTerm(id.to_string()));
    }
    let tid = id.to_string();
    if resp.double_clicked() {
        acts.push(Action::StartRename(tid.clone()));
    }
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            acts.push(Action::OpenTerm(tid.clone()));
            ui.close();
        }
        if ui.button("Rename…").clicked() {
            acts.push(Action::StartRename(tid.clone()));
            ui.close();
        }
        if ui.button("Changes").clicked() {
            acts.push(Action::Show(Target {
                item: tid.clone(),
                child: Child::Changes,
            }));
            ui.close();
        }
        if t.branch.is_some() && ui.button("Merge branch…").clicked() {
            acts.push(Action::OpenTerm(tid.clone()));
            acts.push(Action::Merge);
            ui.close();
        }
        ui.separator();
        if shell {
            if ui.button("Close terminal").clicked() {
                acts.push(Action::CloseTerminal(tid.clone()));
                ui.close();
            }
        } else {
            if ui.button("Kill process").clicked() {
                acts.push(Action::Kill(tid.clone()));
                ui.close();
            }
            if ui.button(RichText::new("Remove…").color(RED)).clicked() {
                acts.push(Action::Remove(tid.clone()));
                ui.close();
            }
        }
    });

    // Answer a numbered prompt from here, as in keel's agent list.
    if !t.choices.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(20.0);
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for (k, label) in &t.choices {
                let short: String = label.chars().take(34).collect();
                let short = if short.len() < label.len() {
                    format!("{short}…")
                } else {
                    short
                };
                if ui
                    .small_button(format!("{k}  {short}"))
                    .on_hover_text(label)
                    .clicked()
                {
                    acts.push(Action::Answer(id.to_string(), k.as_bytes().to_vec()));
                }
            }
            if ui.small_button("Enter").clicked() {
                acts.push(Action::Answer(id.to_string(), b"\r".to_vec()));
            }
            if ui.small_button("Esc").clicked() {
                acts.push(Action::Answer(id.to_string(), b"\x1b".to_vec()));
            }
        });
    }
}

/// Settings, shortcuts and the daemon's state along the foot of the sidebar.
fn side_footer(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    ui.painter().line_segment(
        [ui.max_rect().left_top(), ui.max_rect().right_top()],
        Stroke::new(1.0, Color32::from_white_alpha(10)),
    );
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(RichText::new("⚙ Settings").color(MUTED)).frame(false))
            .on_hover_text(format!("{},", cmd_label()))
            .clicked()
        {
            acts.push(Action::Settings);
        }
        if ui
            .add(egui::Button::new(RichText::new("? Shortcuts").color(MUTED)).frame(false))
            .on_hover_text(format!("{}/", mod_label()))
            .clicked()
        {
            acts.push(Action::Shortcuts);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (dot, label) = if app.client.is_some() {
                (GREEN, "daemon")
            } else {
                (RED, "connecting…")
            };
            ui.label(RichText::new(label).small().color(MUTED))
                .on_hover_text(
                    "The daemon keeps agents and terminals running while this window is closed",
                );
            ui.label(RichText::new("●").small().color(dot));
        });
    });
}

fn files(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    let ctx = app.ctx_dir();
    let name = ctx
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    ui.horizontal(|ui| {
        section_label(ui, "FILES");
        ui.add(egui::Label::new(RichText::new(&name).small().color(MUTED)).truncate())
            .on_hover_text(ctx.display().to_string());
    });
    let row_h = 18.0;
    let n = app.tree.rows.len();
    ScrollArea::vertical()
        .id_salt("files")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, n, |ui, range| {
            let visible = app.tree.rows[range].to_vec();
            for r in &visible {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, PANEL_2);
                }
                let x = rect.left() + 4.0 + r.depth as f32 * 12.0;
                let (label, color) = if r.is_dir {
                    (
                        format!("{} {}", if r.expanded { "▾" } else { "▸" }, r.name),
                        ACCENT,
                    )
                } else {
                    (format!("  {}", r.name), TEXT)
                };
                ui.painter().text(
                    pos2(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(13.0),
                    color,
                );
                let mark = if r.is_dir {
                    (app.marks.dirs.contains(&r.rel) || app.marks.files.contains_key(&r.rel))
                        .then_some(('•', AMBER))
                } else {
                    app.marks.files.get(&r.rel).map(|c| {
                        (
                            *c,
                            match c {
                                'M' => AMBER,
                                'A' | 'U' => GREEN,
                                'D' => RED,
                                _ => MUTED,
                            },
                        )
                    })
                };
                if let Some((m, c)) = mark {
                    ui.painter().text(
                        pos2(rect.right() - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        m.to_string(),
                        FontId::monospace(12.0),
                        c,
                    );
                }
                if resp.clicked() {
                    if r.is_dir {
                        acts.push(Action::ToggleDir(r.rel.clone()));
                    } else {
                        acts.push(Action::OpenFile(r.rel.clone(), None));
                    }
                }
                if !r.is_dir {
                    let rel = r.rel.clone();
                    resp.context_menu(|ui| {
                        if ui.button("Open").clicked() {
                            acts.push(Action::OpenFile(rel.clone(), None));
                            ui.close();
                        }
                        if ui.button("Open in external editor").clicked() {
                            acts.push(Action::External(ctx.join(&rel), 1));
                            ui.close();
                        }
                        if ui.button("Diff").clicked() {
                            acts.push(Action::OpenDiff(Some(rel.clone())));
                            ui.close();
                        }
                        if ui.button("Copy path").clicked() {
                            acts.push(Action::Copy(ctx.join(&rel).display().to_string()));
                            ui.close();
                        }
                    });
                }
            }
        });
}

fn stage(ui: &mut Ui, app: &mut App, accent: Color32, acts: &mut Vec<Action>) {
    let full = ui.available_rect_before_wrap();
    let (strip, rect) = if app.settings.tabs_bottom {
        (
            Rect::from_min_max(pos2(full.left(), full.bottom() - TAB_H), full.max),
            Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - TAB_H - 6.0)),
        )
    } else {
        (
            Rect::from_min_size(full.min, vec2(full.width(), TAB_H)),
            Rect::from_min_max(pos2(full.left(), full.top() + TAB_H + 6.0), full.max),
        )
    };
    tab_strip(ui, app, strip, acts);
    if app.cards.is_empty() {
        let mut c = ui.new_child(egui::UiBuilder::new().id_salt("welcome").max_rect(rect));
        welcome(&mut c, app, acts);
        return;
    }
    // Cards in keel's grid; maximized, only the focused one, full size.
    let gap = vec2(8.0, 8.0);
    let rects = if app.zoom {
        let mut v = vec![Rect::NOTHING; app.cards.len()];
        v[app.active.min(app.cards.len() - 1)] = rect;
        v
    } else {
        crate::tiling::grid_sized(rect, app.cards.len(), gap, &app.col_w, &app.row_w)
    };
    app.card_rects = rects.clone();
    for (i, r) in rects.iter().enumerate() {
        if r.is_positive() {
            card(ui, app, i, *r, accent, acts);
        }
    }
    if !app.zoom && app.cards.len() > 1 {
        splitters(ui, app, rect, &rects, gap);
    }
    if let Some(from) = egui::DragAndDrop::payload::<TabDrag>(ui.ctx()).map(|p| p.0.clone()) {
        for (i, r) in rects.iter().enumerate() {
            if !r.is_positive() {
                continue;
            }
            let drop = ui.interact(*r, Id::unique(("tab_drop", i)), Sense::hover());
            if drop.dnd_hover_payload::<TabDrag>().is_some() {
                let z = r.shrink(4.0);
                ui.painter()
                    .rect_filled(z, 8.0, ACCENT.gamma_multiply(0.12));
                ui.painter().rect_stroke(
                    z,
                    8.0,
                    Stroke::new(2.0, ACCENT),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    z.center(),
                    Align2::CENTER_CENTER,
                    "Drop to tile beside",
                    FontId::proportional(15.0),
                    TEXT,
                );
            }
            if drop.dnd_release_payload::<TabDrag>().is_some() {
                acts.push(Action::TileOnCard(from.clone(), i));
            }
        }
    }
    // Dragging a card by its title bar onto another swaps the two, as in keel.
    if let Some(from) = egui::DragAndDrop::payload::<CardDrag>(ui.ctx()).map(|p| p.0) {
        for (i, r) in rects.iter().enumerate() {
            if i == from || !r.is_positive() {
                continue;
            }
            let drop = ui.interact(*r, Id::unique(("card_drop", i)), Sense::hover());
            if drop.dnd_hover_payload::<CardDrag>().is_some() {
                let z = r.shrink(4.0);
                ui.painter()
                    .rect_filled(z, 8.0, ACCENT.gamma_multiply(0.12));
                ui.painter().rect_stroke(
                    z,
                    8.0,
                    Stroke::new(2.0, ACCENT),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    z.center(),
                    Align2::CENTER_CENTER,
                    "Drop to swap",
                    FontId::proportional(15.0),
                    TEXT,
                );
            }
            if drop.dnd_release_payload::<CardDrag>().is_some() {
                acts.push(Action::SwapCards(from, i));
            }
        }
    }
}

/// The gaps between cards are handles: drag one to resize the cards on
/// either side, double-click it to even them out again.
fn splitters(ui: &mut Ui, app: &mut App, area: Rect, rects: &[Rect], gap: egui::Vec2) {
    let n = rects.len();
    let (rows, cols) = crate::tiling::shape(area, n);
    if app.col_w.len() < cols {
        app.col_w.resize(cols, 1.0);
    }
    if app.row_w.len() < rows {
        app.row_w.resize(rows, 1.0);
    }
    let min = 0.2;
    // Between columns, in each row.
    for r in 0..rows {
        let start = r * cols;
        let count = (n - start).min(cols);
        let sum: f32 = app.col_w[..count].iter().sum();
        let free = area.width() - gap.x * (count as f32 - 1.0);
        for k in 0..count.saturating_sub(1) {
            let (a, b) = (rects[start + k], rects[start + k + 1]);
            let handle = Rect::from_min_max(
                pos2(a.right() - 2.0, a.top()),
                pos2(b.left() + 2.0, a.bottom()),
            );
            let h = ui.interact(
                handle,
                Id::unique(("split-col", r, k)),
                Sense::click_and_drag(),
            );
            if h.hovered() || h.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                ui.painter().line_segment(
                    [
                        pos2(handle.center().x, handle.top() + 10.0),
                        pos2(handle.center().x, handle.bottom() - 10.0),
                    ],
                    Stroke::new(2.0, ACCENT.gamma_multiply(0.7)),
                );
            }
            if h.double_clicked() {
                app.col_w.iter_mut().for_each(|w| *w = 1.0);
            } else if h.dragged() {
                let d = h.drag_delta().x / free * sum;
                let (wa, wb) = (app.col_w[k] + d, app.col_w[k + 1] - d);
                if wa >= min && wb >= min {
                    app.col_w[k] = wa;
                    app.col_w[k + 1] = wb;
                }
            }
        }
    }
    // Between rows, across the stage.
    let sum: f32 = app.row_w[..rows].iter().sum();
    let free = area.height() - gap.y * (rows as f32 - 1.0);
    for r in 0..rows.saturating_sub(1) {
        let above = rects[r * cols];
        let below = rects[(r + 1) * cols];
        let handle = Rect::from_min_max(
            pos2(area.left(), above.bottom() - 2.0),
            pos2(area.right(), below.top() + 2.0),
        );
        let h = ui.interact(
            handle,
            Id::unique(("split-row", r)),
            Sense::click_and_drag(),
        );
        if h.hovered() || h.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            ui.painter().line_segment(
                [
                    pos2(handle.left() + 10.0, handle.center().y),
                    pos2(handle.right() - 10.0, handle.center().y),
                ],
                Stroke::new(2.0, ACCENT.gamma_multiply(0.7)),
            );
        }
        if h.double_clicked() {
            app.row_w.iter_mut().for_each(|w| *w = 1.0);
        } else if h.dragged() {
            let d = h.drag_delta().y / free * sum;
            let (wa, wb) = (app.row_w[r] + d, app.row_w[r + 1] - d);
            if wa >= min && wb >= min {
                app.row_w[r] = wa;
                app.row_w[r + 1] = wb;
            }
        }
    }
}

/// Drag payload for a tab: its task id.
struct TabDrag(String);

/// Drag payload for a card's title bar: its index on the stage.
struct CardDrag(usize);

/// The card's name following the pointer while it's dragged.
fn drag_ghost(ctx: &egui::Context, title: &str) {
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        Id::unique("card_drag_ghost"),
    ));
    let galley = p.layout_no_wrap(title.to_owned(), FontId::proportional(13.5), TEXT);
    let rect = Rect::from_min_size(pos + vec2(12.0, 8.0), galley.size() + vec2(20.0, 12.0));
    p.rect(
        rect,
        4.0,
        PANEL_2,
        Stroke::new(1.0, ACCENT),
        egui::StrokeKind::Inside,
    );
    p.galley(rect.min + vec2(10.0, 6.0), galley, TEXT);
}

fn welcome(ui: &mut Ui, app: &App, acts: &mut Vec<Action>) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.3);
        ui.label(
            RichText::new("Nothing running here yet")
                .size(18.0)
                .color(TEXT),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "Start an agent with a task, or open a project. {}P finds anything.",
                mod_label()
            ))
            .color(MUTED),
        );
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 290.0).max(0.0) / 2.0);
            if ui.button(RichText::new("✦ New agent").size(14.5)).clicked() {
                acts.push(Action::NewAgent);
            }
            if ui
                .button(RichText::new("+ New project").size(14.5))
                .clicked()
            {
                acts.push(Action::NewProject);
            }
        });
        if app.client.is_none() {
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!(
                    "starting daemon… (log: {})",
                    paths::daemon_log().display()
                ))
                .small()
                .color(AMBER),
            );
        }
    });
}

fn child_label(app: &App, t: &Target) -> String {
    match &t.child {
        Child::Terminal => {
            if !app.tasks.get(&t.item).is_some_and(is_agent) {
                "Terminal".into()
            } else {
                "Agent".into()
            }
        }
        Child::Changes => "Changes".into(),
        Child::Files => "Files".into(),
        Child::File(p) => {
            let dir = app.item_dir(&t.item);
            p.strip_prefix(&dir).unwrap_or(p).display().to_string()
        }
    }
}

fn editor_name(app: &App) -> String {
    match crate::editors::resolve(&app.settings.editor) {
        crate::editors::Kind::Builtin => "the built-in editor".into(),
        crate::editors::Kind::Terminal(c) | crate::editors::Kind::Gui(c) => c,
    }
}

/// One card: a breadcrumb, then the target's terminal, changes, files or file.
fn card(
    ui: &mut Ui,
    app: &mut App,
    side: usize,
    rect: Rect,
    accent: Color32,
    acts: &mut Vec<Action>,
) {
    let Some(t) = app.cards.get(side).cloned() else {
        return;
    };
    if !app.item_exists(&t.item) {
        return;
    }
    let active = app.active == side;
    let header = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.min.y + 36.0));
    let mut body =
        Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max).shrink2(vec2(1.0, 1.0));

    let id = Id::unique(("card", side));
    let resp = ui.interact(body, id, Sense::click_and_drag());
    if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
        acts.push(Action::FocusPane(side));
        resp.request_focus();
    }
    let other_focused = ui.ctx().memory(|m| m.focused().is_some_and(|f| f != id));
    if active
        && app.focus_pane
        && !other_focused
        && !resp.has_focus()
        && app.bar.is_none()
        && app.new_form.is_none()
        && app.confirm.is_none()
        && !app.committing
    {
        resp.request_focus();
    }
    if active && other_focused {
        app.focus_pane = false;
    }
    let focused = resp.has_focus();
    if focused {
        ui.ctx().memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }

    // The card itself.
    let scheme_bg = theme::scheme(&app.settings.scheme).bg;
    let painter = ui.painter();
    // Terminals keep their colour scheme; Changes and Files sit on keel's page colour.
    let fill = match t.child {
        Child::Terminal => theme::term_bg(scheme_bg),
        _ => theme::surface(theme::c::BG, scheme_bg),
    };
    painter.rect_filled(rect, CornerRadius::same(10), fill);
    painter.rect_filled(
        header,
        CornerRadius {
            nw: 10,
            ne: 10,
            sw: 0,
            se: 0,
        },
        theme::surface(PANEL_2, scheme_bg),
    );
    painter.line_segment(
        [header.left_bottom(), header.right_bottom()],
        Stroke::new(1.0, Color32::from_white_alpha(12)),
    );
    let edge = if app.broadcast && t.child == Child::Terminal {
        // keel marks broadcasting panes in orange.
        AMBER.gamma_multiply(0.8)
    } else if app.split() && active {
        accent.gamma_multiply(0.7)
    } else {
        Color32::from_white_alpha(16)
    };
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, edge),
        egui::StrokeKind::Inside,
    );

    // Breadcrumb: project › task › what.
    let mut crumb = egui::text::LayoutJob::default();
    let dim = egui::TextFormat::simple(FontId::proportional(12.5), MUTED);
    let strong = egui::TextFormat::simple(FontId::proportional(12.5), TEXT);
    crumb.append(
        &project_title(app, app.project.as_deref().unwrap_or("")),
        0.0,
        dim.clone(),
    );
    crumb.append(
        "  ›  ",
        0.0,
        egui::TextFormat::simple(FontId::proportional(12.5), FAINT),
    );
    // A project's own Changes and Files: project › Changes.
    if item_project(&t.item).is_none() {
        crumb.append(&item_title(app, &t.item), 0.0, strong.clone());
        crumb.append(
            "  ›  ",
            0.0,
            egui::TextFormat::simple(FontId::proportional(12.5), FAINT),
        );
        crumb.append(&child_label(app, &t), 0.0, dim);
    } else {
        crumb.append(&child_label(app, &t), 0.0, strong);
    }
    // The title bar: drag it onto another card to swap them (keel's panes),
    // right-click for the card's commands. Registered before the buttons so
    // they stay clickable on top of it.
    let title = format!("{} › {}", item_title(app, &t.item), child_label(app, &t));
    let hr = ui.interact(
        header,
        Id::unique(("card_head", side)),
        Sense::click_and_drag(),
    );
    if app.split() && !app.zoom {
        hr.dnd_set_drag_payload(CardDrag(side));
        if hr.dragged() {
            drag_ghost(ui.ctx(), &title);
        } else if hr.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    if hr.clicked() || hr.drag_started() {
        acts.push(Action::FocusPane(side));
    }
    let tm = app.settings.tile_mod;
    let key = |shift: bool, k: &str| {
        if tm == crate::tiling::TileMod::Off {
            String::new()
        } else {
            format!("   {}", crate::tiling::chord(tm, shift, k))
        }
    };
    let others: Vec<(usize, String)> = app
        .cards
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != side)
        .map(|(i, c)| {
            (
                i,
                format!("{} › {}", item_title(app, &c.item), child_label(app, c)),
            )
        })
        .collect();
    let (split, zoom, broadcast) = (app.split(), app.zoom, app.broadcast);
    hr.context_menu(|ui| {
        if split {
            if ui
                .button(format!(
                    "{}{}",
                    if zoom {
                        "Restore cards"
                    } else {
                        "Maximize card"
                    },
                    key(false, "Z")
                ))
                .clicked()
            {
                acts.push(Action::FocusPane(side));
                acts.push(Action::ToggleZoom);
                ui.close();
            }
            ui.menu_button("Swap with", |ui| {
                for (i, name) in &others {
                    if ui.button(name).clicked() {
                        acts.push(Action::SwapCards(side, *i));
                        ui.close();
                    }
                }
                ui.label(
                    RichText::new(format!(
                        "or drag the title bar · {}H/J/K/L",
                        key(true, "").trim()
                    ))
                    .small()
                    .color(MUTED),
                );
            });
            if ui
                .button(if broadcast {
                    "Stop broadcasting input"
                } else {
                    "Broadcast input to all terminal cards"
                })
                .clicked()
            {
                acts.push(Action::ToggleBroadcast);
                ui.close();
            }
            ui.separator();
            if ui
                .button(format!("Close card{}", key(false, "W")))
                .clicked()
            {
                acts.push(Action::CloseSide(side));
                ui.close();
            }
        } else if ui.button(format!("Split   {}D", mod_label())).clicked() {
            acts.push(Action::Split);
            ui.close();
        }
    });
    let mut hb = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(header.shrink2(vec2(12.0, 4.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    if app.split() {
        if hb
            .small_button("Close")
            .on_hover_text(format!("{}W", mod_label()))
            .clicked()
        {
            acts.push(Action::CloseSide(side));
        }
    } else if hb
        .small_button("Split")
        .on_hover_text(format!("{}D", mod_label()))
        .clicked()
    {
        acts.push(Action::Split);
    }
    if app.broadcast && t.child == Child::Terminal {
        hb.label(RichText::new("broadcasting").small().color(AMBER))
            .on_hover_text(format!(
                "Typing goes to every terminal card ({}B turns it off)",
                mod_label()
            ));
    }
    if matches!(t.child, Child::Files | Child::File(_)) {
        if hb
            .add(egui::Button::new(RichText::new("change").small().color(ACCENT)).frame(false))
            .clicked()
        {
            acts.push(Action::Settings);
        }
        hb.label(
            RichText::new(format!("opens in {}", editor_name(app)))
                .small()
                .color(MUTED),
        );
    }
    let room = hb.min_rect().left() - header.left() - 24.0;
    if room > 40.0 {
        let galley = ui.fonts_mut(|f| f.layout_job(crumb));
        let p = ui.painter_at(Rect::from_min_max(
            header.min,
            pos2(header.left() + 12.0 + room, header.max.y),
        ));
        p.galley(
            pos2(
                header.left() + 14.0,
                header.center().y - galley.size().y / 2.0,
            ),
            galley,
            TEXT,
        );
    }

    // A pending approval for this agent sits on top of its terminal.
    if t.child == Child::Terminal {
        let pending: Vec<_> = app
            .approvals
            .values()
            .filter(
                |a| matches!(&a.source, ApprovalSource::Task { task_id, .. } if *task_id == t.item),
            )
            .cloned()
            .collect();
        if let Some(a) = pending.first() {
            let strip = Rect::from_min_size(body.min, vec2(body.width(), 40.0));
            body.min.y += 40.0;
            ui.painter()
                .rect_filled(strip, 0.0, Color32::from_rgb(0x2c, 0x25, 0x17));
            let mut s = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(strip.shrink2(vec2(12.0, 6.0)))
                    .layout(Layout::left_to_right(Align::Center)),
            );
            s.label(RichText::new("Run this command?").color(AMBER));
            s.add(egui::Label::new(RichText::new(&a.command).monospace().color(TEXT)).truncate())
                .on_hover_text(&a.reason);
            s.with_layout(Layout::right_to_left(Align::Center), |s| {
                if s.button(
                    RichText::new(format!("Approve  {}Y", mod_label()))
                        .color(GREEN)
                        .strong(),
                )
                .clicked()
                {
                    acts.push(Action::Decide(a.id, true));
                }
                if s.button(RichText::new(format!("Reject  {}R", mod_label())).color(RED))
                    .clicked()
                {
                    acts.push(Action::Decide(a.id, false));
                }
                if pending.len() > 1 {
                    s.label(
                        RichText::new(format!("+{} more", pending.len() - 1))
                            .small()
                            .color(MUTED),
                    );
                }
            });
        }
    }

    // Git data and the file tree are loaded for the focused task only; a
    // second card on another task asks to be focused first.
    let loaded = app.ctx_task.as_deref() == Some(t.item.as_str());
    if !loaded && t.child != Child::Terminal {
        ui.painter().text(
            body.center(),
            Align2::CENTER_CENTER,
            "Click to show",
            FontId::proportional(13.0),
            MUTED,
        );
        return;
    }
    match t.child {
        Child::Terminal => term_body(ui, app, side, &t.item, body, focused, acts),
        // Each card gets its own id scope: two cards both showing Changes or
        // Files would otherwise share their scroll areas' ids.
        Child::Changes => {
            ui.push_id(("card-changes", side), |ui| {
                gitview::body(ui, app, side, body.shrink2(vec2(6.0, 6.0)), focused, acts)
            });
        }
        Child::Files => {
            // A narrow column keeps each file's change marker next to its name.
            let r = body.shrink2(vec2(10.0, 8.0));
            let r = Rect::from_min_size(r.min, vec2(r.width().min(560.0), r.height()));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("card-files", side))
                    .max_rect(r)
                    .layout(Layout::top_down(Align::Min)),
            );
            files(&mut child, app, acts);
        }
        Child::File(path) => file_card(ui, app, side, &path, body, &resp, focused, acts),
    }
}

/// A file opened from a task, shown the way the editor setting says.
#[allow(clippy::too_many_arguments)]
fn file_card(
    ui: &mut Ui,
    app: &mut App,
    side: usize,
    path: &std::path::Path,
    body: Rect,
    resp: &egui::Response,
    focused: bool,
    acts: &mut Vec<Action>,
) {
    let rel = {
        let dir = app.ctx_dir();
        path.strip_prefix(&dir)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let center = |ui: &mut Ui, text: String, button: Option<String>, acts: &mut Vec<Action>| {
        ui.painter().text(
            body.center() - vec2(0.0, 14.0),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(13.0),
            MUTED,
        );
        if let Some(b) = button {
            let r = Rect::from_center_size(body.center() + vec2(0.0, 16.0), vec2(160.0, 26.0));
            if ui.put(r, egui::Button::new(b)).clicked() {
                acts.push(Action::OpenFile(rel.clone(), None));
            }
        }
    };
    match crate::editors::resolve(&app.settings.editor) {
        crate::editors::Kind::Terminal(cmd) => {
            // This card's own item; the focused card may be showing another.
            let item = app
                .cards
                .get(side)
                .map(|t| t.item.clone())
                .unwrap_or_default();
            let ed = crate::editors::task_id(&item);
            let live = app
                .tasks
                .get(&ed)
                .is_some_and(|t| !matches!(t.status, TaskStatus::Exited { .. }));
            if live {
                term_body(ui, app, side, &ed, body, focused, acts);
            } else {
                center(
                    ui,
                    format!("{cmd} isn't running for this task"),
                    Some(format!("Open in {cmd}")),
                    acts,
                );
            }
        }
        kind => {
            let it = &mut app.tabs[app.cur];
            let Some(i) = it.files.iter().position(|e| e.path == path) else {
                center(ui, rel.clone(), Some("Open".into()), acts);
                return;
            };
            it.file = i;
            let mut body = body;
            if let crate::editors::Kind::Gui(cmd) = kind {
                let strip = Rect::from_min_size(body.min, vec2(body.width(), 32.0));
                body.min.y += 32.0;
                ui.painter().rect_filled(strip, 0.0, PANEL_2);
                let mut s = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(strip.shrink2(vec2(12.0, 4.0)))
                        .layout(Layout::left_to_right(Align::Center)),
                );
                s.label(
                    RichText::new(format!(
                        "Open in {cmd}. This view follows along and reloads when you save."
                    ))
                    .small()
                    .color(MUTED),
                );
                s.with_layout(Layout::right_to_left(Align::Center), |s| {
                    if s.small_button(format!("Open in {cmd} again")).clicked() {
                        acts.push(Action::OpenFile(rel.clone(), None));
                    }
                });
            }
            editor_body(ui, app, body, resp, focused, acts);
        }
    }
}

/// Notices and approvals for tasks that aren't on screen, bottom right.
fn overlays(ctx: &egui::Context, app: &mut App, acts: &mut Vec<Action>) {
    let visible = |item: &str| {
        app.cards
            .iter()
            .any(|t| t.item == item && t.child == Child::Terminal)
    };
    let others: Vec<_> = app
        .approvals
        .values()
        .filter(|a| match &a.source {
            ApprovalSource::Task { task_id, .. } => !visible(task_id),
            ApprovalSource::Ops { .. } => true,
        })
        .take(3)
        .cloned()
        .collect();
    if others.is_empty() && app.notice.is_none() {
        return;
    }
    egui::Area::new(Id::unique("overlays"))
        .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -20.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            for a in &others {
                Frame::new()
                    .fill(Color32::from_rgb(0x2c, 0x25, 0x17))
                    .stroke(Stroke::new(1.0, AMBER.gamma_multiply(0.4)))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        let who = match &a.source {
                            ApprovalSource::Task { task_id, .. } => app
                                .tasks
                                .get(task_id)
                                .map(|t| app.label(t))
                                .unwrap_or(task_id.clone()),
                            ApprovalSource::Ops { session, .. } => {
                                format!("investigation #{session}")
                            }
                        };
                        ui.label(
                            RichText::new(format!("{who} wants to run"))
                                .small()
                                .color(AMBER),
                        );
                        ui.add(
                            egui::Label::new(RichText::new(&a.command).monospace().color(TEXT))
                                .truncate(),
                        );
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("Approve").color(GREEN)).clicked() {
                                acts.push(Action::Decide(a.id, true));
                            }
                            if ui.button(RichText::new("Reject").color(RED)).clicked() {
                                acts.push(Action::Decide(a.id, false));
                            }
                            if let ApprovalSource::Task { task_id, .. } = &a.source {
                                if ui.small_button("view").clicked() {
                                    acts.push(Action::OpenTerm(task_id.clone()));
                                }
                            }
                        });
                    });
                ui.add_space(6.0);
            }
            if let Some((m, _, err)) = &app.notice {
                let r = Frame::new()
                    .fill(Color32::from_rgb(0x20, 0x24, 0x2c))
                    .stroke(Stroke::new(1.0, Color32::from_white_alpha(18)))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(m).color(if *err { RED } else { TEXT }));
                    })
                    .response
                    .interact(Sense::click());
                if r.clicked() {
                    app.notice = None;
                }
            }
        });
}

// ── terminal ────────────────────────────────────────────────────────────

/// A terminal in a card: keel's terminal view (selection, mouse reporting,
/// ⌘F find, ⌘-click links and paths, ⌘↑/⌘↓ between commands).
fn term_body(
    ui: &mut Ui,
    app: &mut App,
    side: usize,
    id: &str,
    body: Rect,
    focused: bool,
    acts: &mut Vec<Action>,
) {
    let scheme = theme::scheme(&app.settings.scheme);
    if !app.terms.contains_key(id) {
        // First time on screen: attach at the size this card fits, with the
        // same metrics the view uses.
        let font = theme::term_font(app.settings.font_size);
        let cw = ui.fonts_mut(|f| f.glyph_width(&font, 'M')).max(1.0);
        let ch = (ui.fonts_mut(|f| f.row_height(&font)) * app.settings.line_height)
            .round()
            .max(1.0);
        let inner = body.shrink(10.0);
        let cols = ((inner.width() / cw).floor() as i32).clamp(2, 1000) as u16;
        let rows = ((inner.height() / ch).floor() as i32).clamp(1, 500) as u16;
        app.sync_term(id, rows, cols);
        ui.painter()
            .rect_filled(body, 0.0, theme::term_bg(scheme.bg));
        ui.painter().text(
            body.center(),
            Align2::CENTER_CENTER,
            "attaching…",
            FontId::proportional(13.0),
            MUTED,
        );
        return;
    }
    let App {
        terms, settings, ..
    } = app;
    let t = terms.get_mut(id).unwrap();
    t.view.local = true;
    // The card holds focus while switching; the view takes it from there.
    if focused {
        t.view.want_focus = true;
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    let (actions, resp) = t.view.ui(&mut child, settings, scheme, true);
    if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
        acts.push(Action::FocusPane(side));
    }
    let mut actions = actions;
    let snippets = settings.snippets.clone();
    let mut run_snippet = None;
    resp.context_menu(|ui| {
        let sel = t.view.selection_text();
        if ui
            .add_enabled(sel.is_some(), egui::Button::new("Copy"))
            .clicked()
        {
            if let Some(text) = sel {
                ui.ctx().copy_text(text);
            }
            ui.close();
        }
        if ui.button("Paste").clicked() {
            if let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                actions.push(termview::TermAction::Paste(text));
            }
            ui.close();
        }
        if ui.button("Select all").clicked() {
            t.view.select_all();
            ui.close();
        }
        let out = t.view.last_output();
        if ui
            .add_enabled(
                out.is_some(),
                egui::Button::new("Copy last command's output"),
            )
            .clicked()
        {
            if let Some(text) = out {
                ui.ctx().copy_text(text);
            }
            ui.close();
        }
        if !snippets.is_empty() {
            ui.menu_button("Run snippet", |ui| {
                for (i, sn) in snippets.iter().enumerate() {
                    if ui.button(&sn.name).on_hover_text(&sn.body).clicked() {
                        run_snippet = Some(i);
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        if ui.button(format!("Find…   {}F", cmd_label())).clicked() {
            t.view.open_find();
            ui.close();
        }
        if ui.button(format!("Clear   {}K", cmd_label())).clicked() {
            t.view.clear_scrollback();
            actions.push(termview::TermAction::Typed(vec![0x0c]));
            ui.close();
        }
    });
    if let Some(i) = run_snippet {
        acts.push(Action::RunSnippet(i));
    }
    for a in actions {
        match a {
            termview::TermAction::Input(b) => acts.push(Action::Input(id.to_string(), b)),
            termview::TermAction::Typed(b) => acts.push(Action::Typed(id.to_string(), b)),
            termview::TermAction::Paste(text) => acts.push(Action::Paste(id.to_string(), text)),
            termview::TermAction::Resize { cols, rows } => {
                acts.push(Action::Resize(id.to_string(), rows, cols))
            }
            termview::TermAction::OpenFile(path, line) => {
                acts.push(Action::OpenFile(path.display().to_string(), line))
            }
        }
    }
}

// ── editor ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn editor_body(
    ui: &mut Ui,
    app: &mut App,
    body: Rect,
    resp: &egui::Response,
    focused: bool,
    acts: &mut Vec<Action>,
) {
    let font = theme::term_font(app.settings.font_size);
    let cell = term::cell_size(ui, &font, 1.0);
    let mut body = body;

    // Find / go-to bar.
    let mut bar_action: Option<(bool, String)> = None; // (is_find, value)
    if app.find.is_some() || app.goto.is_some() {
        let bar = Rect::from_min_size(body.min, vec2(body.width(), 30.0));
        body.min.y += 30.0;
        ui.painter().rect_filled(bar, 0.0, PANEL_2);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar.shrink2(vec2(8.0, 4.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let is_find = app.find.is_some();
        let label = if is_find { "Find" } else { "Go to line" };
        child.label(RichText::new(label).small().color(MUTED));
        let val = if is_find {
            app.find.as_mut().unwrap()
        } else {
            app.goto.as_mut().unwrap()
        };
        let r = child.add(TextEdit::singleline(val).desired_width(260.0));
        r.request_focus();
        if child.input(|i| i.key_pressed(Key::Enter)) {
            bar_action = Some((is_find, val.clone()));
        }
        if child.input(|i| i.key_pressed(Key::Escape)) || child.small_button("close").clicked() {
            app.find = None;
            app.goto = None;
            app.focus_pane = true;
        }
    }

    let idx = app.cur;
    let Some(e) = app.tabs[idx].editor() else {
        return;
    };
    if let Some((is_find, v)) = bar_action {
        if is_find {
            if !e.find_next(&v) {
                app.notice = Some((format!("`{v}` not found"), std::time::Instant::now(), true));
            }
            e.follow = true;
        } else if let Ok(n) = v.trim().parse::<usize>() {
            e.goto(n);
            e.follow = true;
            app.goto = None;
            app.focus_pane = true;
        }
    }

    let digits = e.lines.len().to_string().len();
    let gutter = (digits as f32 + 2.0) * cell.x;
    let h = ((body.height() - 4.0) / cell.y).floor().max(1.0) as usize;
    let w = ((body.width() - gutter - 8.0) / cell.x).floor().max(1.0) as usize;

    // Mouse.
    if resp.clicked() || resp.dragged() {
        if let Some(pp) = resp.interact_pointer_pos() {
            if body.contains(pp) {
                let row = ((pp.y - body.top() - 2.0) / cell.y).floor().max(0.0) as usize;
                let col = ((pp.x - body.left() - gutter) / cell.x).round().max(0.0) as usize;
                e.click(row, col);
            }
        }
    }
    if resp.hovered() && ui.input(|i| i.pointer.hover_pos().is_some_and(|p| body.contains(p))) {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy.abs() > 0.5 {
            e.scroll(-(dy / cell.y).round() as isize);
        }
    }

    // Keyboard.
    if focused && app.find.is_none() && app.goto.is_none() {
        let events = ui.input(|i| i.events.clone());
        let mut saved: Option<Result<(), String>> = None;
        for ev in events {
            match ev {
                Event::Text(t) => {
                    for ch in t.chars() {
                        if !ch.is_control() {
                            e.insert_char(ch);
                        }
                    }
                }
                Event::Paste(s) => e.insert_str(&s),
                Event::Copy => acts.push(Action::Copy(format!("{}\n", e.lines[e.cy]))),
                Event::Cut => {
                    acts.push(Action::Copy(format!("{}\n", e.lines[e.cy])));
                    e.delete_line();
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers: m,
                    ..
                } => {
                    let cmd = m.command;
                    let word = if cfg!(target_os = "macos") {
                        m.alt
                    } else {
                        m.ctrl
                    };
                    match key {
                        Key::S if cmd => saved = Some(e.save().map_err(|x| format!("{x:#}"))),
                        Key::Z if cmd && m.shift => e.redo(),
                        Key::Z if cmd => e.undo(),
                        Key::Y if cmd => e.redo(),
                        Key::F if cmd => app.find = Some(e.last_search.clone().unwrap_or_default()),
                        Key::L if cmd => app.goto = Some(String::new()),
                        Key::G if cmd => {
                            if let Some(q) = e.last_search.clone() {
                                e.find_next(&q);
                            }
                        }
                        Key::F3 => {
                            if let Some(q) = e.last_search.clone() {
                                e.find_next(&q);
                            }
                        }
                        Key::K if cmd => e.delete_line(),
                        Key::R if cmd => e.reload(),
                        Key::Enter => e.newline(),
                        Key::Tab => e.tab(),
                        Key::Backspace => e.backspace(),
                        Key::Delete => e.delete(),
                        Key::ArrowLeft if word => e.word_left(),
                        Key::ArrowRight if word => e.word_right(),
                        Key::ArrowLeft if cmd => e.home(),
                        Key::ArrowRight if cmd => e.end(),
                        Key::ArrowUp if cmd => e.top(),
                        Key::ArrowDown if cmd => e.bottom(),
                        Key::ArrowLeft => e.left(),
                        Key::ArrowRight => e.right(),
                        Key::ArrowUp => e.vertical(-1),
                        Key::ArrowDown => e.vertical(1),
                        Key::Home if cmd => e.top(),
                        Key::End if cmd => e.bottom(),
                        Key::Home => e.home(),
                        Key::End => e.end(),
                        Key::PageUp => e.vertical(-(h as isize)),
                        Key::PageDown => e.vertical(h as isize),
                        _ => continue,
                    }
                }
                _ => continue,
            }
            e.follow = true;
        }
        if let Some(r) = saved {
            app.notice = Some(match r {
                Ok(_) => (
                    format!("saved {}", e.name()),
                    std::time::Instant::now(),
                    false,
                ),
                Err(x) => (format!("save failed: {x}"), std::time::Instant::now(), true),
            });
            app.refresh_git_now();
        }
    }
    let Some(e) = app.tabs[idx].editor() else {
        return;
    };
    if e.follow {
        e.ensure_visible(h, w);
        e.follow = false;
    }

    // Paint.
    let painter = ui.painter_at(body);
    painter.rect_filled(body, 0.0, theme::term_bg(term::BG));
    let top = body.top() + 2.0;
    for row in 0..h {
        let y = e.scroll_y + row;
        let py = top + row as f32 * cell.y;
        if y >= e.lines.len() {
            break;
        }
        if y == e.cy {
            painter.rect_filled(
                Rect::from_min_size(pos2(body.left(), py), vec2(body.width(), cell.y)),
                0.0,
                Color32::from_rgb(0x1a, 0x1e, 0x25),
            );
        }
        painter.text(
            pos2(body.left() + gutter - cell.x, py),
            Align2::RIGHT_TOP,
            (y + 1).to_string(),
            font.clone(),
            if y == e.cy {
                MUTED
            } else {
                Color32::from_rgb(0x4a, 0x50, 0x5a)
            },
        );
        let spans = e.highlight.get(y);
        let mut col = 0usize;
        let mut byte = 0usize;
        let mut si = 0usize;
        let mut send = spans
            .and_then(|s| s.first())
            .map(|s| s.1)
            .unwrap_or(usize::MAX);
        let mut run = String::new();
        let mut run_col = 0usize;
        let mut run_color = TEXT;
        let x0 = body.left() + gutter;
        let flush = |run: &mut String, run_col: usize, color: Color32| {
            if !run.trim().is_empty() {
                painter.text(
                    pos2(x0 + (run_col as f32 - e.scroll_x as f32) * cell.x, py),
                    Align2::LEFT_TOP,
                    run.as_str(),
                    font.clone(),
                    color,
                );
            }
            run.clear();
        };
        for ch in e.lines[y].chars() {
            while byte >= send {
                si += 1;
                send += spans
                    .and_then(|s| s.get(si))
                    .map(|s| s.1)
                    .unwrap_or(usize::MAX / 2);
            }
            let color = spans
                .and_then(|s| s.get(si))
                .map(|s| Color32::from_rgb(s.0 .0, s.0 .1, s.0 .2))
                .unwrap_or(TEXT);
            let cw = e.char_width(ch, col);
            let visible = col >= e.scroll_x && col + cw <= e.scroll_x + w;
            if visible && ch.is_ascii() && ch != '\t' {
                if run.is_empty() {
                    run_col = col;
                    run_color = color;
                } else if color != run_color {
                    flush(&mut run, run_col, run_color);
                    run_col = col;
                    run_color = color;
                }
                run.push(ch);
            } else {
                flush(&mut run, run_col, run_color);
                if visible && ch != '\t' {
                    let mut buf = [0u8; 4];
                    painter.text(
                        pos2(x0 + (col as f32 - e.scroll_x as f32) * cell.x, py),
                        Align2::LEFT_TOP,
                        &*ch.encode_utf8(&mut buf),
                        font.clone(),
                        color,
                    );
                }
            }
            col += cw;
            byte += ch.len_utf8();
            if col > e.scroll_x + w {
                break;
            }
        }
        flush(&mut run, run_col, run_color);
    }
    // Cursor.
    if e.cy >= e.scroll_y && e.cy < e.scroll_y + h {
        let col = e.col_of(e.cy, e.cx);
        if col >= e.scroll_x {
            let x = body.left() + gutter + (col - e.scroll_x) as f32 * cell.x;
            let y = top + (e.cy - e.scroll_y) as f32 * cell.y;
            painter.rect_filled(
                Rect::from_min_size(pos2(x, y), vec2(2.0, cell.y)),
                0.0,
                if focused { ACCENT } else { MUTED },
            );
        }
    }
    if e.stale {
        let msg = " changed on disk — Ctrl/⌘R reload (drops your edits) or save to overwrite ";
        let r = Rect::from_min_size(
            pos2(body.left(), body.bottom() - 22.0),
            vec2(body.width(), 22.0),
        );
        painter.rect_filled(r, 0.0, AMBER);
        painter.text(
            r.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            msg,
            FontId::proportional(12.0),
            Color32::BLACK,
        );
    }
}

// ── dialogs ─────────────────────────────────────────────────────────────

/// Name a new agent or project before it's created.
fn new_form(ctx: &egui::Context, app: &mut App) {
    use crate::app::NewKind;
    let Some(kind) = app.new_form.as_ref().map(|f| f.kind) else {
        return;
    };
    let agents: Vec<String> = app
        .cfg
        .agents
        .keys()
        .filter(|a| *a != "shell")
        .cloned()
        .collect();
    let problem = app.form_problem();
    let in_repo = app
        .new_form
        .as_ref()
        .map(|f| git::root(&paths::expand(f.dir.trim())).is_some())
        .unwrap_or(false);
    // ⌘↵ creates from anywhere in the form (the task box takes plain Enter).
    let (cmd_enter, esc) = ctx.input(|i| {
        (
            i.key_pressed(Key::Enter) && i.modifiers.command,
            i.key_pressed(Key::Escape),
        )
    });
    let mut create = cmd_enter;
    let mut cancel = esc;
    let title = match kind {
        NewKind::Agent => "New agent",
        NewKind::Project => "New project",
    };
    let id = Id::unique("new-form");
    egui::Modal::new(id)
        .area(egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 110.0)))
        .show(ctx, |ui| {
            let f = app.new_form.as_mut().unwrap();
            ui.set_width(520.0);
            ui.label(RichText::new(title).size(16.0).strong().color(TEXT));
            ui.add_space(8.0);
            egui::Grid::new("new-form-grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| match kind {
                    NewKind::Agent => {
                        ui.label("Task");
                        let r = ui.add(
                            TextEdit::multiline(&mut f.task)
                                .hint_text("What should it do? (optional: you can tell it later)")
                                .desired_rows(3)
                                .desired_width(380.0),
                        );
                        if r.changed() && !f.name_edited {
                            f.name = crate::app::suggest_name(&f.task);
                        }
                        if !f.focus_set {
                            r.request_focus();
                            f.focus_set = true;
                        }
                        ui.end_row();
                        ui.label("Name");
                        let r = ui.add(
                            TextEdit::singleline(&mut f.name)
                                .hint_text("fix-login-redirect")
                                .desired_width(380.0),
                        );
                        if r.changed() {
                            f.name_edited = true;
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            create = true;
                        }
                        ui.end_row();
                        ui.label("Agent");
                        egui::ComboBox::from_id_salt("new-agent-kind")
                            .selected_text(f.agent.clone())
                            .show_ui(ui, |ui| {
                                for a in &agents {
                                    ui.selectable_value(&mut f.agent, a.clone(), a);
                                }
                            });
                        ui.end_row();
                        ui.label("Folder");
                        ui.horizontal(|ui| {
                            ui.add(TextEdit::singleline(&mut f.dir).desired_width(290.0));
                            if ui.button("Choose…").clicked() {
                                if let Some(p) = pick_folder(&f.dir) {
                                    f.dir = tilde(&p);
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("");
                        let id = crate::app::slugify(&f.name);
                        let label = if in_repo {
                            format!(
                                "Own branch and worktree (muster/{})",
                                if id.is_empty() { "<name>" } else { &id }
                            )
                        } else {
                            "Own branch and worktree (the folder isn't a git repo)".into()
                        };
                        ui.add_enabled(in_repo, egui::Checkbox::new(&mut f.worktree, label));
                        ui.end_row();
                    }
                    NewKind::Project => {
                        ui.label("Folder");
                        let (r, picked) = ui
                            .horizontal(|ui| {
                                let r = ui.add(
                                    TextEdit::singleline(&mut f.dir)
                                        .hint_text("~/code/my-app")
                                        .desired_width(290.0),
                                );
                                let mut picked = false;
                                if ui.button("Choose…").clicked() {
                                    if let Some(p) = pick_folder(&f.dir) {
                                        f.dir = tilde(&p);
                                        picked = true;
                                    }
                                }
                                (r, picked)
                            })
                            .inner;
                        if !f.focus_set {
                            r.request_focus();
                            f.focus_set = true;
                        }
                        // The name follows the folder until you type one.
                        if (r.changed() || picked) && !f.name_edited {
                            f.name = paths::expand(f.dir.trim())
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            create = true;
                        }
                        ui.end_row();
                        ui.label("Name");
                        let r = ui.add(TextEdit::singleline(&mut f.name).desired_width(380.0));
                        if r.changed() {
                            f.name_edited = true;
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            create = true;
                        }
                        ui.end_row();
                        ui.label("");
                        ui.label(
                            RichText::new(
                                "Opens a terminal there; the project shows in the sidebar under this name.",
                            )
                            .small()
                            .color(MUTED),
                        );
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = match kind {
                    NewKind::Agent => "Start agent",
                    NewKind::Project => "Create project",
                };
                if ui
                    .add_enabled(
                        problem.is_none(),
                        egui::Button::new(RichText::new(label).color(theme::c::ON_ACCENT).strong())
                            .fill(ACCENT),
                    )
                    .on_hover_text(format!("{}↵", cmd_label()))
                    .clicked()
                {
                    create = true;
                }
                if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    cancel = true;
                }
                if let Some(p) = &problem {
                    ui.label(RichText::new(p).small().color(AMBER));
                }
            });
        });
    if cancel {
        app.new_form = None;
        app.focus_pane = true;
    } else if create {
        app.submit_form();
    }
}

fn dialogs(ctx: &egui::Context, app: &mut App, acts: &mut Vec<Action>) {
    new_form(ctx, app);
    if let Some(c) = &app.confirm {
        let text = c.text().to_string();
        let mut answer: Option<bool> = None;
        let m = egui::Modal::new(Id::unique("confirm")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new(text).size(14.0));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Yes").strong()).clicked()
                    || ui.input(|i| i.key_pressed(Key::Enter))
                {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if m.should_close() && answer.is_none() {
            answer = Some(false);
        }
        if let Some(a) = answer {
            let c = app.confirm.take().unwrap();
            if a {
                app.run_confirm(c);
            }
            app.focus_pane = true;
        }
    }
    if app.committing {
        let mut commit = false;
        let mut close = false;
        let staged = app.changes.iter().filter(|c| c.staged()).count();
        egui::Window::new("Commit")
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 140.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{staged} staged file(s) on {}",
                        app.branch.clone().unwrap_or_default()
                    ))
                    .small()
                    .color(MUTED),
                );
                let r = ui.add(
                    TextEdit::multiline(&mut app.commit_msg)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("Commit message"),
                );
                r.request_focus();
                // Enter commits, Shift+Enter is a new line, like lazygit.
                let (enter, shift, esc) = ui.input(|i| {
                    (
                        i.key_pressed(Key::Enter),
                        i.modifiers.shift,
                        i.key_pressed(Key::Escape),
                    )
                });
                if enter && !shift {
                    // The text box already took the newline; drop it.
                    while app.commit_msg.ends_with('\n') {
                        app.commit_msg.pop();
                    }
                    commit = true;
                }
                close = esc;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            staged > 0,
                            egui::Button::new(RichText::new("Commit").strong()),
                        )
                        .clicked()
                    {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    ui.label(
                        RichText::new("Enter commits · Shift+Enter new line · Esc cancels")
                            .small()
                            .color(MUTED),
                    );
                });
            });
        if commit && !app.commit_msg.trim().is_empty() {
            acts.push(Action::Commit);
            app.committing = false;
            app.focus_pane = true;
        } else if close {
            app.committing = false;
            app.focus_pane = true;
        }
    }
}

// ── apply ───────────────────────────────────────────────────────────────

// ── command bar ─────────────────────────────────────────────────────────

type BarFn = fn(&mut App, &mut Vec<Action>);

/// What a command bar entry does.
enum BarAct {
    Start(String),
    Item(String),
    File(String),
    Snippet(usize),
    Run(BarFn),
}

struct BarOpt {
    group: &'static str,
    icon: String,
    text: String,
    hint: String,
    act: BarAct,
}

/// Characters of `q` appear in order in `s`. Lower score is a better match.
fn fuzzy(q: &str, s: &str) -> Option<usize> {
    if q.is_empty() {
        return Some(0);
    }
    let (q, s) = (q.to_lowercase(), s.to_lowercase());
    if let Some(i) = s.find(&q) {
        return Some(i);
    }
    let mut it = s.chars().enumerate();
    let mut last = 0;
    for qc in q.chars() {
        let (i, _) = it.find(|(_, c)| *c == qc)?;
        last = i;
    }
    Some(100 + last)
}

fn bar_options(app: &App, bar: &CmdBar) -> Vec<BarOpt> {
    let q = bar.query.trim();
    let mut out = vec![];
    let agent = app.settings.default_agent.clone();
    let iso = if app.settings.worktree {
        "own branch + worktree"
    } else {
        "in the project folder"
    };
    let mut files: Vec<(usize, &String)> = app
        .tree
        .files()
        .iter()
        .filter_map(|f| {
            let name = f.rsplit('/').next().unwrap_or(f);
            fuzzy(q, name)
                .map(|s| (s, f))
                .or_else(|| fuzzy(q, f).map(|s| (s + 50, f)))
        })
        .collect();
    if !q.is_empty() {
        files.sort_by_key(|(s, f)| (*s, f.len()));
        for (_, f) in files.into_iter().take(8) {
            out.push(BarOpt {
                group: "Files",
                icon: "▤".into(),
                text: f.clone(),
                hint: format!("opens in {}", editor_name(app)),
                act: BarAct::File(f.clone()),
            });
        }
    }
    let mut tasks: Vec<&TaskInfo> = app
        .tasks
        .values()
        .filter(|t| !crate::editors::is_editor_task(&t.id))
        .filter(|t| fuzzy(q, &app.label(t)).is_some() || fuzzy(q, &t.name).is_some())
        .collect();
    tasks.sort_by_key(|t| (urgency(t), !is_agent(t), t.created_unix));
    for t in tasks.into_iter().take(8) {
        let (state, _) = state_label(t);
        out.push(BarOpt {
            group: "Agents and terminals",
            icon: status_mark(t, app.frame).0.into(),
            text: app.label(t),
            hint: format!(
                "{} · {}",
                project_title(app, &app.project_of(t)),
                if is_agent(t) { state } else { "terminal" }
            ),
            act: BarAct::Item(t.id.clone()),
        });
    }
    for (i, sn) in app.settings.snippets.iter().enumerate() {
        if !q.is_empty() && fuzzy(q, &sn.name).is_some() {
            out.push(BarOpt {
                group: "Snippets",
                icon: "»".into(),
                text: sn.name.clone(),
                hint: sn.body.lines().next().unwrap_or_default().to_string(),
                act: BarAct::Snippet(i),
            });
        }
    }
    if q.split_whitespace().count() >= 2 {
        out.push(BarOpt {
            group: "New agent",
            icon: "✦".into(),
            text: format!("Start {agent}: “{q}”"),
            hint: iso.into(),
            act: BarAct::Start(q.to_string()),
        });
    }
    let cmds: [(&str, &str, BarFn); 10] = [
        ("New agent", "✦", |_, a| a.push(Action::NewAgent)),
        ("New terminal", "❯", |app, a| {
            a.push(new_terminal_action(app))
        }),
        ("New project…", "+", |_, a| a.push(Action::NewProject)),
        ("Show changes", "±", |app, _| app.show_git()),
        ("Browse files", "▤", |app, _| {
            app.toggle_child(Child::Files)
        }),
        ("Commit staged changes", "c", |_, a| {
            a.push(Action::StartCommit)
        }),
        ("Merge agent branch into main", "⇢", |_, a| {
            a.push(Action::Merge)
        }),
        ("Split view", "◫", |_, a| a.push(Action::Split)),
        ("Settings: editor, font, theme", "⚙", |_, a| {
            a.push(Action::Settings)
        }),
        ("Keyboard shortcuts", "?", |_, a| a.push(Action::Shortcuts)),
    ];
    for (text, icon, f) in cmds {
        if fuzzy(q, text).is_some() {
            out.push(BarOpt {
                group: "Commands",
                icon: icon.into(),
                text: text.into(),
                hint: String::new(),
                act: BarAct::Run(f),
            });
        }
    }
    out
}

fn command_bar(ctx: &egui::Context, app: &mut App, acts: &mut Vec<Action>) {
    let Some(bar) = app.bar.as_ref() else {
        return;
    };
    let opts = bar_options(app, bar);
    let (up, down, enter, esc) = ctx.input(|i| {
        (
            i.key_pressed(Key::ArrowUp),
            i.key_pressed(Key::ArrowDown),
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Escape),
        )
    });
    let bar = app.bar.as_mut().unwrap();
    if down {
        bar.sel = (bar.sel + 1).min(opts.len().saturating_sub(1));
    }
    if up {
        bar.sel = bar.sel.saturating_sub(1);
    }
    bar.sel = bar.sel.min(opts.len().saturating_sub(1));
    let mut chosen: Option<usize> = enter.then_some(bar.sel);
    let id = Id::unique("cmdbar");
    let m = egui::Modal::new(id)
        .area(egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 90.0)))
        .frame(
            Frame::new()
                .fill(Color32::from_rgb(0x1a, 0x1d, 0x24))
                .corner_radius(CornerRadius::same(12))
                .stroke(Stroke::new(1.0, Color32::from_white_alpha(20)))
                .inner_margin(Margin::same(0)),
        )
        .show(ctx, |ui| {
            ui.set_width(600.0);
            let hint = "Open a file, jump to an agent, or describe a task";
            let r = Frame::new()
                .inner_margin(Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.add(
                        TextEdit::singleline(&mut bar.query)
                            .hint_text(hint)
                            .font(FontId::proportional(16.0))
                            .frame(Frame::NONE)
                            .desired_width(f32::INFINITY),
                    )
                })
                .inner;
            r.request_focus();
            if r.changed() {
                bar.sel = 0;
            }
            ui.painter().line_segment(
                [ui.min_rect().left_bottom(), ui.min_rect().right_bottom()],
                Stroke::new(1.0, Color32::from_white_alpha(14)),
            );
            ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                ui.add_space(4.0);
                let mut group = "";
                for (i, o) in opts.iter().enumerate() {
                    if o.group != group {
                        group = o.group;
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            section_label(ui, &group.to_uppercase());
                        });
                    }
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                    let rect = rect.shrink2(vec2(6.0, 0.0));
                    if i == bar.sel {
                        ui.painter()
                            .rect_filled(rect, 6.0, ACCENT.gamma_multiply(0.22));
                        if up || down {
                            ui.scroll_to_rect(rect, None);
                        }
                    }
                    if resp.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                        bar.sel = i;
                    }
                    if resp.clicked() {
                        chosen = Some(i);
                    }
                    let y = rect.center().y;
                    let p = ui.painter();
                    p.text(
                        pos2(rect.left() + 12.0, y),
                        Align2::CENTER_CENTER,
                        &o.icon,
                        FontId::monospace(12.0),
                        MUTED,
                    );
                    let hint = p.text(
                        pos2(rect.right() - 10.0, y),
                        Align2::RIGHT_CENTER,
                        &o.hint,
                        FontId::proportional(11.5),
                        MUTED,
                    );
                    // Long paths stop short of the hint instead of running under it.
                    let clip = Rect::from_min_max(rect.min, pos2(hint.left() - 16.0, rect.max.y));
                    ui.painter_at(clip).text(
                        pos2(rect.left() + 28.0, y),
                        Align2::LEFT_CENTER,
                        &o.text,
                        FontId::proportional(13.5),
                        TEXT,
                    );
                }
                if opts.is_empty() {
                    ui.label(
                        RichText::new("   No matches. Type a sentence to start an agent with it.")
                            .color(MUTED),
                    );
                }
                ui.add_space(6.0);
            });
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(
                    RichText::new("↑↓ choose   ↵ open   esc close")
                        .small()
                        .color(MUTED),
                );
            });
            ui.add_space(6.0);
        });
    if esc || m.should_close() {
        app.bar = None;
        app.focus_pane = true;
        return;
    }
    let Some(o) = chosen.and_then(|i| opts.into_iter().nth(i)) else {
        return;
    };
    match o.act {
        BarAct::Start(task) if task.is_empty() => return,
        BarAct::Start(task) => acts.push(Action::StartAgent(task)),
        BarAct::Item(id) => acts.push(Action::OpenTerm(id)),
        BarAct::File(f) => acts.push(Action::OpenFile(f, None)),
        BarAct::Snippet(i) => acts.push(Action::RunSnippet(i)),
        BarAct::Run(f) => f(app, acts),
    }
    app.bar = None;
    app.focus_pane = true;
}

fn shortcuts_sheet(ctx: &egui::Context, app: &mut App) {
    if !app.show_keys {
        return;
    }
    let m = mod_label();
    let c = cmd_label();
    let rows: Vec<(String, &str)> = vec![
        (
            format!("{m}P"),
            "Command bar: open a file, jump to an agent, start a task",
        ),
        (format!("{m}N"), "New agent (asks for the task)"),
        (format!("{m}T"), "New terminal"),
        (format!("{m}A"), "Next agent that needs you"),
        (
            format!("{m}Y  /  {m}R"),
            "Approve / reject the pending command",
        ),
        (format!("{m}G"), "Changes (again to go back)"),
        (format!("{m}O"), "Files (again to go back)"),
        (format!("{m}U"), "Undo history"),
        (format!("{m}D"), "Split: a second card beside this one"),
        (format!("{m}W"), "Close the focused card or the open file"),
        (
            format!("{m}B"),
            "Broadcast: typing goes to every terminal card",
        ),
        (
            format!("Ctrl+Tab  /  {m}]"),
            "Next agent or terminal (Shift / [ for previous)",
        ),
        (format!("{c}1–9"), "Switch project"),
        (format!("{m}J  /  {m}K"), "Previous / next project"),
        (format!("{m}E"), "Hide or show the sidebar"),
        (
            format!("{c}=  {c}-  {c}0"),
            "Font size bigger / smaller / reset",
        ),
        (format!("{c},"), "Settings"),
        (format!("{m}/"), "These shortcuts"),
        (
            format!("{c}F"),
            "Terminal: find in scrollback (Enter / Shift+Enter step)",
        ),
        (
            format!("{c}↑  /  {c}↓"),
            "Terminal: previous / next command",
        ),
        (format!("{c}K"), "Terminal: clear"),
        (
            format!("{c}-click"),
            "Terminal: open a link, or a file path in the editor",
        ),
        ("Shift+PageUp / Down".into(), "Terminal: scroll back"),
        ("j  k".into(), "Changes: move (the diff follows)"),
        ("space  a".into(), "Changes: stage the file / everything"),
        (
            "e  c  d".into(),
            "Changes: edit / commit / discard (asks first)",
        ),
        ("s  u".into(), "Diff: stage / unstage the hunk"),
    ];
    let mut rows = rows;
    let tm = app.settings.tile_mod;
    if tm != crate::tiling::TileMod::Off {
        for (shift, key, what) in crate::tiling::HELP {
            rows.push((crate::tiling::chord(tm, *shift, key), what));
        }
    }
    let mut open = true;
    egui::Window::new("Keyboard shortcuts")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 80.0))
        .show(ctx, |ui| {
            egui::Grid::new("keys")
                .num_columns(2)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    for (k, what) in &rows {
                        ui.label(RichText::new(k).monospace().color(TEXT));
                        ui.label(RichText::new(*what).color(MUTED));
                        ui.end_row();
                    }
                });
        });
    if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
        app.show_keys = false;
    }
}

// ── settings ────────────────────────────────────────────────────────────

fn toggle_settings(app: &mut App) {
    app.settings_ui = match app.settings_ui.take() {
        Some(_) => None,
        None => Some(SettingsUi::default()),
    };
}

/// The system folder picker, starting at what's typed when that's a folder.
/// It's modal and blocks the frame, as the macOS sheet does anyway.
fn pick_folder(typed: &str) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new().set_title("Choose a folder");
    let start = paths::expand(typed.trim());
    let start = if start.is_dir() {
        Some(start)
    } else {
        start
            .parent()
            .filter(|p| p.is_dir())
            .map(|p| p.to_path_buf())
    };
    if let Some(dir) = start.or_else(|| std::env::var_os("HOME").map(PathBuf::from)) {
        d = d.set_directory(dir);
    }
    d.pick_folder()
}

/// `/Users/me/code/x` as `~/code/x`.
fn tilde(p: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(h) if p.starts_with(&h) => {
            let rest = p.strip_prefix(&h).unwrap_or(p);
            if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            }
        }
        _ => p.display().to_string(),
    }
}

fn settings_window(ctx: &egui::Context, app: &mut App) {
    let Some(sui) = app.settings_ui.as_mut() else {
        return;
    };
    // Scanning reads every font file on the machine, so it runs once per
    // opening, off the UI thread.
    if sui.mono_fonts.is_none() && sui.font_scan.is_none() {
        let (tx, rx) = std::sync::mpsc::channel();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(theme::scan_mono_fonts());
            c.request_repaint();
        });
        sui.font_scan = Some(rx);
    }
    if let Some(list) = sui.font_scan.as_ref().and_then(|rx| rx.try_recv().ok()) {
        sui.mono_fonts = Some(list);
        sui.font_scan = None;
    }
    let mut open = true;
    let agents: Vec<String> = app
        .cfg
        .agents
        .keys()
        .filter(|a| *a != "shell")
        .cloned()
        .collect();
    let s = &mut app.settings;
    let font_error = app.font_error.as_deref();
    let started_transparent = app.transparent;
    egui::Window::new("Settings")
        .anchor(Align2::CENTER_TOP, vec2(0.0, 50.0))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .default_height(620.0)
        .show(ctx, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                settings_heading(ui, "Editor");
                ui.label(
                    RichText::new("What happens when you click a file in Files or Changes, or press e in Changes.")
                        .small()
                        .color(MUTED),
                );
                for (group, kinds) in [
                    ("Built in", &["builtin"][..]),
                    ("Terminal editors, run inside muster", &["nvim", "hx", "vim", "micro", "nano", "$EDITOR"][..]),
                    ("Desktop apps", &["code", "cursor", "zed", "subl"][..]),
                ] {
                    ui.label(RichText::new(group).color(MUTED));
                    ui.horizontal_wrapped(|ui| {
                        for k in kinds {
                            let label = crate::editors::PRESETS
                                .iter()
                                .find(|(v, _)| v == k)
                                .map(|(_, l)| *l)
                                .unwrap_or(k);
                            if ui.selectable_label(s.editor == *k, label).clicked() {
                                s.editor = k.to_string();
                            }
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Or a command");
                    let preset = crate::editors::PRESETS.iter().any(|(v, _)| *v == s.editor);
                    let mut custom = if preset { String::new() } else { s.editor.clone() };
                    if ui
                        .add(TextEdit::singleline(&mut custom).desired_width(200.0).hint_text("emacs -nw, idea, …"))
                        .changed()
                    {
                        s.editor = if custom.trim().is_empty() { "builtin".into() } else { custom };
                    }
                });
                let what = match crate::editors::resolve(&s.editor) {
                    crate::editors::Kind::Builtin => "Clicking a file opens it in the card, in muster's own editor. It reloads when an agent edits the file.".to_string(),
                    crate::editors::Kind::Terminal(c) => format!("Clicking a file runs `{c}` inside the card, one per agent or terminal. Opening another file sends it to the same {c}."),
                    crate::editors::Kind::Gui(c) => format!("Clicking a file opens it in `{c}` at the right line. muster shows it read-only alongside and reloads on save."),
                };
                Frame::new()
                    .fill(Color32::from_black_alpha(60))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(what).color(TEXT));
                    });

                ui.add_space(8.0);
                settings_heading(ui, "New agents");
                ui.horizontal(|ui| {
                    ui.label("Agent");
                    egui::ComboBox::from_id_salt("default-agent")
                        .selected_text(s.default_agent.clone())
                        .show_ui(ui, |ui| {
                            for a in agents.iter() {
                                ui.selectable_value(&mut s.default_agent, a.clone(), a);
                            }
                        });
                });
                ui.checkbox(&mut s.worktree, "Give each agent its own branch and worktree");
                ui.checkbox(
                    &mut s.system_notifications,
                    "Notify me when an agent needs me or finishes while muster is in the background",
                );

                ui.add_space(8.0);
                settings_heading(ui, "Font");
                ui.horizontal(|ui| {
                    ui.label("Font");
                    let current = s.font.as_ref().map_or("System monospace", |f| f.name.as_str()).to_owned();
                    egui::ComboBox::from_id_salt("term-font")
                        .selected_text(current)
                        .width(260.0)
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.font, None, "System monospace");
                            match &sui.mono_fonts {
                                Some(list) => {
                                    for f in list {
                                        ui.selectable_value(&mut s.font, Some(f.clone()), &f.name);
                                    }
                                }
                                None => {
                                    ui.label(RichText::new("Scanning installed fonts…").color(MUTED));
                                }
                            }
                        });
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Or a font file");
                    let r = ui.add(
                        TextEdit::singleline(&mut sui.font_path)
                            .desired_width(260.0)
                            .hint_text("~/Library/Fonts/MyMono-Regular.ttf"),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let mut chosen = false;
                    if ui.button("Choose…").clicked() {
                        let start = paths::expand(sui.font_path.trim());
                        let mut d = rfd::FileDialog::new()
                            .set_title("Choose a font file")
                            .add_filter("Fonts", &["ttf", "otf", "ttc"]);
                        if let Some(dir) = start.parent().filter(|d| d.is_dir()) {
                            d = d.set_directory(dir);
                        }
                        if let Some(p) = d.pick_file() {
                            sui.font_path = p.display().to_string();
                            chosen = true;
                        }
                    }
                    if (chosen || ui.button("Use").clicked() || enter)
                        && !sui.font_path.trim().is_empty()
                    {
                        match theme::term_font_from_path(&paths::expand(sui.font_path.trim())) {
                            Ok(f) => {
                                s.font = Some(f);
                                sui.font_path_error = None;
                            }
                            Err(e) => sui.font_path_error = Some(e),
                        }
                    }
                });
                if sui.font_weights.as_ref().is_none_or(|(f, _)| *f != s.font) {
                    sui.font_weights = Some((s.font.clone(), theme::available_weights(s.font.as_ref())));
                }
                let weights = sui.font_weights.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    let show = |w: u16| format!("{} {w}", theme::weight_name(w));
                    ui.label("Weight");
                    egui::ComboBox::from_id_salt("font-weight")
                        .selected_text(show(s.font_weight))
                        .show_ui(ui, |ui| {
                            for &w in &weights {
                                ui.selectable_value(&mut s.font_weight, w, show(w));
                            }
                        });
                    ui.add_space(12.0);
                    ui.label("Bold text");
                    egui::ComboBox::from_id_salt("bold-weight")
                        .selected_text(show(s.bold_weight))
                        .show_ui(ui, |ui| {
                            for &w in weights.iter().filter(|&&w| w > s.font_weight) {
                                ui.selectable_value(&mut s.bold_weight, w, show(w));
                            }
                        });
                });
                if weights.iter().all(|&w| w <= s.font_weight) {
                    ui.label(
                        RichText::new("This font has nothing heavier, so bold text is thickened instead.")
                            .small()
                            .color(MUTED),
                    );
                }
                if let Some(e) = sui.font_path_error.as_deref().or(font_error) {
                    ui.label(RichText::new(e).small().color(RED));
                }
                ui.horizontal(|ui| {
                    ui.label("Size");
                    if ui.small_button("−").on_hover_text(format!("{}-", cmd_label())).clicked() {
                        s.bump_font(-1.0);
                    }
                    ui.add(egui::Slider::new(&mut s.font_size, cfg::FONT_SIZE_MIN..=cfg::FONT_SIZE_MAX).step_by(0.5));
                    if ui.small_button("+").on_hover_text(format!("{}=", cmd_label())).clicked() {
                        s.bump_font(1.0);
                    }
                    if s.font_size != cfg::FONT_SIZE_DEFAULT && ui.small_button("Reset").clicked() {
                        s.font_size = cfg::FONT_SIZE_DEFAULT;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Line height");
                    ui.add(egui::Slider::new(&mut s.line_height, 1.0..=1.8).step_by(0.05));
                });
                ui.checkbox(&mut s.bold_is_bright, "Draw bold text in bright colours");
                ui.checkbox(&mut s.copy_on_select, "Copy text when you select it");
                let sch = theme::scheme(&s.scheme);
                Frame::new()
                    .fill(sch.bg)
                    .stroke(Stroke::new(1.0, SEL))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("The quick brown fox  0O 1lI {}[]() => != 中文")
                                .font(theme::term_font(s.font_size))
                                .color(sch.fg),
                        );
                        let bold = theme::term_bold_font(s.font_size).unwrap_or_else(|| theme::term_font(s.font_size));
                        ui.label(
                            RichText::new("Bold: error[E0308] warning ✔ passed ✖ failed")
                                .font(bold)
                                .color(if s.bold_is_bright { sch.ansi[9] } else { sch.ansi[1] }),
                        );
                    });

                ui.add_space(8.0);
                settings_heading(ui, "Cursor");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.cursor, CursorStyle::Block, "Block");
                    ui.selectable_value(&mut s.cursor, CursorStyle::Underline, "Underline");
                    ui.selectable_value(&mut s.cursor, CursorStyle::Bar, "Bar");
                    ui.add_space(12.0);
                    ui.checkbox(&mut s.cursor_blink, "Blink");
                });

                ui.add_space(8.0);
                settings_heading(ui, "Snippets");
                ui.label(
                    RichText::new("Commands you run often. Run one from the command bar or a terminal's right-click menu; while broadcasting it goes to every terminal card.")
                        .small()
                        .color(MUTED),
                );
                let mut remove = None;
                for (i, sn) in s.snippets.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(TextEdit::singleline(&mut sn.name).hint_text("name").desired_width(140.0));
                        ui.add(
                            TextEdit::multiline(&mut sn.body)
                                .hint_text("cargo test -p muster")
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(1)
                                .desired_width(ui.available_width() - 40.0),
                        );
                        if ui.small_button("×").on_hover_text("Delete").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    s.snippets.remove(i);
                }
                if ui.button("+ Add snippet").clicked() {
                    s.snippets.push(cfg::Snippet::default());
                }

                ui.add_space(8.0);
                settings_heading(ui, "Tabs and cards");
                ui.horizontal(|ui| {
                    ui.label("Tabs along the");
                    ui.selectable_value(&mut s.tabs_bottom, false, "Top");
                    ui.selectable_value(&mut s.tabs_bottom, true, "Bottom");
                });
                ui.horizontal(|ui| {
                    ui.label("Card keys use");
                    for m in crate::tiling::TileMod::ALL {
                        ui.selectable_value(&mut s.tile_mod, m, m.label());
                    }
                });
                if s.tile_mod != crate::tiling::TileMod::Off {
                    egui::Grid::new("tile-keys").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                        for (shift, key, what) in crate::tiling::HELP {
                            ui.label(
                                RichText::new(crate::tiling::chord(s.tile_mod, *shift, key))
                                    .monospace()
                                    .color(TEXT),
                            );
                            ui.label(RichText::new(*what).color(MUTED));
                            ui.end_row();
                        }
                    });
                    ui.label(
                        RichText::new("These chords are taken before the terminal sees them, so the shell no longer gets them as Meta keys.")
                            .small()
                            .color(MUTED),
                    );
                }
                ui.add_space(8.0);
                settings_heading(ui, "Shell integration");
                ui.label(
                    RichText::new("Each command you run gets a mark in the terminal's margin (⌘↑ / ⌘↓ jump between them). Paste this into ~/.zshrc or ~/.bashrc and the marks turn green or red with the command's exit status.")
                        .small()
                        .color(MUTED),
                );
                ui.horizontal(|ui| {
                    if ui.button("Copy snippet").clicked() {
                        ui.ctx().copy_text(crate::find::SHELL_INTEGRATION.to_string());
                    }
                });
                ui.add_space(8.0);
                settings_heading(ui, "Colours");
                let gap = 8.0;
                let ncols = ((ui.available_width() + gap) / (160.0 + gap)).floor().max(1.0) as usize;
                let cw = (ui.available_width() - gap * (ncols - 1) as f32) / ncols as f32;
                for row in theme::SCHEMES.chunks(ncols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for sch in row {
                            let (rect, resp) = ui.allocate_exact_size(vec2(cw, 66.0), Sense::click());
                            let p = ui.painter();
                            p.rect_filled(rect, 4.0, sch.bg);
                            if s.scheme == sch.name {
                                p.rect_stroke(rect, 4.0, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
                            } else if resp.hovered() {
                                p.rect_stroke(rect, 4.0, Stroke::new(1.0, MUTED), egui::StrokeKind::Outside);
                            }
                            p.text(rect.min + vec2(8.0, 7.0), Align2::LEFT_TOP, sch.name, FontId::proportional(12.0), sch.fg);
                            p.text(rect.min + vec2(8.0, 25.0), Align2::LEFT_TOP, "$ ls -la", theme::term_font(11.0), sch.ansi[2]);
                            for (i, col) in sch.ansi[..8].iter().enumerate() {
                                let r = Rect::from_min_size(rect.min + vec2(8.0 + i as f32 * 14.0, 46.0), vec2(10.0, 10.0));
                                p.rect_filled(r, 2.0, *col);
                            }
                            if resp.clicked() {
                                s.scheme = sch.name.to_string();
                            }
                        }
                    });
                }

                ui.add_space(8.0);
                settings_heading(ui, "Window");
                if ui.checkbox(&mut s.see_through, "See-through window").changed()
                    && s.see_through
                    && s.opacity >= 1.0
                {
                    // Fully opaque would make the switch look like it did nothing.
                    s.opacity = 0.85;
                }
                if s.see_through != started_transparent {
                    ui.label(RichText::new("Restart muster to apply this.").small().color(AMBER));
                }
                ui.add_enabled_ui(s.see_through, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Background opacity");
                        ui.add(
                            egui::Slider::new(&mut s.opacity, 0.3..=1.0)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                        );
                    });
                    if cfg!(target_os = "macos") {
                        ui.horizontal(|ui| {
                            ui.label("Blur");
                            ui.add(egui::Slider::new(&mut s.blur, 0.0..=cfg::BLUR_MAX).step_by(1.0).suffix(" px"));
                        });
                    }
                });
                ui.label(
                    RichText::new("How much of the colour theme's background covers the desktop. Dialogs stay solid, and so do cells a program colours itself.")
                        .small()
                        .color(MUTED),
                );
            });
        });
    if !open {
        app.settings_ui = None;
    }
}

fn settings_heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).strong().color(ACCENT));
    ui.add_space(2.0);
}

/// Where typing in `id` goes: just there, or every terminal card while broadcasting.
fn typing_targets(app: &App, id: &str) -> Vec<String> {
    let mut v = vec![id.to_string()];
    if app.broadcast {
        for t in app.terminal_cards() {
            if !v.contains(&t) {
                v.push(t);
            }
        }
    }
    v
}

fn rel_from_repo(app: &App, f: &str) -> String {
    let dir = app.ctx_dir();
    let root = git::root(&dir).unwrap_or_else(|| dir.clone());
    let abs = root.join(f);
    abs.strip_prefix(&dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| f.to_string())
}

fn apply(app: &mut App, a: Action) {
    match a {
        Action::OpenTerm(id) => app.open_item(&id),
        Action::OpenFile(f, line) => {
            // Diff paths are repo-relative; tree paths are dir-relative.
            let rel = if app.ctx_dir().join(&f).exists() {
                f
            } else {
                rel_from_repo(app, &f)
            };
            app.open_file(&rel, line);
        }
        Action::OpenDiff(p) => app.open_diff(p),
        Action::ShowMain(src) => app.show_main(src),
        Action::Show(t) => {
            if let Some(task) = app.tasks.get(&t.item) {
                let p = app.project_of(task);
                if app.project.as_deref() != Some(p.as_str()) {
                    app.project = Some(p);
                    app.cards.truncate(1);
                    app.active = 0;
                }
            }
            app.show(t);
        }
        // "id" starts a rename; "id\0text" updates what's typed so far.
        Action::StartRename(s) => {
            app.renaming = Some(match s.split_once('\u{0}') {
                Some((id, text)) => (id.to_string(), text.to_string()),
                None => {
                    app.rename_in_tab = false;
                    let text = app.tasks.get(&s).map(|t| app.label(t)).unwrap_or_default();
                    (s, text)
                }
            });
        }
        Action::StartRenameTab(id) => {
            let text = app.tasks.get(&id).map(|t| app.label(t)).unwrap_or_default();
            app.renaming = Some((id, text));
            app.rename_in_tab = true;
        }
        Action::ToggleChild(c) => app.toggle_child(c),
        Action::Rename(id, text) => {
            app.renaming = None;
            let text = text.trim().to_string();
            let original = app
                .tasks
                .get(&id)
                .map(|t| t.name.clone())
                .unwrap_or_default();
            if text.is_empty() || text == original {
                app.settings.labels.remove(&id);
            } else {
                app.settings.labels.insert(id, text);
            }
            app.focus_pane = true;
        }
        Action::SetProject(root) => {
            if app.project.as_deref() != Some(root.as_str()) {
                app.project = Some(root);
                app.cards.truncate(1);
                app.active = 0;
                app.zoom = false;
                match project_ids(app).first().cloned() {
                    Some(id) => app.open_item(&id),
                    None => app.cards.clear(),
                }
            }
        }
        Action::OpenBar(task) => {
            let _ = task;
            app.bar = Some(CmdBar::default());
            // Its file search needs the file list.
            app.refresh_git_now();
        }
        Action::CloseSide(side) => app.close_side(side),
        Action::CloseTerminal(id) => {
            if let Some(i) = app.tabs.iter().position(|t| t.id == id) {
                app.close_tab(i, false);
            } else if let Some(c) = &app.client {
                c.send(muster_core::proto::Request::Remove {
                    id,
                    prune_worktree: false,
                });
            }
        }
        Action::CloseFile(item, path) => {
            let it = app.item_mut(&item);
            if it.files.iter().any(|e| e.path == path && e.dirty) {
                app.notify("save or revert the file before closing it", true);
                return;
            }
            it.opened.retain(|p| *p != path);
            it.files.retain(|e| e.path != path);
            it.file = 0;
            let back = if item_project(&item).is_some() {
                Child::Files
            } else {
                Child::Terminal
            };
            for t in app.cards.iter_mut() {
                if t.item == item && t.child == Child::File(path.clone()) {
                    t.child = back.clone();
                }
            }
        }
        Action::FocusPane(p) => app.focus_card(p),
        Action::Split => app.toggle_split(),
        Action::NewShell => app.new_shell(),
        Action::NewShellIn(root) => app.new_shell_in(PathBuf::from(root)),
        Action::FoldProject(root) => {
            let f = &mut app.settings.folded_projects;
            if !f.remove(&root) {
                f.insert(root);
            }
        }
        Action::ToggleAgentSort => app.settings.agents_by_recent = !app.settings.agents_by_recent,
        Action::NewCompanion(id) => app.new_companion(&id),
        Action::OpenBeside(id) => app.open_beside(&id),
        Action::NewAgent => app.open_new_agent(""),
        // A task typed into the command bar: name it before it starts.
        Action::StartAgent(task) => app.open_new_agent(&task),
        Action::NewProject => app.open_new_project(),
        Action::Settings => {
            if app.settings_ui.is_none() {
                app.settings_ui = Some(SettingsUi::default());
            }
        }
        Action::Shortcuts => app.show_keys = true,
        Action::Kill(id) => app.kill(&id),
        Action::Remove(id) => {
            let prune = app.tasks.get(&id).and_then(|t| t.branch.as_ref()).is_some();
            let text = if prune {
                format!(
                    "Remove {id} and delete its worktree?\nIts branch and undo snapshots are kept."
                )
            } else {
                format!("Remove {id}?")
            };
            app.confirm = Some(Confirm::Remove { id, prune, text });
        }
        Action::Merge => app.merge_prompt(),
        Action::Stage(p) => app.git_op(|d| git::stage_file(d, &p).map(|_| format!("staged {p}"))),
        Action::Unstage(p) => {
            app.git_op(|d| git::unstage_file(d, &p).map(|_| format!("unstaged {p}")))
        }
        Action::StageAll => app.git_op(|d| git::stage_all(d).map(|_| "staged everything".into())),
        Action::UnstageAll => {
            app.git_op(|d| git::unstage_file(d, ".").map(|_| "unstaged everything".into()))
        }
        Action::StartCommit => {
            if app.changes.iter().any(|c| c.staged()) {
                app.committing = true;
                app.focus_pane = false;
            } else {
                app.notify(
                    "nothing staged: space stages a file, a stages everything",
                    true,
                );
            }
        }
        Action::Commit => {
            let msg = app.commit_msg.trim().to_string();
            app.git_op(|d| git::commit(d, &msg).map(|m| format!("committed {m}")));
            if app.notice.as_ref().is_some_and(|n| !n.2) {
                app.commit_msg.clear();
            }
        }
        Action::Discard(c) => {
            let text = format!("Discard all changes to {}?\n(muster's undo timeline still has them if this is an agent worktree.)", c.path);
            app.confirm = Some(Confirm::Discard {
                dir: app.ctx_dir(),
                change: c,
                text,
            });
        }
        Action::Checkout(b) => app.git_op(|d| git::checkout(d, &b)),
        Action::Decide(id, allow) => app.decide(id, allow),
        Action::NextAttention => app.next_attention(),
        Action::Revert(task, s) => {
            let text = format!("Revert {task}'s files to #{} \"{}\"?\nA safety snapshot is taken first, so you can undo this.", s.seq, s.label);
            app.confirm = Some(Confirm::Revert {
                task,
                snapshot: s.id,
                text,
            });
        }
        Action::Checkpoint(task) => app.checkpoint(&task),
        Action::StageHunk(h) => {
            app.git_op(|d| git::stage_hunk(d, &h).map(|_| "staged hunk".into()))
        }
        Action::UnstageHunk(h) => {
            app.git_op(|d| git::unstage_hunk(d, &h).map(|_| "unstaged hunk".into()))
        }
        Action::StageFileFromDiff(f) => {
            let rel = rel_from_repo(app, &f);
            app.git_op(|d| git::stage_file(d, &rel).map(|_| format!("staged {rel}")));
        }
        Action::UnstageFileFromDiff(f) => {
            let rel = rel_from_repo(app, &f);
            app.git_op(|d| git::unstage_file(d, &rel).map(|_| format!("unstaged {rel}")));
        }
        Action::Input(id, data) => app.send_input(&id, data),
        Action::Typed(id, data) => {
            for to in typing_targets(app, &id) {
                app.send_input(&to, data.clone());
            }
        }
        Action::Paste(id, text) => {
            // Each terminal wraps it in its own bracketed-paste mode.
            for to in typing_targets(app, &id) {
                if let Some(b) = app.terms.get(&to).map(|t| t.view.paste_bytes(&text)) {
                    app.send_input(&to, b);
                }
            }
        }
        Action::Answer(id, b) => app.send_input(&id, b),
        Action::RunSnippet(i) => {
            let Some(sn) = app.settings.snippets.get(i).cloned() else {
                return;
            };
            let Some(Target {
                item,
                child: Child::Terminal,
            }) = app.target().cloned()
            else {
                return app.notify("focus a terminal to run a snippet in it", true);
            };
            for to in typing_targets(app, &item) {
                let mut b = app
                    .terms
                    .get(&to)
                    .map(|t| t.view.paste_bytes(sn.body.trim_end()))
                    .unwrap_or_default();
                b.push(b'\r');
                app.send_input(&to, b);
            }
        }
        Action::Resize(id, rows, cols) => app.sync_term(&id, rows, cols),
        Action::SwapCards(a, b) => app.swap_cards(a, b),
        Action::TileOnTab(id, onto) => {
            let at = match app
                .cards
                .iter()
                .position(|t| t.item == onto && t.child == Child::Terminal)
            {
                Some(i) => i,
                // Not on screen yet: it takes the focused card, the dragged
                // one goes beside it.
                None => {
                    app.show(Target {
                        item: onto,
                        child: Child::Terminal,
                    });
                    app.active
                }
            };
            app.tile_beside(&id, at);
        }
        Action::TileOnCard(id, at) => app.tile_beside(&id, at),
        Action::ToggleZoom => {
            if app.split() {
                app.zoom = !app.zoom;
            }
        }
        Action::ToggleBroadcast => app.broadcast = !app.broadcast,
        Action::Copy(text) => {
            app.ctx.copy_text(text);
            app.notify("copied", false);
        }
        Action::External(path, line) => app.open_external(&path, line),
        Action::ToggleDir(rel) => app.tree.toggle(&rel),
    }
}
