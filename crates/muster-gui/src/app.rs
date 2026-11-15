//! Application state and actions. Drawing lives in `views.rs`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use muster_core::client::Client;
use muster_core::config::Config;
use muster_core::editor::Editor;
use muster_core::git::{self, Branch, Change, Commit, DiffMode, ParsedDiff};
use muster_core::proto::{
    Approval, ApprovalSource, NewTask, Push, Request, Response, SnapshotInfo, TaskInfo, TaskStatus,
};
use muster_core::tree::Tree;

use crate::net::{self, Ev};
use crate::settings::{Settings, TermFont};
use crate::theme::{self, FontSet};

/// What a card shows for an agent or terminal: its PTY, its git changes, its
/// file tree, or one file it has opened.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Child {
    Terminal,
    Changes,
    Files,
    File(PathBuf),
}

/// One card's content: which task, and which of its children.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Target {
    pub item: String,
    pub child: Child,
}

/// Per-task UI state, created the first time the task is shown.
pub struct Item {
    pub id: String,
    /// Children listed under its sidebar row.
    pub expanded: bool,
    /// Files opened from it, in the order opened (sidebar rows).
    pub opened: Vec<PathBuf>,
    /// Built-in editor buffers for those files.
    pub files: Vec<Editor>,
    pub file: usize,
    pub git: GitView,
    pub snaps: Vec<SnapshotInfo>,
}

impl Item {
    pub fn new(id: String) -> Item {
        Item {
            id,
            expanded: false,
            opened: vec![],
            files: vec![],
            file: 0,
            git: GitView::default(),
            snaps: vec![],
        }
    }

    pub fn editor(&mut self) -> Option<&mut Editor> {
        self.files.get_mut(self.file)
    }
}

/// The command bar.
#[derive(Default)]
pub struct CmdBar {
    pub query: String,
    pub sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NewKind {
    Agent,
    Project,
}

/// The popup that names a new agent or project before it's created.
pub struct NewForm {
    pub kind: NewKind,
    pub name: String,
    /// Typed by hand, so the task no longer suggests it.
    pub name_edited: bool,
    pub task: String,
    pub agent: String,
    pub worktree: bool,
    pub dir: String,
    pub focus_set: bool,
}

/// A task name the way the daemon turns it into an id and branch.
/// Changes, Files and the files opened from them belong to a project, not to
/// one of its terminals, so they sit under an item of their own. Task ids are
/// slugs, so the prefix can't clash with one.
pub fn project_item(root: &str) -> String {
    format!("project:{root}")
}

pub fn item_project(id: &str) -> Option<&str> {
    id.strip_prefix("project:")
}

pub fn slugify(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(40).collect()
}

/// A name from the first words of a task: "Fix the login redirect" → fix-the-login-redirect.
pub fn suggest_name(task: &str) -> String {
    let words: Vec<&str> = task.split_whitespace().take(4).collect();
    slugify(&words.join(" "))
}

/// The lazygit-style panels, in the order 1–5 select them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Panel {
    Status,
    #[default]
    Files,
    Branches,
    Commits,
    Snapshots,
    /// The diff on the right.
    Main,
}

impl Panel {
    pub const LISTS: [Panel; 5] = [
        Panel::Status,
        Panel::Files,
        Panel::Branches,
        Panel::Commits,
        Panel::Snapshots,
    ];

    pub fn index(self) -> usize {
        Panel::LISTS.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// What the diff side of the git view shows.
#[derive(Clone, Debug, PartialEq)]
pub enum MainSrc {
    Changes {
        path: Option<String>,
        mode: DiffMode,
    },
    Commit(String),
    Snapshot(SnapshotInfo),
}

#[derive(Default)]
pub struct GitView {
    pub panel: Panel,
    pub sel: [usize; 5],
    pub main: Option<MainSrc>,
    pub diff: DiffView,
    /// Bumped per load so a slow, stale result can't overwrite a newer one.
    pub gen: u64,
}

#[derive(Default)]
pub struct DiffView {
    pub title: String,
    pub parsed: ParsedDiff,
    pub cursor: usize,
    pub scroll_to_cursor: bool,
    pub error: Option<String>,
    pub loading: bool,
}

/// A terminal on screen: keel's view over a local copy of the daemon's screen,
/// kept in step by the Output pushes.
pub struct TermState {
    pub view: crate::termview::TerminalView,
}

impl TermState {
    fn new(id: &str, rows: u16, cols: u16) -> TermState {
        let parser = crate::termview::new_parser(rows, cols, 10_000);
        TermState {
            view: crate::termview::TerminalView::new(
                parser,
                egui::Id::unique(("term", id)),
                cols,
                rows,
            ),
        }
    }
}

pub enum Reply {
    Snapshots {
        task: String,
        list: Vec<SnapshotInfo>,
    },
    Main {
        item: String,
        gen: u64,
        title: String,
        result: Result<ParsedDiff, String>,
    },
    Created(TaskInfo, bool),
    Notice(String, bool),
}

pub struct GitData {
    pub dir: PathBuf,
    pub changes: Result<Vec<Change>, String>,
    pub marks: Marks,
    /// Only fetched while something shows the file list.
    pub listing: Option<muster_core::tree::Listing>,
    /// How long the git and file listing took, to pace the next poll.
    pub took: Duration,
    pub branch: Option<String>,
    pub root: Option<PathBuf>,
    pub ahead: Option<usize>,
    /// Only fetched while a git view is showing.
    pub log: Option<(Vec<Branch>, Vec<Commit>)>,
}

/// Change letters for the file tree, worked out off the UI thread: drawing used
/// to scan every change for every visible folder each frame.
#[derive(Default, Debug)]
pub struct Marks {
    pub files: HashMap<String, char>,
    /// Folders with a change somewhere below them.
    pub dirs: std::collections::HashSet<String>,
}

impl Marks {
    pub fn new(changes: &[Change]) -> Marks {
        let mut m = Marks::default();
        for c in changes {
            let path = c.path.trim_end_matches('/');
            m.files.insert(path.to_string(), c.letter());
            let mut end = 0;
            while let Some(i) = path[end..].find('/') {
                end += i;
                if !m.dirs.contains(&path[..end]) {
                    m.dirs.insert(path[..end].to_string());
                }
                end += 1;
            }
        }
        m
    }
}

pub enum Confirm {
    Merge {
        root: PathBuf,
        branch: String,
        text: String,
    },
    Revert {
        task: String,
        snapshot: String,
        text: String,
    },
    Remove {
        id: String,
        prune: bool,
        text: String,
    },
    Discard {
        dir: PathBuf,
        change: Change,
        text: String,
    },
    CloseDirty {
        tab: usize,
        text: String,
    },
    RestartDaemon {
        text: String,
    },
}

impl Confirm {
    pub fn text(&self) -> &str {
        match self {
            Confirm::Merge { text, .. }
            | Confirm::Revert { text, .. }
            | Confirm::Remove { text, .. }
            | Confirm::Discard { text, .. }
            | Confirm::CloseDirty { text, .. }
            | Confirm::RestartDaemon { text } => text,
        }
    }
}

pub struct App {
    pub rt: tokio::runtime::Runtime,
    pub client: Option<Client>,
    rx: Receiver<Ev>,
    pub tx: Sender<Ev>,
    pub ctx: egui::Context,
    pub cfg: Config,
    /// Where muster was launched; the context until a terminal is focused.
    pub workspace: PathBuf,
    workspace_repo: Option<String>,
    pub tasks: BTreeMap<String, TaskInfo>,
    pub approvals: BTreeMap<u64, Approval>,
    pub terms: HashMap<String, TermState>,
    pub tabs: Vec<Item>,
    /// Index into `tabs` of the focused card's task.
    pub cur: usize,
    /// The cards on screen, laid out in a grid (keel's tiling).
    pub cards: Vec<Target>,
    /// Which card has the keyboard.
    pub active: usize,
    /// Only the active card is shown, filling the stage.
    pub zoom: bool,
    /// Where each card was drawn last frame, for moving focus by direction.
    pub card_rects: Vec<egui::Rect>,
    /// The next terminal created opens in a card of its own.
    pub open_in_new_card: bool,
    /// Typing in one terminal card goes to every terminal card.
    pub broadcast: bool,
    saved_layout: Option<crate::layout::Layout>,
    /// Terminals opened for an agent (terminal id -> agent id): listed under
    /// the agent and grouped with its project.
    pub parent_of: std::collections::BTreeMap<String, String>,
    /// The terminal being created belongs to this agent.
    pending_parent: Option<String>,
    /// The repo root whose agents the sidebar lists ("" for tasks outside any).
    pub project: Option<String>,
    pub bar: Option<CmdBar>,
    pub show_keys: bool,
    /// The naming popup for a new agent or project.
    pub new_form: Option<NewForm>,
    /// Task being renamed in the sidebar, and the text typed so far.
    pub renaming: Option<(String, String)>,
    /// The terminal last put in a card, for a project's Changes or Files to
    /// go back to.
    pub last_term: Option<String>,
    /// Relative sizes of the card grid's columns and rows, set by dragging
    /// the gaps between cards.
    pub col_w: Vec<f32>,
    pub row_w: Vec<f32>,
    /// That rename is being typed into its tab, not its sidebar row.
    pub rename_in_tab: bool,
    pub ctx_task: Option<String>,
    pub tree: Tree,
    pub changes: Vec<Change>,
    pub branches: Vec<Branch>,
    pub commits: Vec<Commit>,
    pub branch: Option<String>,
    pub ahead: Option<usize>,
    pub git_error: Option<String>,
    pub ctx_root: Option<PathBuf>,
    pub marks: Marks,
    /// The `muster` binary was rebuilt after the running daemon started.
    pub daemon_outdated: bool,
    /// Terminal agents that finished a turn while you weren't looking.
    pub done_unseen: std::collections::HashSet<String>,
    git_inflight: bool,
    last_git: Instant,
    /// Gap between git polls; grows when a repo is slow to scan.
    git_every: Duration,
    pub sidebar: bool,
    pub settings: Settings,
    saved_settings: Settings,
    pub settings_ui: Option<SettingsUi>,
    fonts: FontSet,
    applied_font: Option<(Option<TermFont>, u16, u16)>,
    pub font_error: Option<String>,
    /// Whether the window was created transparent (only possible at startup).
    pub transparent: bool,
    applied_blur: Option<i64>,
    pub commit_msg: String,
    /// The commit message popup is open.
    pub committing: bool,
    pub confirm: Option<Confirm>,
    pub notice: Option<(String, Instant, bool)>,
    /// Keyboard goes to the active pane (vs. a text field / sidebar).
    pub focus_pane: bool,
    pub find: Option<String>,
    pub goto: Option<String>,
    started: bool,
    last_tick: Instant,
    pub frame: usize,
    pub title: String,
    #[cfg(debug_assertions)]
    devshot: Option<crate::devshot::DevShot>,
}

/// State of the open settings window.
#[derive(Default)]
pub struct SettingsUi {
    pub mono_fonts: Option<Vec<TermFont>>,
    pub font_scan: Option<Receiver<Vec<TermFont>>>,
    pub font_path: String,
    pub font_path_error: Option<String>,
    pub font_weights: Option<(Option<TermFont>, Vec<u16>)>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        workspace: PathBuf,
        settings: Settings,
        transparent: bool,
    ) -> App {
        let ctx = cc.egui_ctx.clone();
        crate::views::style(&ctx);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let (tx, rx) = std::sync::mpsc::channel();
        net::start(&rt, ctx.clone(), tx.clone());
        let cfg = Config::load().unwrap_or_default();
        // Installed here, not on the first frame: egui only swaps fonts in at
        // the start of a pass, and drawing with the unregistered "terminal"
        // family panics.
        let fonts = FontSet::load();
        let want = (
            settings.font.clone(),
            settings.font_weight,
            settings.bold_weight,
        );
        let font_error = fonts.apply(&ctx, want.0.as_ref(), want.1, want.2).err();
        App {
            rt,
            client: None,
            rx,
            tx,
            ctx,
            cfg,
            ctx_root: git::root(&workspace),
            workspace_repo: git::root(&workspace).map(|p| p.display().to_string()),
            workspace,
            tasks: BTreeMap::new(),
            approvals: BTreeMap::new(),
            terms: HashMap::new(),
            tabs: vec![],
            cur: 0,
            cards: vec![],
            zoom: false,
            card_rects: vec![],
            open_in_new_card: false,
            broadcast: false,
            saved_layout: None,
            parent_of: Default::default(),
            pending_parent: None,
            active: 0,
            project: None,
            rename_in_tab: false,
            last_term: None,
            col_w: vec![],
            row_w: vec![],
            bar: None,
            show_keys: false,
            renaming: None,
            new_form: None,
            ctx_task: None,
            tree: Tree::default(),
            changes: vec![],
            branches: vec![],
            commits: vec![],
            branch: None,
            ahead: None,
            git_error: None,
            marks: Marks::default(),
            done_unseen: Default::default(),
            daemon_outdated: false,
            git_inflight: false,
            last_git: Instant::now() - Duration::from_secs(60),
            git_every: Duration::from_millis(1500),
            sidebar: true,
            saved_settings: settings.clone(),
            settings,
            settings_ui: None,
            fonts,
            applied_font: Some(want),
            font_error,
            transparent,
            applied_blur: None,
            commit_msg: String::new(),
            committing: false,
            confirm: None,
            notice: None,
            focus_pane: true,
            find: None,
            goto: None,
            started: false,
            last_tick: Instant::now(),
            frame: 0,
            title: String::new(),
            #[cfg(debug_assertions)]
            devshot: crate::devshot::DevShot::from_env(),
        }
    }

    // ── context ─────────────────────────────────────────────────────────

    pub fn agent_ids(&self) -> Vec<String> {
        // Called every frame by the agents panel, so this must not shell out to git.
        let here = &self.workspace_repo;
        let mut v: Vec<&TaskInfo> = self
            .tasks
            .values()
            .filter(|t| !crate::editors::is_editor_task(&t.id))
            .collect();
        v.sort_by_key(|t| (t.repo != *here, !is_agent(t), t.created_unix, t.id.clone()));
        v.into_iter().map(|t| t.id.clone()).collect()
    }

    /// The directory the sidebar, git panel and new terminals work in: the
    /// focused terminal's current directory. Agents stay on their worktree even
    /// if a tool inside them changes directory.
    pub fn ctx_dir(&self) -> PathBuf {
        match &self.ctx_task {
            Some(id) => self.item_dir(id),
            None => self.workspace.clone(),
        }
    }

    /// A task, or a project's Changes and Files.
    pub fn item_exists(&self, id: &str) -> bool {
        self.tasks.contains_key(id) || item_project(id).is_some()
    }

    /// The folder an item's Changes and Files look at.
    pub fn item_dir(&self, id: &str) -> PathBuf {
        match item_project(id) {
            Some(root) if !root.is_empty() => PathBuf::from(root),
            Some(_) => self.workspace.clone(),
            None => self
                .tasks
                .get(id)
                .map(task_dir)
                .unwrap_or_else(|| self.workspace.clone()),
        }
    }

    /// Where a task's Changes and Files live: an agent's own worktree, or
    /// the project for everything else.
    pub fn changes_item(&self, id: &str) -> String {
        match self.tasks.get(id) {
            Some(t) if t.agent != "shell" => id.to_string(),
            Some(t) => project_item(&self.project_of(t)),
            None => id.to_string(),
        }
    }

    pub fn set_ctx_task(&mut self, id: Option<String>) {
        if self.ctx_task != id {
            self.ctx_task = id;
            self.refresh_git_now();
        }
    }

    /// The name shown for a task: yours if you renamed it.
    pub fn label(&self, t: &TaskInfo) -> String {
        self.settings
            .labels
            .get(&t.id)
            .cloned()
            .unwrap_or_else(|| t.name.clone())
    }

    pub fn notify(&mut self, msg: impl Into<String>, error: bool) {
        self.notice = Some((msg.into(), Instant::now(), error));
    }

    // ── tabs & panes ────────────────────────────────────────────────────

    pub fn item(&mut self) -> Option<&mut Item> {
        self.tabs.get_mut(self.cur)
    }

    pub fn item_mut(&mut self, id: &str) -> &mut Item {
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            return &mut self.tabs[i];
        }
        self.tabs.push(Item::new(id.to_string()));
        self.tabs.last_mut().unwrap()
    }

    /// The card with the keyboard.
    pub fn target(&self) -> Option<&Target> {
        self.cards.get(self.active).or(self.cards.first())
    }

    pub fn split(&self) -> bool {
        self.cards.len() > 1
    }

    /// Cards showing a task's terminal, for watching and broadcasting.
    pub fn terminal_cards(&self) -> Vec<String> {
        self.cards
            .iter()
            .filter(|t| t.child == Child::Terminal)
            .map(|t| t.item.clone())
            .collect()
    }

    pub fn child(&self) -> Option<Child> {
        self.target().map(|t| t.child.clone())
    }

    /// Show `t` in the focused card.
    pub fn show(&mut self, t: Target) {
        self.item_mut(&t.item);
        if t.child == Child::Terminal {
            self.last_term = Some(t.item.clone());
        }
        if t.child == Child::Changes && self.tasks.get(&t.item).is_some_and(|x| x.agent != "shell")
        {
            self.load_snapshots(&t.item);
        }
        if t.child == Child::Terminal && self.done_unseen.remove(&t.item) {
            if let Some(x) = self.tasks.get_mut(&t.item) {
                x.attention = false;
            }
        }
        match self.cards.get_mut(self.active) {
            Some(c) => *c = t,
            None => {
                self.cards = vec![t];
                self.active = 0;
            }
        }
        self.focus_pane = true;
        self.on_active_changed();
        if matches!(self.child(), Some(Child::Files)) {
            self.refresh_git_now();
        }
        let needs_default = self.child() == Some(Child::Changes)
            && self
                .tabs
                .get(self.cur)
                .is_some_and(|it| it.git.main.is_none());
        if needs_default {
            self.select_git_default();
        }
    }

    /// Switch the focused card to `c` on the same task, or back to the
    /// terminal if it's already showing.
    pub fn toggle_child(&mut self, c: Child) {
        let Some(t) = self.target().cloned() else {
            return;
        };
        if t.child == c {
            // A project's view goes back to the terminal shown before it.
            let item = if item_project(&t.item).is_some() {
                match self
                    .last_term
                    .clone()
                    .filter(|i| self.tasks.contains_key(i))
                {
                    Some(i) => i,
                    None => return,
                }
            } else {
                t.item
            };
            return self.show(Target {
                item,
                child: Child::Terminal,
            });
        }
        let item = self.changes_item(&t.item);
        self.show(Target { item, child: c });
    }

    /// Show a task's terminal, switching project if it lives in another.
    pub fn open_item(&mut self, id: &str) {
        if let Some(p) = self.tasks.get(id).map(|t| self.project_of(t)) {
            if self.project.as_deref() != Some(p.as_str()) {
                self.project = Some(p);
                self.cards.truncate(1);
                self.active = 0;
                self.zoom = false;
            }
        }
        // Already in a card: just focus that one.
        if let Some(i) = self
            .cards
            .iter()
            .position(|t| t.item == id && t.child == Child::Terminal)
        {
            self.focus_card(i);
            return;
        }
        self.show(Target {
            item: id.to_string(),
            child: Child::Terminal,
        });
    }

    /// Keep `cur` and the git context on whatever the focused card shows.
    pub fn on_active_changed(&mut self) {
        let Some(id) = self.target().map(|t| t.item.clone()) else {
            return;
        };
        self.item_mut(&id);
        self.cur = self.tabs.iter().position(|t| t.id == id).unwrap_or(0);
        self.set_ctx_task(Some(id));
        if self.child() == Some(Child::Changes) {
            self.refresh_git_now();
        }
    }

    pub fn focus_card(&mut self, i: usize) {
        let i = i.min(self.cards.len().saturating_sub(1));
        if i != self.active {
            self.active = i;
            self.on_active_changed();
        }
        self.focus_pane = true;
    }

    /// End a plain terminal. Agents aren't closed this way; Remove them.
    pub fn close_tab(&mut self, idx: usize, force: bool) {
        let Some(it) = self.tabs.get(idx) else {
            return;
        };
        if !force {
            if let Some(e) = it.files.iter().find(|e| e.dirty) {
                self.confirm = Some(Confirm::CloseDirty {
                    tab: idx,
                    text: format!("{} has unsaved changes. Close anyway?", e.name()),
                });
                return;
            }
        }
        let id = it.id.clone();
        let is_shell = self
            .tasks
            .get(&id)
            .map(|t| t.agent == "shell")
            .unwrap_or(false);
        if let Some(c) = &self.client {
            if is_shell {
                c.send(Request::Remove {
                    id: id.clone(),
                    prune_worktree: false,
                });
            }
            // A terminal editor belongs to its task.
            let ed = crate::editors::task_id(&id);
            if self.tasks.contains_key(&ed) {
                c.send(Request::Remove {
                    id: ed,
                    prune_worktree: false,
                });
            }
        }
        if is_shell {
            self.forget_task(&id);
        }
    }

    /// Add a card beside the focused one, on what's most useful next to it:
    /// a terminal's Changes, or the terminal of whatever else is showing.
    pub fn add_card(&mut self) {
        let Some(t) = self.target().cloned() else {
            return;
        };
        let child = match t.child {
            Child::Terminal => Child::Changes,
            _ => Child::Terminal,
        };
        self.cards.insert(self.active + 1, t.clone());
        self.active += 1;
        self.zoom = false;
        self.show(Target {
            item: t.item,
            child,
        });
    }

    /// Split toggles between one card and two.
    pub fn toggle_split(&mut self) {
        if self.split() {
            let keep = self.cards[self.active].clone();
            self.cards = vec![keep];
            self.active = 0;
            self.zoom = false;
            self.on_active_changed();
        } else {
            self.add_card();
        }
        self.focus_pane = true;
    }

    pub fn close_side(&mut self, i: usize) {
        if self.cards.len() <= 1 || i >= self.cards.len() {
            return;
        }
        self.cards.remove(i);
        if self.active >= i && self.active > 0 {
            self.active -= 1;
        }
        self.active = self.active.min(self.cards.len() - 1);
        if self.cards.len() == 1 {
            self.zoom = false;
        }
        self.on_active_changed();
    }

    /// A new terminal in its own card (keel's Mod+Return).
    pub fn new_terminal_card(&mut self) {
        self.open_in_new_card = true;
        self.new_shell();
    }

    /// A terminal for running things next to an agent: in the agent's
    /// directory (its worktree, if it has one), in a card beside it.
    pub fn new_companion(&mut self, agent: &str) {
        let Some(dir) = self.tasks.get(agent).map(task_dir) else {
            return;
        };
        let mut i = self.tasks.values().filter(|t| t.agent == "shell").count() + 1;
        let mut name = format!("term-{i}");
        while self.tasks.contains_key(&name) {
            i += 1;
            name = format!("term-{i}");
        }
        self.pending_parent = Some(agent.to_string());
        self.open_in_new_card = true;
        self.create_task("shell".into(), name, None, false, dir);
    }

    /// Show a terminal beside the card it belongs with, without covering it.
    pub fn open_beside(&mut self, id: &str) {
        if let Some(i) = self
            .cards
            .iter()
            .position(|t| t.item == id && t.child == Child::Terminal)
        {
            return self.focus_card(i);
        }
        let parent = self.parent_of.get(id).cloned();
        let after = parent
            .and_then(|p| self.cards.iter().position(|t| t.item == p))
            .unwrap_or(self.active);
        self.cards.insert(
            (after + 1).min(self.cards.len()),
            Target {
                item: id.to_string(),
                child: Child::Terminal,
            },
        );
        self.active = (after + 1).min(self.cards.len() - 1);
        self.zoom = false;
        self.on_active_changed();
        self.focus_pane = true;
    }

    /// Tile `id`'s terminal in a card of its own right after card `at`. A
    /// card already showing it moves there instead.
    pub fn tile_beside(&mut self, id: &str, at: usize) {
        let mut at = at.min(self.cards.len().saturating_sub(1));
        if let Some(i) = self
            .cards
            .iter()
            .position(|t| t.item == id && t.child == Child::Terminal)
        {
            if i == at {
                return self.focus_card(i);
            }
            self.cards.remove(i);
            if i < at {
                at -= 1;
            }
        }
        let to = (at + 1).min(self.cards.len());
        self.cards.insert(
            to,
            Target {
                item: id.to_string(),
                child: Child::Terminal,
            },
        );
        self.active = to;
        self.zoom = false;
        self.on_active_changed();
        self.focus_pane = true;
    }

    /// The card next to the focused one in a direction, by where they were drawn.
    pub fn neighbour(&self, dx: f32, dy: f32) -> Option<usize> {
        let from = self.card_rects.get(self.active)?.center();
        self.card_rects
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.active)
            .filter(|(_, r)| {
                let d = r.center() - from;
                // Mostly in that direction.
                d.x * dx + d.y * dy > (d.x * dy).abs() + (d.y * dx).abs() - 1.0
                    && d.x * dx + d.y * dy > 0.0
            })
            .min_by(|(_, a), (_, b)| {
                let da = (a.center() - from).length();
                let db = (b.center() - from).length();
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
    }

    /// Swap two cards; the dragged (or keyed) one keeps the focus.
    pub fn swap_cards(&mut self, from: usize, to: usize) {
        if from < self.cards.len() && to < self.cards.len() && from != to {
            self.cards.swap(from, to);
            self.active = to;
            self.on_active_changed();
            self.focus_pane = true;
        }
    }

    pub fn swap_card(&mut self, to: usize) {
        if to < self.cards.len() && to != self.active {
            self.cards.swap(self.active, to);
            self.active = to;
            self.on_active_changed();
        }
    }

    /// Which project (repo root) a task belongs to. Shells go wherever they
    /// have cd'd to, if that's inside a known repo.
    pub fn project_of(&self, t: &TaskInfo) -> String {
        // An agent's terminal goes with the agent, even when it sits in the
        // agent's worktree outside the repo.
        if let Some(p) = self.parent_of.get(&t.id).and_then(|p| self.tasks.get(p)) {
            if p.id != t.id {
                return self.project_of(p);
            }
        }
        // A folder that isn't a repo is a project of its own.
        let start = t.repo.clone().unwrap_or_else(|| t.cwd.clone());
        if t.agent != "shell" {
            return start;
        }
        let here = task_dir(t);
        self.project_roots()
            .into_iter()
            .filter(|r| here.starts_with(r))
            .max_by_key(|r| r.len())
            .unwrap_or(start)
    }

    pub fn project_roots(&self) -> Vec<String> {
        let launch = self
            .workspace_repo
            .clone()
            .unwrap_or_else(|| self.workspace.display().to_string());
        let mut v: Vec<String> = self
            .tasks
            .values()
            .filter(|t| !crate::editors::is_editor_task(&t.id))
            .map(|t| t.repo.clone().unwrap_or_else(|| t.cwd.clone()))
            .chain(Some(launch))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Something sensible on the git view's diff side the first time it opens:
    /// everything an agent did, or a terminal's first changed file.
    fn select_git_default(&mut self) {
        let base = self
            .ctx_task
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .and_then(|t| t.base.clone());
        let first = self.changes.first().cloned();
        let (panel, src) = match (base, first) {
            (Some(b), _) => (
                Panel::Status,
                MainSrc::Changes {
                    path: None,
                    mode: DiffMode::Base(b),
                },
            ),
            (None, Some(c)) => (
                Panel::Files,
                MainSrc::Changes {
                    mode: if c.staged() && !c.unstaged() {
                        DiffMode::Staged
                    } else {
                        DiffMode::Unstaged
                    },
                    path: Some(c.path),
                },
            ),
            (None, None) => (
                Panel::Files,
                MainSrc::Changes {
                    path: None,
                    mode: DiffMode::Unstaged,
                },
            ),
        };
        if let Some(it) = self.tabs.get_mut(self.cur) {
            it.git.panel = panel;
        }
        self.show_main(src);
    }

    /// Point the current tab's git diff at `src` and load it in the background.
    pub fn show_main(&mut self, src: MainSrc) {
        let dir = self.ctx_dir();
        let Some(it) = self.tabs.get_mut(self.cur) else {
            return;
        };
        if it.git.main.as_ref() != Some(&src) {
            it.git.diff.cursor = 0;
        }
        it.git.main = Some(src.clone());
        it.git.gen += 1;
        it.git.diff.loading = true;
        let (item, gen) = (it.id.clone(), it.git.gen);
        if let MainSrc::Snapshot(snap) = &src {
            let title = format!("snapshot #{}: {}", snap.seq, snap.label);
            let item2 = item.clone();
            self.call(
                Request::Diff {
                    id: item.clone(),
                    from: Some(format!("{}^", snap.id)),
                    to: Some(snap.id.clone()),
                },
                move |r| {
                    Some(Reply::Main {
                        item: item2,
                        gen,
                        title,
                        result: match r {
                            Ok(Response::Diff(text)) => Ok(git::parse(&text)),
                            Ok(_) => Err("unexpected reply".into()),
                            Err(e) => Err(e),
                        },
                    })
                },
            );
            return;
        }
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn_blocking(move || {
            let (title, text) = match &src {
                MainSrc::Changes { path, mode } => {
                    let what = path.clone().unwrap_or_else(|| "all files".into());
                    let label = match mode {
                        DiffMode::Base(_) => "everything since the agent started".to_string(),
                        m => m.label().to_string(),
                    };
                    (
                        format!("{what} · {label}"),
                        git::diff(&dir, mode, path.as_deref()),
                    )
                }
                MainSrc::Commit(h) => (
                    format!("commit {}", &h[..h.len().min(10)]),
                    git::show(&dir, h),
                ),
                MainSrc::Snapshot(_) => unreachable!(),
            };
            let result = text.map(|t| git::parse(&t)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Ev::Reply(Reply::Main {
                item,
                gen,
                title,
                result,
            }));
            ctx.request_repaint();
        });
    }

    /// Reload the diff side if it shows the working tree (after a poll or a git op).
    pub fn reload_main(&mut self) {
        let src = self
            .tabs
            .get(self.cur)
            .and_then(|t| t.git.main.clone())
            .filter(|m| matches!(m, MainSrc::Changes { .. }));
        if let Some(src) = src {
            self.show_main(src);
        }
    }

    // ── terminals ───────────────────────────────────────────────────────

    pub fn sync_term(&mut self, id: &str, rows: u16, cols: u16) {
        let Some(client) = self.client.clone() else {
            return;
        };
        // The view resizes its own screen and asks for this through
        // TermAction::Resize; here a terminal is attached the first time it shows.
        if self.terms.contains_key(id) {
            client.send(Request::Resize {
                id: id.to_string(),
                rows,
                cols,
            });
        } else {
            self.terms
                .insert(id.to_string(), TermState::new(id, rows, cols));
            client.send(Request::Attach {
                id: id.to_string(),
                rows,
                cols,
            });
        }
    }

    /// An agent's own notifications (OSC 9 / 777 / 99), from terminals that
    /// have been on screen. Ones from the terminal you're watching are shown
    /// already, so they're dropped.
    fn drain_notifications(&mut self) {
        let mut got = vec![];
        for (id, t) in self.terms.iter() {
            let n =
                std::mem::take(&mut t.view.parser.lock().unwrap().callbacks_mut().notifications);
            if !n.is_empty() {
                got.push((id.clone(), n));
            }
        }
        for (id, msgs) in got {
            if self.watching(&id) {
                continue;
            }
            let name = self
                .tasks
                .get(&id)
                .map(|t| self.label(t))
                .unwrap_or(id.clone());
            for m in msgs {
                self.notify(format!("{name}: {m}"), false);
                self.notify_away(&name, &m);
            }
        }
    }

    /// A system notification, only while muster is in the background.
    pub fn notify_away(&self, title: &str, body: &str) {
        if self.settings.system_notifications && !self.ctx.input(|i| i.focused) {
            notify_system(title, body);
        }
    }

    pub fn send_input(&mut self, id: &str, data: Vec<u8>) {
        if let Some(c) = &self.client {
            c.send(Request::Input {
                id: id.to_string(),
                data,
            });
        }
    }

    // ── async ───────────────────────────────────────────────────────────

    pub fn call<F>(&self, req: Request, f: F)
    where
        F: FnOnce(Result<Response, String>) -> Option<Reply> + Send + 'static,
    {
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let r = client.call(req).await.map_err(|e| format!("{e:#}"));
            if let Some(reply) = f(r) {
                let _ = tx.send(Ev::Reply(reply));
                ctx.request_repaint();
            }
        });
    }

    pub fn refresh_git_now(&mut self) {
        self.last_git = Instant::now() - Duration::from_secs(60);
        self.git_inflight = false;
        self.request_git();
    }

    pub fn request_git(&mut self) {
        if self.git_inflight {
            return;
        }
        self.git_inflight = true;
        self.last_git = Instant::now();
        let dir = self.ctx_dir();
        let base = self
            .ctx_task
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .and_then(|t| t.base.clone());
        let want_log = self.cards.iter().any(|t| t.child == Child::Changes);
        // Listing every file is the expensive part in a big repo, and only
        // the Files card and the command bar's file search use it.
        let want_tree = self.bar.is_some()
            || self
                .cards
                .iter()
                .any(|t| matches!(t.child, Child::Files | Child::File(_)));
        let tx = self.tx.clone();

        self.rt.spawn_blocking(move || {
            let started = Instant::now();
            let root = git::root(&dir);
            let changes = if root.is_some() {
                git::status(&dir).map_err(|e| format!("{e:#}"))
            } else {
                Ok(vec![])
            };
            let marks = changes.as_deref().map(Marks::new).unwrap_or_default();
            let listing = want_tree.then(|| {
                let (files, truncated) = muster_core::tree::list(&dir);
                muster_core::tree::build(files, truncated)
            });
            let branch = git::current_branch(&dir);
            let ahead = base.map(|b| git::ahead_count(&dir, &b));
            let log = (want_log && root.is_some()).then(|| {
                (
                    git::branches(&dir).unwrap_or_default(),
                    git::log(&dir, "HEAD", 200).unwrap_or_default(),
                )
            });
            let _ = tx.send(Ev::Git(GitData {
                dir,
                changes,
                marks,
                listing,
                took: started.elapsed(),
                branch,
                root,
                ahead,
                log,
            }));
            // No repaint: the result is picked up on the next frame, at most
            // a couple of seconds away, so an idle window isn't woken twice.
        });
    }

    pub fn load_snapshots(&self, task: &str) {
        let t = task.to_string();
        self.call(Request::Snapshots { id: t.clone() }, move |r| match r {
            Ok(Response::Snapshots(list)) => Some(Reply::Snapshots { task: t, list }),
            Err(e) => Some(Reply::Notice(e, true)),
            _ => None,
        });
    }

    // ── event pump (called every frame) ─────────────────────────────────

    pub fn pump(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ev::Connected {
                    client,
                    tasks,
                    approvals,
                } => {
                    self.client = Some(client);
                    self.tasks = tasks.into_iter().map(|t| (t.id.clone(), t)).collect();
                    self.approvals = approvals.into_iter().map(|a| (a.id, a)).collect();
                    // Re-attach every terminal we show.
                    self.terms.clear();
                    if !self.started {
                        self.started = true;
                        self.initial_tabs();
                    } else {
                        // A restarted daemon has none of the old tasks; their
                        // tabs would sit there as "(ended)".
                        let gone: Vec<String> = self
                            .tabs
                            .iter()
                            .filter(|t| !self.tasks.contains_key(&t.id))
                            .map(|t| t.id.clone())
                            .collect();
                        for id in gone {
                            self.forget_task(&id);
                        }
                        if self.tabs.is_empty() {
                            self.new_shell();
                        }
                    }
                }
                Ev::Disconnected => {
                    self.client = None;
                    self.notify("lost connection to the muster daemon — reconnecting…", true);
                }
                Ev::Push(p) => self.on_push(p),
                Ev::Reply(r) => self.on_reply(r),
                Ev::Git(g) => self.on_git(g),
            }
        }
        if self.last_tick.elapsed() >= Duration::from_millis(250) {
            self.last_tick = Instant::now();
            self.tick();
        }
    }

    /// Put back the layout from the last launch, for the tasks still running.
    fn restore_layout(&mut self) -> bool {
        let Some(l) = crate::layout::Layout::load() else {
            return false;
        };
        let cards: Vec<Target> = l
            .cards
            .iter()
            .filter(|t| self.item_exists(&t.item))
            .cloned()
            .collect();
        if cards.is_empty() {
            return false;
        }
        self.project = l.project.clone();
        self.sidebar = l.sidebar;
        self.parent_of = l
            .parents
            .iter()
            .filter(|(k, v)| self.tasks.contains_key(*k) && self.tasks.contains_key(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.zoom = l.zoom && cards.len() > 1;
        self.active = l.active.min(cards.len() - 1);
        for (id, il) in &l.items {
            if !self.item_exists(id) {
                continue;
            }
            let builtin = !matches!(
                crate::editors::resolve(&self.settings.editor),
                crate::editors::Kind::Terminal(_)
            );
            let tab_width = self.cfg.tui.tab_width;
            let it = self.item_mut(id);
            it.expanded = il.expanded;
            it.opened = il.opened.iter().filter(|p| p.is_file()).cloned().collect();
            if builtin {
                it.files = it
                    .opened
                    .iter()
                    .filter_map(|p| Editor::open(p, tab_width).ok())
                    .collect();
            }
        }
        self.cards = cards;
        self.saved_layout = Some(l);
        self.on_active_changed();
        self.focus_pane = true;
        true
    }

    fn layout(&self) -> crate::layout::Layout {
        crate::layout::Layout {
            project: self.project.clone(),
            cards: self.cards.clone(),
            active: self.active,
            zoom: self.zoom,
            sidebar: self.sidebar,
            parents: self
                .parent_of
                .iter()
                .filter(|(k, v)| self.tasks.contains_key(*k) && self.tasks.contains_key(*v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            items: self
                .tabs
                .iter()
                .filter(|t| self.item_exists(&t.id))
                .map(|t| {
                    (
                        t.id.clone(),
                        crate::layout::ItemLayout {
                            expanded: t.expanded,
                            opened: t.opened.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Written when something changed, from the 4-a-second tick.
    fn save_layout(&mut self) {
        if !self.started || self.cards.is_empty() {
            return;
        }
        let l = self.layout();
        if self.saved_layout.as_ref() != Some(&l) {
            if let Err(e) = l.save() {
                tracing_free_log(&format!("couldn't save layout: {e}"));
            }
            self.saved_layout = Some(l);
        }
    }

    fn initial_tabs(&mut self) {
        if self.restore_layout() {
            return;
        }
        self.project = Some(
            self.workspace_repo
                .clone()
                .unwrap_or_else(|| self.workspace.display().to_string()),
        );
        let here = self.project.clone().unwrap_or_default();
        let ids: Vec<String> = self
            .agent_ids()
            .into_iter()
            .filter(|id| !matches!(self.tasks[id].status, TaskStatus::Exited { .. }))
            .collect();
        let mine: Vec<&String> = ids
            .iter()
            .filter(|id| self.project_of(&self.tasks[*id]) == here)
            .collect();
        let pick = mine
            .iter()
            .find(|id| self.tasks[**id].attention)
            .or(mine.first())
            .map(|s| s.to_string());
        match pick {
            Some(id) => self.open_item(&id),
            None => self.new_shell(),
        }
    }

    fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.save_layout();
        self.drain_notifications();
        if self.frame.is_multiple_of(20) {
            self.daemon_outdated = daemon_outdated();
        }
        // In the background nobody is looking at the counts; a slower poll
        // still keeps notifications and the dock badge honest.
        let every = if self.ctx.input(|i| i.focused) {
            self.git_every
        } else {
            self.git_every.max(Duration::from_secs(10))
        };
        if self.last_git.elapsed() > every {
            self.request_git();
        }
        let theme = self.cfg.tui.theme.clone();
        let mut reloaded = vec![];
        for e in self.tabs.iter_mut().flat_map(|t| t.files.iter_mut()) {
            if e.check_disk() {
                reloaded.push(e.name());
            }
            e.rehighlight_if_due(&theme);
        }
        if !reloaded.is_empty() {
            self.notify(
                format!("reloaded {} (changed on disk)", reloaded.join(", ")),
                false,
            );
        }
        if let Some((_, at, err)) = &self.notice {
            if at.elapsed() > Duration::from_secs(if *err { 15 } else { 6 }) {
                self.notice = None;
            }
        }
    }

    fn on_git(&mut self, g: GitData) {
        self.git_inflight = false;
        if g.dir != self.ctx_dir() {
            self.request_git();
            return;
        }
        let changed = matches!(&g.changes, Ok(c) if *c != self.changes);
        match g.changes {
            Ok(c) => {
                self.changes = c;
                self.git_error = None;
            }
            Err(e) => self.git_error = Some(e),
        }
        self.branch = g.branch;
        self.ahead = g.ahead;
        self.ctx_root = g.root;
        self.marks = g.marks;
        if let Some(l) = g.listing {
            self.tree.apply(&g.dir, l);
        }
        // A repo that takes 400ms to scan gets polled every 4s instead of
        // keeping a core busy; cd-ing or focusing another task still
        // refreshes at once via refresh_git_now.
        self.git_every = (g.took * 10).clamp(Duration::from_millis(1500), Duration::from_secs(15));
        if let Some((b, c)) = g.log {
            self.branches = b;
            self.commits = c;
        }
        if changed {
            self.reload_main();
        }
    }

    /// The window is focused and this task's terminal is on screen.
    fn watching(&self, id: &str) -> bool {
        self.ctx.input(|i| i.focused) && self.terminal_cards().iter().any(|t| t == id)
    }

    fn on_push(&mut self, p: Push) {
        match p {
            Push::Screen {
                id,
                rows,
                cols,
                data,
            } => {
                // A full repaint: start a fresh screen, but keep the view (its
                // focus, find bar) if this terminal is already showing.
                let parser = crate::termview::new_parser(rows, cols, 10_000);
                crate::termview::feed(&parser, &data);
                match self.terms.get_mut(&id) {
                    Some(t) => t.view.parser = parser,
                    None => {
                        self.terms
                            .insert(id.clone(), TermState::new(&id, rows, cols));
                        self.terms.get_mut(&id).unwrap().view.parser = parser;
                    }
                }
            }
            Push::Output { id, data } => {
                if let Some(t) = self.terms.get_mut(&id) {
                    // Answers to cursor-position and device queries; the daemon
                    // keeps no emulator that could give them.
                    let replies = crate::termview::feed(&t.view.parser, &data);
                    if !replies.is_empty() {
                        if let Some(c) = &self.client {
                            c.send(Request::Input { id, data: replies });
                        }
                    }
                }
            }
            Push::TaskUpdated(t) => {
                let was = self.tasks.get(&t.id).map(|o| o.attention).unwrap_or(false);
                let moved = self.ctx_task.as_deref() == Some(t.id.as_str())
                    && self.tasks.get(&t.id).map(task_dir) != Some(task_dir(&t));
                // The daemon doesn't flag a terminal's agent as needing you when
                // it goes quiet, so a finished turn you didn't see is tracked here.
                let mut t = t;
                let prev = self.tasks.get(&t.id).map(|o| o.status.clone());
                if t.agent == "shell"
                    && is_agent(&t)
                    && matches!(prev, Some(TaskStatus::Running))
                    && t.status == TaskStatus::Idle
                    && !self.watching(&t.id)
                {
                    self.done_unseen.insert(t.id.clone());
                    let name = self.label(&t);
                    self.notify_away(&name, "finished its turn");
                }
                if !is_agent(&t) {
                    self.done_unseen.remove(&t.id);
                }
                t.attention |= self.done_unseen.contains(&t.id);
                if t.attention && !was && is_agent(&t) && !self.watching(&t.id) {
                    let why = match &t.status {
                        TaskStatus::Waiting { reason } => reason.clone(),
                        TaskStatus::Idle => "finished its turn".into(),
                        other => other.label(),
                    };
                    let name = self.label(&t);
                    self.notify(format!("{name} needs you: {why}"), false);
                    self.notify_away(&name, &why);
                    self.ctx
                        .send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Informational,
                        ));
                }
                // A plain shell that exited closes its tab, like any terminal app.
                if t.agent == "shell" && matches!(t.status, TaskStatus::Exited { .. }) {
                    let id = t.id.clone();
                    self.tasks.insert(t.id.clone(), t);
                    if let Some(i) = self.tabs.iter().position(|x| x.id == id) {
                        self.close_tab(i, true);
                    }
                    return;
                }
                self.tasks.insert(t.id.clone(), t);
                if moved {
                    self.refresh_git_now();
                }
            }
            Push::TaskRemoved { id } => self.forget_task(&id),
            Push::ApprovalAdded(a) => {
                if !self.ctx.input(|i| i.focused) {
                    self.ctx
                        .send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Critical,
                        ));
                }
                self.approvals.insert(a.id, a);
            }
            Push::ApprovalResolved { id, .. } => {
                self.approvals.remove(&id);
            }
            Push::SnapshotTaken { task, snapshot } => {
                for t in self.tabs.iter_mut().filter(|t| t.id == task) {
                    if !t.snaps.iter().any(|s| s.id == snapshot.id) {
                        t.snaps.insert(0, snapshot.clone());
                    }
                }
            }
            Push::Ops(_) => {}
            Push::Message { from, to, text } => {
                let short: String = text.chars().take(80).collect();
                self.notify(
                    format!("{} → {to}: {short}", from.as_deref().unwrap_or("you")),
                    false,
                );
            }
        }
    }

    /// Drop a task and every tab showing it.
    fn forget_task(&mut self, id: &str) {
        self.tasks.remove(id);
        self.parent_of.retain(|k, v| k != id && v != id);
        // A later task can reuse the id; it shouldn't inherit the name.
        self.settings.labels.remove(id);
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs.remove(i);
        }
        self.terms.remove(id);
        self.cards.retain(|t| t.item != id);
        self.active = self.active.min(self.cards.len().saturating_sub(1));
        if self.cards.is_empty() {
            // Fall back to the most urgent task in this project.
            let here = self.project.clone().unwrap_or_default();
            let next = self
                .agent_ids()
                .into_iter()
                .find(|i| self.project_of(&self.tasks[i]) == here);
            if let Some(n) = next {
                self.open_item(&n);
                return;
            }
        }
        self.on_active_changed();
    }

    /// True when the daemon predates live working directories, so the sidebar
    /// can't follow `cd`. Running shells always get one from a current daemon.
    pub fn daemon_stale(&self) -> bool {
        if self.client.is_some() && self.daemon_outdated {
            return true;
        }
        let mut shells = self
            .tasks
            .values()
            .filter(|t| t.agent == "shell" && !matches!(t.status, TaskStatus::Exited { .. }))
            .filter(|t| !crate::editors::is_editor_task(&t.id))
            .peekable();
        // A current daemon always reports both for a live shell.
        self.client.is_some()
            && shells.peek().is_some()
            && shells.all(|t| t.live_cwd.is_none() || t.fg.is_none())
    }

    fn on_reply(&mut self, r: Reply) {
        match r {
            Reply::Snapshots { task, list } => {
                for t in self.tabs.iter_mut().filter(|t| t.id == task) {
                    t.snaps = list.clone();
                }
            }
            Reply::Main {
                item,
                gen,
                title,
                result,
            } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.id == item) else {
                    return;
                };
                if t.git.gen != gen {
                    return;
                }
                let d = &mut t.git.diff;
                d.loading = false;
                d.title = title;
                match result {
                    Ok(p) => {
                        d.parsed = p;
                        d.error = None;
                    }
                    Err(e) => d.error = Some(e),
                }
                d.cursor = d.cursor.min(d.parsed.lines.len().saturating_sub(1));
            }
            Reply::Created(t, shell) => {
                let id = t.id.clone();
                self.tasks.insert(id.clone(), t);
                if crate::editors::is_editor_task(&id) {
                    return;
                }
                if let Some(p) = self.pending_parent.take().filter(|_| shell) {
                    self.parent_of.insert(id.clone(), p);
                }
                if self.open_in_new_card && shell {
                    self.open_in_new_card = false;
                    self.open_beside(&id);
                } else {
                    self.open_item(&id);
                }
                // A new terminal asks for its name in its tab. Enter on an
                // empty box keeps the one it got (term-3).
                if shell {
                    self.renaming = Some((id.clone(), String::new()));
                    self.rename_in_tab = true;
                }
                if !shell {
                    let branch = self.tasks[&id].branch.clone();
                    self.notify(
                        match branch {
                            Some(b) => format!("started {id} on {b}"),
                            None => format!("started {id}"),
                        },
                        false,
                    );
                }
            }
            Reply::Notice(m, e) => self.notify(m, e),
        }
    }

    // ── actions ─────────────────────────────────────────────────────────

    pub fn pane_grid(&self) -> (u16, u16) {
        self.terms
            .values()
            .next()
            .map(|t| (t.view.rows, t.view.cols))
            .unwrap_or((40, 120))
    }

    /// The project's folder, where new agents and terminals start.
    fn project_dir(&self) -> PathBuf {
        self.project
            .clone()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.ctx_dir())
    }

    /// Ask for a new agent's name (and task), suggested from `task`.
    pub fn open_new_agent(&mut self, task: &str) {
        self.bar = None;
        self.new_form = Some(NewForm {
            kind: NewKind::Agent,
            name: suggest_name(task),
            name_edited: false,
            task: task.to_string(),
            agent: self.settings.default_agent.clone(),
            worktree: self.settings.worktree,
            dir: self.project_dir().display().to_string(),
            focus_set: false,
        });
    }

    pub fn open_new_project(&mut self) {
        self.bar = None;
        self.new_form = Some(NewForm {
            kind: NewKind::Project,
            name: String::new(),
            name_edited: false,
            task: String::new(),
            agent: self.settings.default_agent.clone(),
            worktree: self.settings.worktree,
            dir: String::new(),
            focus_set: false,
        });
    }

    /// Why the form can't be submitted yet, if it can't.
    pub fn form_problem(&self) -> Option<String> {
        let f = self.new_form.as_ref()?;
        match f.kind {
            NewKind::Agent => {
                let id = slugify(&f.name);
                if id.is_empty() {
                    Some("Give it a name".into())
                } else if self.tasks.contains_key(&id) {
                    Some(format!("{id} is already taken"))
                } else if !muster_core::paths::expand(f.dir.trim()).is_dir() {
                    Some("That folder doesn't exist".into())
                } else {
                    None
                }
            }
            NewKind::Project => {
                if f.dir.trim().is_empty() {
                    Some("Pick a folder".into())
                } else if !muster_core::paths::expand(f.dir.trim()).is_dir() {
                    Some("That folder doesn't exist".into())
                } else if f.name.trim().is_empty() {
                    Some("Give it a name".into())
                } else {
                    None
                }
            }
        }
    }

    pub fn submit_form(&mut self) {
        if self.form_problem().is_some() {
            return;
        }
        let Some(f) = self.new_form.take() else {
            return;
        };
        let dir = muster_core::paths::expand(f.dir.trim());
        let dir = dir.canonicalize().unwrap_or(dir);
        match f.kind {
            NewKind::Agent => {
                let task = Some(f.task.trim().to_string()).filter(|t| !t.is_empty());
                self.create_task(f.agent, slugify(&f.name), task, f.worktree, dir);
            }
            NewKind::Project => {
                let root = git::root(&dir).unwrap_or(dir.clone());
                let key = root.display().to_string();
                let folder = root
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let name = f.name.trim().to_string();
                if name != folder {
                    self.settings.project_names.insert(key.clone(), name);
                } else {
                    self.settings.project_names.remove(&key);
                }
                self.project = Some(key);
                self.cards.clear();
                self.active = 0;
                let mut i = self.tasks.values().filter(|t| t.agent == "shell").count() + 1;
                let mut term = format!("term-{i}");
                while self.tasks.contains_key(&term) {
                    i += 1;
                    term = format!("term-{i}");
                }
                self.create_task("shell".into(), term, None, false, root);
            }
        }
        self.focus_pane = true;
    }

    pub fn create_task(
        &mut self,
        agent: String,
        name: String,
        prompt: Option<String>,
        worktree: bool,
        dir: PathBuf,
    ) {
        let repo = git::root(&dir).map(|p| p.display().to_string());
        let (rows, cols) = self.pane_grid();
        let shell = agent == "shell";
        let req = NewTask {
            name,
            agent,
            no_worktree: !worktree || repo.is_none(),
            cwd: Some(dir.display().to_string()),
            repo,
            base: None,
            args: vec![],
            prompt,
            rows,
            cols,
        };
        self.call(Request::NewTask(req), move |r| match r {
            Ok(Response::Task(t)) => Some(Reply::Created(t, shell)),
            Err(e) => Some(Reply::Notice(e, true)),
            _ => None,
        });
    }

    pub fn new_shell(&mut self) {
        let dir = self.ctx_dir();
        self.new_shell_in(dir);
    }

    pub fn new_shell_in(&mut self, dir: PathBuf) {
        let mut i = self.tasks.values().filter(|t| t.agent == "shell").count() + 1;
        let mut name = format!("term-{i}");
        while self.tasks.contains_key(&name) {
            i += 1;
            name = format!("term-{i}");
        }
        self.create_task("shell".into(), name, None, false, dir);
    }

    /// The git view with one file's (or every file's) working-tree diff.
    pub fn open_diff(&mut self, path: Option<String>) {
        let staged_only = path
            .as_ref()
            .and_then(|p| self.changes.iter().find(|c| &c.path == p))
            .map(|c| c.staged() && !c.unstaged())
            .unwrap_or(false);
        let mode = if staged_only {
            DiffMode::Staged
        } else {
            DiffMode::Unstaged
        };
        self.show_git();
        self.show_main(MainSrc::Changes { path, mode });
    }

    /// The Changes card for the focused task.
    pub fn show_git(&mut self) {
        if let Some(t) = self.target().cloned() {
            if t.child != Child::Changes {
                self.show(Target {
                    item: t.item,
                    child: Child::Changes,
                });
            }
        }
    }

    /// Open `rel` (relative to the tab's directory) in the Editor view, using
    /// whichever editor the settings pick.
    pub fn open_file(&mut self, rel: &str, line: Option<usize>) {
        let path = self.ctx_dir().join(rel);
        if path.is_dir() {
            return;
        }
        self.tree.reveal(rel);
        let Some(item) = self.target().map(|t| self.changes_item(&t.item)) else {
            return;
        };
        {
            let it = self.item_mut(&item);
            if !it.opened.contains(&path) {
                it.opened.push(path.clone());
            }
            it.expanded = true;
        }
        self.show(Target {
            item,
            child: Child::File(path.clone()),
        });
        match crate::editors::resolve(&self.settings.editor) {
            crate::editors::Kind::Terminal(cmd) => {
                return self.open_in_terminal_editor(&cmd, &path, line.unwrap_or(1));
            }
            crate::editors::Kind::Gui(cmd) => {
                match crate::editors::open_gui(&cmd, &path, line.unwrap_or(1)) {
                    Ok(()) => self.notify(format!("opened {rel} in {cmd}"), false),
                    Err(e) => self.notify(format!("couldn't start `{cmd}`: {e}"), true),
                }
                // Also shown read-along in the built-in editor, which reloads
                // as the other editor saves.
            }
            crate::editors::Kind::Builtin => {}
        }
        let Some(it) = self.tabs.get_mut(self.cur) else {
            return;
        };
        let i = match it.files.iter().position(|e| e.path == path) {
            Some(i) => i,
            None => match Editor::open(&path, self.cfg.tui.tab_width) {
                Ok(e) => {
                    it.files.push(e);
                    it.files.len() - 1
                }
                Err(e) => return self.notify(format!("open {rel}: {e:#}"), true),
            },
        };
        it.file = i;
        if let Some(l) = line {
            it.files[i].goto(l);
            it.files[i].follow = true;
        }
        self.focus_pane = true;
    }

    /// nvim/helix/… in a PTY inside the Editor view. One per tab, kept as a
    /// daemon task so it survives restarts; a second file goes to the running
    /// editor where it has a way to take one.
    fn open_in_terminal_editor(&mut self, cmd: &str, path: &Path, line: usize) {
        let Some(item) = self.tabs.get(self.cur).map(|t| t.id.clone()) else {
            return;
        };
        let id = crate::editors::task_id(&item);
        let running = self
            .tasks
            .get(&id)
            .is_some_and(|t| !matches!(t.status, TaskStatus::Exited { .. }));
        if running {
            match crate::editors::reopen_keys(cmd, path, line) {
                Some(keys) => self.send_input(&id, keys),
                None => self.notify(
                    format!("{cmd} is already open in this tab; quit it to open another file"),
                    true,
                ),
            }
            return;
        }
        if self.tasks.contains_key(&id) {
            // Exited editor still registered; clear it so the name is free.
            if let Some(c) = &self.client {
                c.send(Request::Remove {
                    id: id.clone(),
                    prune_worktree: false,
                });
            }
            self.tasks.remove(&id);
            self.terms.remove(&id);
        }
        let dir = self.ctx_dir();
        let (rows, cols) = self.pane_grid();
        let req = NewTask {
            name: id,
            agent: "shell".into(),
            no_worktree: true,
            cwd: Some(dir.display().to_string()),
            repo: None,
            base: None,
            args: vec!["-c".into(), crate::editors::terminal_cmd(cmd, path, line)],
            prompt: None,
            rows,
            cols,
        };
        self.call(Request::NewTask(req), move |r| match r {
            Ok(Response::Task(t)) => Some(Reply::Created(t, true)),
            Err(e) => Some(Reply::Notice(e, true)),
            _ => None,
        });
    }

    /// Hand a file to an external editor (doesn't block; muster reloads on save).
    pub fn open_external(&mut self, path: &Path, line: usize) {
        let file = path.display().to_string();
        let r = if let Ok(v) = std::env::var("VISUAL").or_else(|_| std::env::var("MUSTER_EDITOR")) {
            let mut parts = v.split_whitespace();
            let prog = parts.next().unwrap_or("code").to_string();
            let mut args: Vec<String> = parts.map(String::from).collect();
            let base = Path::new(&prog)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            match base.as_str() {
                "code" | "cursor" => {
                    args.push("-g".into());
                    args.push(format!("{file}:{line}"));
                }
                "zed" | "subl" => args.push(format!("{file}:{line}")),
                _ => args.push(file.clone()),
            }
            std::process::Command::new(prog)
                .args(args)
                .spawn()
                .map(|_| ())
        } else if cfg!(target_os = "macos") {
            std::process::Command::new("open")
                .arg(&file)
                .spawn()
                .map(|_| ())
        } else {
            std::process::Command::new("xdg-open")
                .arg(&file)
                .spawn()
                .map(|_| ())
        };
        if let Err(e) = r {
            self.notify(
                format!("couldn't open external editor: {e} (set $VISUAL)"),
                true,
            );
        }
    }

    pub fn merge_prompt(&mut self) {
        let Some(t) = self
            .ctx_task
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .cloned()
        else {
            return self.notify("select an agent with its own branch", true);
        };
        let (Some(branch), Some(root)) = (t.branch.clone(), t.repo.clone()) else {
            return self.notify("this task runs in place (no branch to merge)", true);
        };
        let root = PathBuf::from(root);
        let target = git::current_branch(&root).unwrap_or_else(|| "HEAD".into());
        let mut text = format!("Merge {branch} into {target} in {}?", root.display());
        if !self.changes.is_empty() {
            text.push_str(&format!("\n\n{} uncommitted change(s) in the agent's worktree are not included. Commit them first.", self.changes.len()));
        }
        self.confirm = Some(Confirm::Merge { root, branch, text });
    }

    pub fn git_op(&mut self, f: impl FnOnce(&Path) -> anyhow::Result<String>) {
        let dir = self.ctx_dir();
        match f(&dir) {
            Ok(msg) if !msg.is_empty() => self.notify(msg, false),
            Ok(_) => {}
            Err(e) => self.notify(format!("{e:#}"), true),
        }
        self.refresh_git_now();
        self.reload_main();
    }

    pub fn decide(&mut self, id: u64, allow: bool) {
        if let Some(c) = &self.client {
            c.send(Request::Decide { id, allow });
        }
    }

    pub fn decide_first(&mut self, allow: bool) {
        let ctx = self.ctx_task.clone();
        let pick = self
            .approvals
            .values()
            .find(|a| matches!(&a.source, ApprovalSource::Task { task_id, .. } if Some(task_id) == ctx.as_ref()))
            .or_else(|| self.approvals.values().next())
            .map(|a| a.id);
        match pick {
            Some(id) => self.decide(id, allow),
            None => self.notify("no pending approvals", false),
        }
    }

    pub fn next_attention(&mut self) {
        let ids = self.agent_ids();
        let cur = self
            .tabs
            .get(self.cur)
            .and_then(|t| ids.iter().position(|x| *x == t.id));
        let start = cur.map(|c| c + 1).unwrap_or(0);
        for i in 0..ids.len() {
            let id = &ids[(start + i) % ids.len()];
            if self.tasks[id].attention {
                let id = id.clone();
                self.open_item(&id);
                return;
            }
        }
        self.notify("nothing needs you", false);
    }

    pub fn kill(&mut self, id: &str) {
        if let Some(c) = &self.client {
            c.send(Request::Kill { id: id.to_string() });
        }
    }

    pub fn checkpoint(&mut self, task: &str) {
        self.call(
            Request::TakeSnapshot {
                id: task.to_string(),
                label: "manual checkpoint".into(),
            },
            |r| match r {
                Ok(Response::Snapshot(Some(s))) => {
                    Some(Reply::Notice(format!("checkpoint #{}", s.seq), false))
                }
                Ok(_) => Some(Reply::Notice(
                    "nothing changed since the last snapshot".into(),
                    false,
                )),
                Err(e) => Some(Reply::Notice(e, true)),
            },
        );
    }

    pub fn run_confirm(&mut self, c: Confirm) {
        match c {
            Confirm::Merge { root, branch, .. } => match git::merge(&root, &branch) {
                Ok(m) => self.notify(m, false),
                Err(e) => self.notify(format!("{e:#}"), true),
            },
            Confirm::Revert { task, snapshot, .. } => {
                let Some(client) = self.client.clone() else {
                    return;
                };
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                self.rt.spawn(async move {
                    let reply = match client
                        .call(Request::Revert {
                            id: task.clone(),
                            snapshot,
                        })
                        .await
                    {
                        Ok(Response::Snapshot(Some(s))) => Reply::Notice(
                            format!("{} (#{}; revert that to undo)", s.label, s.seq),
                            false,
                        ),
                        Ok(_) => Reply::Notice("reverted".into(), false),
                        Err(e) => Reply::Notice(format!("{e:#}"), true),
                    };
                    let _ = tx.send(Ev::Reply(reply));
                    if let Ok(Response::Snapshots(list)) =
                        client.call(Request::Snapshots { id: task.clone() }).await
                    {
                        let _ = tx.send(Ev::Reply(Reply::Snapshots { task, list }));
                    }
                    ctx.request_repaint();
                });
                self.refresh_git_now();
            }
            Confirm::Remove { id, prune, .. } => {
                self.call(
                    Request::Remove {
                        id: id.clone(),
                        prune_worktree: prune,
                    },
                    move |r| match r {
                        Ok(_) => Some(Reply::Notice(format!("removed {id}"), false)),
                        Err(e) => Some(Reply::Notice(e, true)),
                    },
                );
            }
            Confirm::Discard { dir, change, .. } => match git::discard_file(&dir, &change) {
                Ok(_) => self.git_op(|_| Ok(format!("discarded {}", change.path))),
                Err(e) => self.notify(format!("{e:#}"), true),
            },
            Confirm::CloseDirty { tab, .. } => self.close_tab(tab, true),
            Confirm::RestartDaemon { .. } => {
                // The connection loop notices the daemon is gone and starts
                // the one built alongside this app.
                self.call(Request::Shutdown, |_| None);
                self.notify("restarting the daemon…", false);
            }
        }
    }
}

impl App {
    /// Fonts, backdrop and blur from the settings, then persist any change.
    fn apply_settings(&mut self, ui: &egui::Ui, frame: &eframe::Frame) {
        let s = &self.settings;
        let want = (s.font.clone(), s.font_weight, s.bold_weight);
        if self.applied_font.as_ref() != Some(&want) {
            self.font_error = self
                .fonts
                .apply(ui.ctx(), want.0.as_ref(), want.1, want.2)
                .err();
            self.applied_font = Some(want);
        }
        let sch = theme::scheme(&s.scheme);
        theme::set_backdrop(self.transparent, sch.bg, s.opacity.clamp(0.3, 1.0));
        if self.transparent {
            let r = s.blur.clamp(0.0, crate::settings::BLUR_MAX).round() as i64;
            if self.applied_blur != Some(r) {
                set_window_blur(frame, r);
                self.applied_blur = Some(r);
            }
        }
        if self.settings != self.saved_settings {
            match self.settings.save() {
                Ok(()) => self.saved_settings = self.settings.clone(),
                Err(e) => {
                    // Don't retry every frame; the next change tries again.
                    self.saved_settings = self.settings.clone();
                    self.notify(format!("couldn't save settings: {e}"), true);
                }
            }
        }
    }
}

/// A string literal for AppleScript: backslashes and quotes escaped.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A Notification Center / desktop notification. muster can run as a bare
/// binary without an app bundle, which the native APIs need, so macOS goes
/// through osascript and Linux through notify-send (as keel does).
fn notify_system(title: &str, body: &str) {
    let mut c = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("osascript");
        c.arg("-e").arg(format!(
            "display notification {} with title {}",
            applescript_string(body),
            applescript_string(&format!("muster · {title}"))
        ));
        c
    } else {
        let mut c = std::process::Command::new("notify-send");
        c.args(["--app-name=muster", title, body]);
        c
    };
    let _ = c
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// The GUI has no log of its own; a failed layout save isn't worth a toast.
fn tracing_free_log(msg: &str) {
    eprintln!("muster: {msg}");
}

/// The daemon makes its socket when it starts, so a `muster` binary newer than
/// the socket means the daemon is running code from before the last build.
fn daemon_outdated() -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (
        mtime(&net::muster_binary()),
        mtime(&muster_core::paths::socket()),
    ) {
        (Some(bin), Some(sock)) => bin > sock + Duration::from_secs(2),
        _ => false,
    }
}

pub use muster_core::agents::{agent_kind, is_agent};

impl App {
    /// When to draw again if nothing happens. egui redraws the whole window
    /// each time, so an idle window shouldn't wake more than it needs to:
    /// output, typing and the mouse all wake it anyway. What's left is
    /// the spinner of a working agent, a toast running out, and the polls in
    /// `tick` (git status, files changed on disk, layout).
    fn next_wake(&self) -> Duration {
        let working = self
            .tasks
            .values()
            .any(|t| t.status == TaskStatus::Running && is_agent(t));
        if working {
            Duration::from_millis(500)
        } else if self.notice.is_some() || self.bar.is_some() {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(2)
        }
    }
}

/// Where a task is right now: a shell follows its `cd`s, an agent stays put.
pub fn task_dir(t: &TaskInfo) -> PathBuf {
    t.live_cwd
        .as_deref()
        .filter(|_| t.agent == "shell")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from(&t.cwd))
}

/// Blurs whatever shows through the window, like Terminal.app, iTerm2 and
/// Ghostty. Private CoreGraphics call (winit's own `set_blur` uses it too);
/// egui has no option for it.
#[cfg(target_os = "macos")]
fn set_window_blur(frame: &eframe::Frame, radius: i64) -> bool {
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGSMainConnectionID() -> *mut std::ffi::c_void;
        fn CGSSetWindowBackgroundBlurRadius(
            connection: *mut std::ffi::c_void,
            window: isize,
            radius: i64,
        ) -> i32;
    }
    let Ok(handle) = frame.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let window: *mut AnyObject = objc2::msg_send![view, window];
        if window.is_null() {
            return false;
        }
        let number: isize = objc2::msg_send![window, windowNumber];
        CGSSetWindowBackgroundBlurRadius(CGSMainConnectionID(), number, radius) == 0
    }
}

#[cfg(not(target_os = "macos"))]
fn set_window_blur(_frame: &eframe::Frame, _radius: i64) -> bool {
    true
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.pump();
        self.apply_settings(ui, frame);
        if let Some(t) = theme::backdrop() {
            ui.painter().rect_filled(ui.max_rect(), 0.0, t);
        }
        crate::views::draw(ui, self);
        #[cfg(debug_assertions)]
        if let Some(mut d) = self.devshot.take() {
            d.tick(&ui.ctx().clone(), self);
            self.devshot = Some(d);
        }
        ui.ctx().request_repaint_after(self.next_wake());
    }

    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(text) = self.devshot.as_mut().and_then(|d| d.typed.take()) {
            raw.events.push(egui::Event::Text(text));
            raw.events.push(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            });
        }
        if let Some((key, shift)) = self.devshot.as_mut().and_then(|d| d.key.take()) {
            let mut m = egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND;
            m.shift = shift;
            raw.events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: m,
            });
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.transparent {
            return [0.0; 4];
        }
        egui::Rgba::from(theme::scheme(&self.settings.scheme).bg).to_array()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(path: &str) -> Change {
        Change {
            path: path.into(),
            x: ' ',
            y: 'M',
        }
    }

    #[test]
    fn marks_cover_every_parent_folder() {
        let m = Marks::new(&[ch("a/b/c.rs"), ch("a/x/y.rs"), ch("top.rs"), ch("new/")]);
        for d in ["a", "a/b", "a/x"] {
            assert!(m.dirs.contains(d), "{d} should be marked");
        }
        assert!(!m.dirs.contains("a/b/c.rs"));
        assert_eq!(m.files.get("top.rs"), Some(&'M'));
        // An untracked folder from `git status` comes with a trailing slash.
        assert!(m.files.contains_key("new"));
    }
}
