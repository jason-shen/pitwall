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

use crate::i18n::{tr, trf};
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
    Brief,
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
    /// A brief's template, an index into `briefs::TEMPLATES`.
    pub template: usize,
    /// The brief this agent is handed, linked to it once it starts.
    pub brief: Option<PathBuf>,
}

/// A review comment on one line of an agent's diff, waiting to be sent.
#[derive(Clone)]
pub struct Remark {
    pub file: String,
    pub line: Option<usize>,
    /// The diff line it's about, as shown.
    pub quote: String,
    pub text: String,
}

/// A project's briefs as last read, and when.
type BriefList = (Instant, Vec<crate::briefs::Entry>);

/// How a Markdown file's card shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MdMode {
    Edit,
    /// The editor with the rendered page beside it.
    Preview,
}

/// Keeps the editor and the preview beside it scrolled to the same place:
/// where each one was last frame, and where the preview's blocks landed.
#[derive(Default)]
pub struct MdSync {
    pub editor_top: usize,
    pub preview_y: f32,
    pub anchors: crate::markdown::Anchors,
    /// The preview's scroll to apply next frame, when the editor moved.
    pub scroll_to: Option<f32>,
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
    /// Output held back inside a synchronized update (mode 2026), drawn in
    /// one go when the program ends it.
    held: Vec<u8>,
    held_since: Option<Instant>,
    /// A synchronized update is open (its begin seen, its end not yet).
    sync_open: bool,
}

/// Programs that mark their redraws (Claude Code, Codex) say where one starts
/// and ends; drawing halfway through tears. A program that never ends one
/// is drawn anyway after this long.
const SYNC_LIMIT: Duration = Duration::from_millis(120);

impl TermState {
    /// Take output in, holding it while a synchronized update is open.
    fn take(&mut self, data: &[u8]) {
        // Only the new bytes are looked at, starting a marker's length back
        // so one split across two reads is still seen.
        const MARK: usize = 8;
        let from = self.held.len().saturating_sub(MARK - 1);
        self.held.extend_from_slice(data);
        let mut k = from;
        while k + MARK <= self.held.len() {
            match &self.held[k..k + MARK] {
                b"\x1b[?2026h" => self.sync_open = true,
                b"\x1b[?2026l" => self.sync_open = false,
                _ => {
                    k += 1;
                    continue;
                }
            }
            k += MARK;
        }
        let since = *self.held_since.get_or_insert_with(Instant::now);
        if !self.sync_open || since.elapsed() > SYNC_LIMIT {
            self.flush();
        }
    }

    fn flush(&mut self) {
        // Drawn now; a later end marker on its own closes nothing.
        self.sync_open = false;
        if !self.held.is_empty() {
            let _ = crate::termview::feed(&self.view.parser, &std::mem::take(&mut self.held));
        }
        self.held_since = None;
    }
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
            held: vec![],
            held_since: None,
            sync_open: false,
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
    Brief(PathBuf, crate::briefs::Update),
    /// A git command run off the UI thread finished.
    GitDone(Result<String, String>),
    Accepted {
        id: String,
        result: Result<String, String>,
    },
    RemarksSent(String, Result<usize, String>),
    Diffstats(Vec<(String, (usize, usize, usize))>),
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
    /// Commit what the agent left in its worktree, merge its branch into the
    /// one it forked from, then offer to remove it.
    Accept {
        id: String,
        root: PathBuf,
        worktree: PathBuf,
        branch: String,
        target: String,
        message: String,
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
    DiscardHunk {
        dir: PathBuf,
        hunk: git::Hunk,
        text: String,
    },
    /// Close a project's terminals and take it off the sidebar.
    RemoveProject {
        root: String,
        text: String,
    },
    /// Move a file to the Trash.
    TrashFile {
        path: PathBuf,
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
            Confirm::Accept { text, .. }
            | Confirm::Revert { text, .. }
            | Confirm::Remove { text, .. }
            | Confirm::Discard { text, .. }
            | Confirm::DiscardHunk { text, .. }
            | Confirm::RemoveProject { text, .. }
            | Confirm::TrashFile { text, .. }
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
    /// How each Markdown file is shown, and its scroll sync.
    pub md_mode: HashMap<PathBuf, MdMode>,
    pub md_sync: HashMap<PathBuf, MdSync>,
    /// A Markdown file read from disk for the preview, with its mtime, so it
    /// isn't read again every frame.
    pub md_disk: HashMap<PathBuf, (Option<std::time::SystemTime>, String)>,
    /// The headless claude filling in each brief.
    pub brief_runs: HashMap<PathBuf, crate::briefs::Run>,
    briefs_cache: std::cell::RefCell<HashMap<String, BriefList>>,
    repo_cache: std::cell::RefCell<HashMap<String, bool>>,
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
    /// Each agent's files and lines changed since it started, for the
    /// AGENTS list; polled in the background.
    pub diffstats: HashMap<String, (usize, usize, usize)>,
    last_stats: Instant,
    /// A diffstat pass is still running; slow repos mustn't stack them up.
    stats_running: bool,
    /// The terminals on screen, shared with the network thread so output
    /// from others doesn't redraw the window.
    shown: net::Shown,
    /// See-through mode is set aside while muster isn't the focused app.
    resting_opaque: Option<bool>,
    /// Gap between git polls; grows when a repo is slow to scan.
    git_every: Duration,
    pub sidebar: bool,
    pub settings: Settings,
    saved_settings: Settings,
    /// config.toml as last read or written, to notice when it's edited by hand.
    config_seen: Option<std::time::SystemTime>,
    config_checked: Instant,
    /// Mistakes found in config.toml at startup, shown once the window's up.
    config_problems: Vec<String>,
    /// Settings → Shortcuts is waiting for keys for this command.
    pub capturing: Option<&'static str>,
    pub settings_ui: Option<SettingsUi>,
    fonts: FontSet,
    applied_font: Option<(Option<TermFont>, u16, u16, crate::i18n::Lang)>,
    pub font_error: Option<String>,
    /// Whether the window was created transparent (only possible at startup).
    pub transparent: bool,
    /// Height of the title bar the content runs under, in points; 0 when
    /// opaque or full screen.
    pub titlebar: f32,
    applied_blur: Option<i64>,
    pub commit_msg: String,
    /// The commit message popup is open.
    pub committing: bool,
    pub confirm: Option<Confirm>,
    /// Review comments per agent, sent back to it together.
    pub remarks: HashMap<String, Vec<Remark>>,
    /// The comment being written: its agent and the line it's on.
    pub remarking: Option<(String, Remark)>,
    git_lock: std::sync::Arc<std::sync::Mutex<()>>,
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
        config_problems: Vec<String>,
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
        let shown: net::Shown = Default::default();
        net::start(&rt, ctx.clone(), tx.clone(), shown.clone());
        let cfg = Config::load().unwrap_or_default();
        // Installed here, not on the first frame: egui only swaps fonts in at
        // the start of a pass, and drawing with the unregistered "terminal"
        // family panics.
        let fonts = FontSet::load();
        let want = (
            settings.font.clone(),
            settings.font_weight,
            settings.bold_weight,
            crate::i18n::current(),
        );
        let font_error = fonts
            .apply(&ctx, want.0.as_ref(), want.1, want.2, want.3)
            .err();
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
            md_mode: HashMap::new(),
            md_sync: HashMap::new(),
            md_disk: HashMap::new(),
            brief_runs: HashMap::new(),
            briefs_cache: Default::default(),
            repo_cache: Default::default(),
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
            diffstats: HashMap::new(),
            last_stats: Instant::now() - Duration::from_secs(60),
            stats_running: false,
            shown,
            resting_opaque: None,
            git_every: Duration::from_millis(1500),
            sidebar: true,
            saved_settings: settings.clone(),
            config_seen: config_mtime(),
            config_checked: Instant::now(),
            config_problems,
            capturing: None,
            settings,
            settings_ui: None,
            fonts,
            applied_font: Some(want),
            font_error,
            transparent,
            titlebar: 0.0,
            applied_blur: None,
            commit_msg: String::new(),
            committing: false,
            confirm: None,
            git_lock: Default::default(),
            remarks: HashMap::new(),
            remarking: None,
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
                    text: trf!("{name} has unsaved changes. Close anyway?", name = e.name()),
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
        let launch = Some(launch).filter(|l| !self.settings.hidden_projects.contains(l));
        let mut v: Vec<String> = self
            .tasks
            .values()
            .filter(|t| !crate::editors::is_editor_task(&t.id))
            .map(|t| t.repo.clone().unwrap_or_else(|| t.cwd.clone()))
            .chain(launch)
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
            let title = trf!(
                "snapshot #{seq}: {label}",
                seq = snap.seq,
                label = snap.label
            );
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
                            Ok(_) => Err(tr("unexpected reply").into()),
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
                    let what = path.clone().unwrap_or_else(|| tr("all files").into());
                    let label = match mode {
                        DiffMode::Base(_) => tr("everything since the agent started").to_string(),
                        m => tr(m.label()).to_string(),
                    };
                    (
                        format!("{what} · {label}"),
                        git::diff(&dir, mode, path.as_deref()),
                    )
                }
                MainSrc::Commit(h) => (
                    trf!("commit {hash}", hash = &h[..h.len().min(10)]),
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
                repaints: true,
            });
        }
    }

    /// An agent's own notifications (OSC 9 / 777 / 99), from terminals that
    /// have been on screen. Ones from the terminal you're watching are shown
    /// already, so they're dropped.
    fn drain_notifications(&mut self) {
        let mut got = vec![];
        let mut copied = vec![];
        for (id, t) in self.terms.iter() {
            let mut p = t.view.parser.lock().unwrap();
            let n = std::mem::take(&mut p.callbacks_mut().notifications);
            if !n.is_empty() {
                got.push((id.clone(), n));
            }
            // OSC 52: nvim's "+y, tmux, anything over ssh.
            if let Some(text) = p.callbacks_mut().clipboard.take() {
                copied.push((id.clone(), text));
            }
        }
        for (id, text) in copied {
            let n = text.chars().count();
            self.ctx.copy_text(text);
            let name = self.tasks.get(&id).map(|t| self.label(t)).unwrap_or(id);
            // Said out loud, so a program can't swap the clipboard unseen.
            self.notify(
                trf!(
                    "{name} copied {n} characters to the clipboard",
                    name = name,
                    n = n
                ),
                false,
            );
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
        // A flood of output (`cat` of a big log, `yes`) is taken a slice at a
        // time so frames keep coming and Ctrl+C gets drawn; the rest waits in
        // the channel for the next frame.
        let budget = Instant::now();
        while let Ok(ev) = self.rx.try_recv() {
            if budget.elapsed() > Duration::from_millis(12) {
                self.on_event(ev);
                self.ctx.request_repaint();
                break;
            }
            self.on_event(ev);
        }
    }

    fn on_event(&mut self, ev: Ev) {
        {
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
                    self.notify(
                        tr("lost connection to the muster daemon — reconnecting…"),
                        true,
                    );
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
            let setting = self.settings.editor.clone();
            let tab_width = self.cfg.tui.tab_width;
            let it = self.item_mut(id);
            it.expanded = il.expanded;
            it.opened = il.opened.iter().filter(|p| p.is_file()).cloned().collect();
            it.files = it
                .opened
                .iter()
                .filter(|p| {
                    !matches!(
                        crate::editors::for_file(&setting, p),
                        crate::editors::Kind::Terminal(_)
                    )
                })
                .filter_map(|p| Editor::open(p, tab_width).ok())
                .collect();
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
        self.watch_config();
        self.save_layout();
        self.drain_notifications();
        for t in self.terms.values_mut() {
            if t.held_since.is_some_and(|s| s.elapsed() > SYNC_LIMIT) {
                t.flush();
            }
        }
        if self.last_stats.elapsed() > Duration::from_secs(8) {
            self.refresh_diffstats();
        }
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
            // A brief being filled in reloads several times a second: one
            // undo step back to what you wrote, no notice each time, and no
            // typing into it meanwhile.
            match self.brief_runs.get_mut(&e.path) {
                Some(run) => {
                    e.locked = true;
                    if e.check_disk_with(!run.recorded) {
                        run.recorded = true;
                    }
                }
                None if e.check_disk() => reloaded.push(e.name()),
                None => {}
            }
            e.rehighlight_if_due(&theme);
        }
        if !reloaded.is_empty() {
            self.notify(
                trf!(
                    "reloaded {files} (changed on disk)",
                    files = reloaded.join(", ")
                ),
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
                    Some(t) => {
                        t.held.clear();
                        t.held_since = None;
                        t.view.replace_parser(parser);
                    }
                    None => {
                        self.terms
                            .insert(id.clone(), TermState::new(&id, rows, cols));
                        self.terms.get_mut(&id).unwrap().view.parser = parser;
                    }
                }
            }
            Push::Output { id, data } => {
                if let Some(t) = self.terms.get_mut(&id) {
                    // The daemon answers the terminal's queries (cursor
                    // position, device attributes) for every task, shown or
                    // not; answering here as well would send each twice.
                    t.take(&data);
                    if t.held_since.is_some() {
                        self.ctx.request_repaint_after(SYNC_LIMIT);
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
                    self.notify_away(&name, tr("finished its turn"));
                }
                if !is_agent(&t) {
                    self.done_unseen.remove(&t.id);
                }
                t.attention |= self.done_unseen.contains(&t.id);
                if t.attention && !was && is_agent(&t) && !self.watching(&t.id) {
                    let why = match &t.status {
                        TaskStatus::Waiting { reason } => reason.clone(),
                        TaskStatus::Idle => tr("finished its turn").into(),
                        other => tr(&other.label()).to_string(),
                    };
                    let name = self.label(&t);
                    self.notify(
                        trf!("{name} needs you: {why}", name = name, why = why),
                        false,
                    );
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
                    format!("{} → {to}: {short}", from.as_deref().unwrap_or(tr("you"))),
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
                if self.open_in_new_card {
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
                            Some(b) => trf!("started {id} on {branch}", id = id, branch = b),
                            None => trf!("started {id}", id = id),
                        },
                        false,
                    );
                }
            }
            Reply::Notice(m, e) => self.notify(m, e),
            Reply::Brief(path, u) => self.on_brief(path, u),
            Reply::GitDone(r) => {
                match r {
                    Ok(m) if !m.is_empty() => self.notify(m, false),
                    Ok(_) => {}
                    Err(e) => self.notify(e, true),
                }
                self.refresh_git_now();
                self.reload_main();
            }
            Reply::Diffstats(v) => {
                self.stats_running = false;
                self.diffstats = v.into_iter().collect();
            }
            Reply::RemarksSent(id, r) => match r {
                Ok(n) => {
                    self.remarks.remove(&id);
                    self.notify(trf!("sent {n} comment(s) to {id}", n = n, id = id), false);
                }
                Err(e) => self.notify(trf!("couldn't send the comments: {e}", e = e), true),
            },
            Reply::Accepted { id, result } => {
                self.refresh_git_now();
                self.reload_main();
                match result {
                    // Don't replace a question that's already open; Remove
                    // is in the agent's menu for later.
                    Ok(m) if self.confirm.is_some() => self.notify(m, false),
                    Ok(m) => {
                        self.notify(m, false);
                        self.confirm = Some(Confirm::Remove {
                            text: trf!(
                                "{id} is merged. Remove it, its worktree and branch?\n\n\
                                 Its snapshots go too.",
                                id = id
                            ),
                            id,
                            prune: true,
                        });
                    }
                    Err(e) => self.notify(e, true),
                }
            }
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
            template: 0,
            brief: None,
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
            template: 0,
            brief: None,
        });
    }

    pub fn open_new_brief(&mut self) {
        self.bar = None;
        self.new_form = Some(NewForm {
            kind: NewKind::Brief,
            name: String::new(),
            name_edited: false,
            task: String::new(),
            agent: self.settings.default_agent.clone(),
            worktree: self.settings.worktree,
            dir: self.project.clone().unwrap_or_default(),
            focus_set: false,
            template: 0,
            brief: None,
        });
    }

    /// Open a brief (or any project file) under its project's own item.
    pub fn open_brief(&mut self, path: &Path) {
        let Some(root) = path.ancestors().nth(3).map(|p| p.display().to_string()) else {
            return;
        };
        if self.project.as_deref() != Some(root.as_str()) {
            self.project = Some(root.clone());
            self.cards.truncate(1);
            self.active = 0;
            self.zoom = false;
        }
        self.md_mode
            .entry(path.to_path_buf())
            .or_insert(MdMode::Preview);
        self.open_path(project_item(&root), path.to_path_buf(), None);
    }

    /// Whether a folder is in a git repo, asked once per folder: the New
    /// agent form wants to know every frame while it's open.
    pub fn is_repo_cached(&self, dir: &str) -> bool {
        let mut c = self.repo_cache.borrow_mut();
        if let Some(v) = c.get(dir) {
            return *v;
        }
        let v = git::root(&muster_core::paths::expand(dir)).is_some();
        c.insert(dir.to_string(), v);
        v
    }

    /// Ask before taking a project off the sidebar, saying what closes.
    pub fn remove_project_prompt(&mut self, root: &str) {
        let name = root.rsplit('/').next().unwrap_or(root).to_string();
        let live: Vec<&TaskInfo> = self
            .tasks
            .values()
            .filter(|t| {
                self.project_of(t) == root && !matches!(t.status, TaskStatus::Exited { .. })
            })
            .collect();
        let agents = live.iter().filter(|t| t.agent != "shell").count();
        let terms = live.len() - agents;
        let text = if live.is_empty() {
            trf!(
                "Remove {name} from muster?\n\nThe folder isn't touched.",
                name = name
            )
        } else {
            let mut what = vec![];
            if terms > 0 {
                what.push(match terms {
                    1 => tr("1 terminal").to_string(),
                    n => trf!("{n} terminals", n = n),
                });
            }
            if agents > 0 {
                what.push(match agents {
                    1 => tr("1 agent").to_string(),
                    n => trf!("{n} agents", n = n),
                });
            }
            let what = match what.as_slice() {
                [a, b] => trf!("{a} and {b}", a = a, b = b),
                _ => what.concat(),
            };
            trf!(
                "Remove {name} from muster?\n\nThe folder isn't touched. Its {what} close; agents' worktrees and branches stay.",
                name = name,
                what = what
            )
        };
        self.confirm = Some(Confirm::RemoveProject {
            root: root.to_string(),
            text,
        });
    }

    fn remove_project(&mut self, root: &str) {
        let item = project_item(root);
        if let Some(e) = self
            .tabs
            .iter()
            .find(|t| t.id == item)
            .and_then(|t| t.files.iter().find(|e| e.dirty))
        {
            return self.notify(trf!("save or revert {name} first", name = e.name()), true);
        }
        let ids: Vec<String> = self
            .tasks
            .values()
            .filter(|t| self.project_of(t) == root)
            .map(|t| t.id.clone())
            .collect();
        if let Some(c) = &self.client {
            for id in &ids {
                c.send(Request::Remove {
                    id: id.clone(),
                    prune_worktree: false,
                });
            }
        }
        self.tabs.retain(|t| t.id != item);
        self.cards.retain(|c| c.item != item);
        self.settings.hidden_projects.insert(root.to_string());
        self.settings.folded_projects.remove(root);
        if self.project.as_deref() == Some(root) {
            self.project = self.project_roots().into_iter().find(|r| r != root);
            self.cards.clear();
            self.active = 0;
        }
        let name = root.rsplit('/').next().unwrap_or(root);
        self.notify(trf!("removed {name} from muster", name = name), false);
    }

    /// Ask before moving a file to the Trash.
    pub fn trash_prompt(&mut self, path: &Path) {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let unsaved = self
            .tabs
            .iter()
            .flat_map(|t| &t.files)
            .any(|e| e.path == path && e.dirty);
        let mut text = trf!("Move {name} to the Trash?", name = name);
        if unsaved {
            text.push_str("\n\n");
            text.push_str(tr("It has unsaved changes, which go too."));
        }
        self.confirm = Some(Confirm::TrashFile {
            path: path.to_path_buf(),
            text,
        });
    }

    fn trash_file(&mut self, path: &Path) {
        if let Some(run) = self.brief_runs.get(path) {
            run.stop();
        }
        if let Err(e) = crate::trash::trash(path) {
            return self.notify(format!("{e}"), true);
        }
        // Wherever it was open, it's closed now.
        for it in self.tabs.iter_mut() {
            it.opened.retain(|p| p != path);
            it.files.retain(|e| e.path != path);
            it.file = 0;
        }
        for c in self.cards.iter_mut() {
            if c.child == Child::File(path.to_path_buf()) {
                c.child = if item_project(&c.item).is_some() {
                    Child::Files
                } else {
                    Child::Terminal
                };
            }
        }
        self.brief_agents_forget(path);
        self.briefs_cache.borrow_mut().clear();
        self.md_disk.remove(path);
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        self.notify(trf!("moved {name} to the Trash", name = name), false);
    }

    /// A brief that's gone: its open run, if any, has nothing to write into.
    fn brief_agents_forget(&mut self, path: &Path) {
        self.brief_runs.remove(path);
        self.md_mode.remove(path);
        self.md_sync.remove(path);
    }

    /// A brief's entry from the cached list: title and progress.
    pub fn brief_entry(&self, path: &Path) -> Option<crate::briefs::Entry> {
        let root = path.ancestors().nth(3)?.display().to_string();
        self.briefs(&root).into_iter().find(|e| e.path == path)
    }

    /// The agent handed this brief, newest first if it went to more than one.
    pub fn brief_agent(&self, path: &Path) -> Option<&TaskInfo> {
        let p = path.display().to_string();
        self.tasks
            .values()
            .filter(|t| t.brief.as_deref() == Some(p.as_str()))
            .max_by_key(|t| t.created_unix)
    }

    /// claude fills in the rest of a brief headless, writing into the file
    /// as its reply streams in.
    pub fn finish_brief(&mut self, path: &Path) {
        if self.brief_runs.contains_key(path) {
            return;
        }
        let Some(root) = path.ancestors().nth(3).map(Path::to_path_buf) else {
            return;
        };
        let Some(agent) = self.cfg.agents.get("claude") else {
            return self.notify(
                tr("Finish with AI runs claude: add [agents.claude] to config.toml"),
                true,
            );
        };
        let (tx, ctx, p) = (self.tx.clone(), self.ctx.clone(), path.to_path_buf());
        let send = move |u| {
            let _ = tx.send(Ev::Reply(Reply::Brief(p.clone(), u)));
            ctx.request_repaint();
        };
        match crate::briefs::finish(agent, &root, path, send) {
            Ok(run) => {
                self.brief_runs.insert(path.to_path_buf(), run);
            }
            Err(e) => self.notify(
                trf!("couldn't start {cmd}: {e}", cmd = agent.command, e = e),
                true,
            ),
        }
    }

    pub fn stop_brief(&mut self, path: &Path) {
        if let Some(r) = self.brief_runs.get(path) {
            r.stop();
        }
    }

    fn on_brief(&mut self, path: PathBuf, u: crate::briefs::Update) {
        use crate::briefs::Update;
        let result = match u {
            Update::Status(s) => {
                if let Some(r) = self.brief_runs.get_mut(&path) {
                    r.status = s;
                }
                return;
            }
            Update::Done(r) => Some(r),
            Update::Stopped => None,
        };
        let run = self.brief_runs.remove(&path);
        let recorded = run.as_ref().is_some_and(|r| r.recorded);
        // Stopped or failed halfway: the file holds a partial brief. Put the
        // one you wrote back; briefs aren't in git to recover it from.
        let mut restored = None;
        if !matches!(result, Some(Ok(()))) {
            if let Some(r) = &run {
                restored = Some(crate::briefs::put(&path, &r.original));
            }
        }
        // Take the last write now, or the next tick reports it as an outside change.
        for e in self.tabs.iter_mut().flat_map(|t| t.files.iter_mut()) {
            if e.path == path {
                e.locked = false;
                if !e.dirty {
                    e.reload_with(!recorded);
                }
            }
        }
        if let Some(Err(e)) = restored {
            return self.notify(
                trf!(
                    "couldn't put the brief back as it was ({e}); ⌘Z in its editor has it",
                    e = e
                ),
                true,
            );
        }
        match result {
            Some(Ok(())) => self.notify(tr("brief filled in"), false),
            Some(Err(e)) => self.notify(
                trf!("couldn't finish the brief: {e}; it's as you left it", e = e),
                true,
            ),
            None => self.notify(tr("stopped; the brief is as you left it"), false),
        }
    }

    /// The New agent form, filled in from a brief: its title as the name,
    /// the brief as the task, the project as the folder.
    pub fn hand_off_brief(&mut self, path: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => return self.notify(trf!("couldn't read the brief: {e}", e = e), true),
        };
        let title = crate::briefs::title_of(path, &text);
        self.open_new_agent(&crate::briefs::handoff_prompt(path, &text));
        if let Some(f) = self.new_form.as_mut() {
            f.name = slugify(&title);
            f.name_edited = true;
            f.brief = Some(path.to_path_buf());
            if let Some(root) = path.ancestors().nth(3) {
                f.dir = root.display().to_string();
            }
        }
    }

    /// A project's briefs, read from disk at most every couple of seconds.
    pub fn briefs(&self, root: &str) -> Vec<crate::briefs::Entry> {
        let mut cache = self.briefs_cache.borrow_mut();
        if let Some((at, v)) = cache.get(root) {
            if at.elapsed() < Duration::from_secs(2) {
                return v.clone();
            }
        }
        let v = crate::briefs::list(Path::new(root));
        cache.insert(root.to_string(), (std::time::Instant::now(), v.clone()));
        v
    }

    /// Why the form can't be submitted yet, if it can't.
    pub fn form_problem(&self) -> Option<String> {
        let f = self.new_form.as_ref()?;
        match f.kind {
            NewKind::Agent => {
                let id = slugify(&f.name);
                if id.is_empty() {
                    Some(tr("Give it a name").into())
                } else if self.tasks.contains_key(&id) {
                    Some(trf!("{id} is already taken", id = id))
                } else if !muster_core::paths::expand(f.dir.trim()).is_dir() {
                    Some(tr("That folder doesn't exist").into())
                } else {
                    None
                }
            }
            NewKind::Brief => {
                if f.name.trim().is_empty() {
                    Some(tr("Give it a title").into())
                } else if f.dir.is_empty() {
                    Some(tr("Open a project first").into())
                } else {
                    None
                }
            }
            NewKind::Project => {
                if f.dir.trim().is_empty() {
                    Some(tr("Pick a folder").into())
                } else if !muster_core::paths::expand(f.dir.trim()).is_dir() {
                    Some(tr("That folder doesn't exist").into())
                } else if f.name.trim().is_empty() {
                    Some(tr("Give it a name").into())
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
            NewKind::Brief => {
                let t =
                    &crate::briefs::TEMPLATES[f.template.min(crate::briefs::TEMPLATES.len() - 1)];
                match crate::briefs::create(&dir, &f.name, t) {
                    Ok(p) => {
                        self.briefs_cache.borrow_mut().remove(&f.dir);
                        self.open_brief(&p);
                        // Ready to type the request.
                        for it in self.tabs.iter_mut() {
                            for e in it.files.iter_mut().filter(|e| e.path == p) {
                                if let Some(i) =
                                    e.lines.iter().position(|l| l.starts_with("**Ask:**"))
                                {
                                    e.cy = i;
                                    e.cx = e.lines[i].chars().count();
                                }
                            }
                        }
                    }
                    Err(e) => self.notify(trf!("couldn't write the brief: {e}", e = e), true),
                }
            }
            NewKind::Agent => {
                let task = Some(f.task.trim().to_string()).filter(|t| !t.is_empty());
                self.create_task_with(f.agent, slugify(&f.name), task, f.worktree, dir, f.brief);
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
                self.settings.hidden_projects.remove(&key);
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
        self.create_task_with(agent, name, prompt, worktree, dir, None)
    }

    pub fn create_task_with(
        &mut self,
        agent: String,
        name: String,
        prompt: Option<String>,
        worktree: bool,
        dir: PathBuf,
        brief: Option<PathBuf>,
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
            brief: brief.map(|b| b.display().to_string()),
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
        self.open_path(item, path, line);
    }

    /// Open `path` under `item` in whichever editor the settings pick.
    pub fn open_path(&mut self, item: String, path: PathBuf, line: Option<usize>) {
        let rel = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
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
        match crate::editors::for_file(&self.settings.editor, &path) {
            crate::editors::Kind::Terminal(cmd) => {
                return self.open_in_terminal_editor(&cmd, &path, line.unwrap_or(1));
            }
            crate::editors::Kind::Gui(cmd) => {
                match crate::editors::open_gui(&cmd, &path, line.unwrap_or(1)) {
                    Ok(()) => {
                        self.notify(trf!("opened {file} in {cmd}", file = rel, cmd = cmd), false)
                    }
                    Err(e) => {
                        self.notify(trf!("couldn't start `{cmd}`: {e}", cmd = cmd, e = e), true)
                    }
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
                    trf!(
                        "{cmd} is already open in this tab; quit it to open another file",
                        cmd = cmd
                    ),
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
            brief: None,
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
                trf!("couldn't open external editor: {e} (set $VISUAL)", e = e),
                true,
            );
        }
    }

    /// Start a comment on the line under the diff cursor.
    pub fn start_remark(&mut self) {
        let Some(it) = self.tabs.get(self.cur) else {
            return;
        };
        if !self.tasks.get(&it.id).is_some_and(|t| t.agent != "shell") {
            return self.notify(tr("comments go to an agent; open an agent's Changes"), true);
        }
        let d = &it.git.diff;
        let Some(l) = d.parsed.lines.get(d.cursor) else {
            return;
        };
        let Some(file) = l.file.clone() else {
            return self.notify(tr("put the cursor on a line of the diff"), true);
        };
        let quote: String = l.text.trim().chars().take(100).collect();
        self.remarking = Some((
            it.id.clone(),
            Remark {
                file,
                line: l.new_line,
                quote,
                text: String::new(),
            },
        ));
    }

    /// What every agent with a worktree has changed since it started, so
    /// finished agents can be told apart without opening each one.
    fn refresh_diffstats(&mut self) {
        if self.stats_running {
            return;
        }
        self.last_stats = Instant::now();
        let jobs: Vec<(String, PathBuf, String)> = self
            .tasks
            .values()
            .filter(|t| t.branch.is_some() && !matches!(t.status, TaskStatus::Exited { .. }))
            .filter_map(|t| Some((t.id.clone(), PathBuf::from(&t.cwd), t.base.clone()?)))
            .collect();
        if jobs.is_empty() {
            self.diffstats.clear();
            return;
        }
        self.stats_running = true;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let v = jobs
                .into_iter()
                .filter_map(|(id, dir, base)| Some((id, git::shortstat(&dir, &base).ok()?)))
                .collect();
            let _ = tx.send(Ev::Reply(Reply::Diffstats(v)));
            ctx.request_repaint();
        });
    }

    /// Every comment for this agent, typed into its prompt as one message.
    pub fn send_remarks(&mut self, id: &str) {
        let Some(list) = self.remarks.get(id).filter(|l| !l.is_empty()) else {
            return;
        };
        let n = list.len();
        let text = review_message(list);
        let id2 = id.to_string();
        self.call(
            Request::Prompt {
                id: id.to_string(),
                text,
                from: None,
            },
            move |r| Some(Reply::RemarksSent(id2, r.map(|_| n))),
        );
    }

    pub fn accept_prompt(&mut self) {
        let Some(t) = self
            .ctx_task
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .cloned()
        else {
            return self.notify(tr("select an agent with its own branch"), true);
        };
        let (Some(branch), Some(root)) = (t.branch.clone(), t.repo.clone()) else {
            return self.notify(
                tr("this agent works in place: there's no branch to merge"),
                true,
            );
        };
        let root = PathBuf::from(root);
        let here = git::current_branch(&root);
        // Older daemons don't say; then it's whatever the main checkout is on.
        let target = t
            .base_branch
            .clone()
            .or_else(|| here.clone())
            .unwrap_or_else(|| "HEAD".into());
        let message = t
            .brief
            .as_ref()
            .map(|b| {
                let b = Path::new(b);
                crate::briefs::title_of(b, &std::fs::read_to_string(b).unwrap_or_default())
            })
            .or_else(|| t.title.clone())
            .unwrap_or_else(|| t.name.clone());
        let mut text = trf!("Accept {id}?", id = t.id);
        let pending = self.changes.len();
        if pending > 0 {
            text.push_str("\n\n");
            text.push_str(&trf!(
                "• Commit its {pending} uncommitted change(s) on {branch} with this message",
                pending = pending,
                branch = branch
            ));
        }
        text.push('\n');
        text.push_str(&trf!(
            "• Merge {branch} into {target} in {dir}",
            branch = branch,
            target = target,
            dir = root.display()
        ));
        if here.as_deref() != Some(target.as_str()) {
            text.push_str("\n\n");
            text.push_str(&trf!(
                "Your checkout is on {here}, not {target}. Check out {target} first.",
                here = here.as_deref().unwrap_or(tr("a detached HEAD")),
                target = target
            ));
        }
        self.confirm = Some(Confirm::Accept {
            id: t.id.clone(),
            root,
            worktree: PathBuf::from(&t.cwd),
            branch,
            target,
            message,
            text,
        });
    }

    /// Run a git command off the UI thread: a commit's hooks or a big
    /// checkout would freeze the window. One at a time, in order, so quick
    /// hunk stages don't fight over the index lock.
    pub fn git_op(&mut self, f: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static) {
        let dir = self.ctx_dir();
        self.in_background(
            move || f(&dir).map_err(|e| format!("{e:#}")),
            Reply::GitDone,
        );
    }

    fn in_background<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        reply: impl FnOnce(T) -> Reply + Send + 'static,
    ) {
        let (tx, ctx, lock) = (self.tx.clone(), self.ctx.clone(), self.git_lock.clone());
        std::thread::spawn(move || {
            let out = {
                let _turn = lock.lock().unwrap_or_else(|e| e.into_inner());
                work()
            };
            let _ = tx.send(Ev::Reply(reply(out)));
            ctx.request_repaint();
        });
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
            .map(|a| a.id);
        // Only the agent on screen: the shortcut mustn't answer for a command
        // you haven't seen.
        match pick {
            Some(id) => self.decide(id, allow),
            None if self.approvals.is_empty() => self.notify(tr("no pending approvals"), false),
            None => self.notify(
                tr("this agent has nothing to approve; ⌘⇧A goes to the one waiting"),
                false,
            ),
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
        self.notify(tr("nothing needs you"), false);
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
                    Some(Reply::Notice(trf!("checkpoint #{seq}", seq = s.seq), false))
                }
                Ok(_) => Some(Reply::Notice(
                    tr("nothing changed since the last snapshot").into(),
                    false,
                )),
                Err(e) => Some(Reply::Notice(e, true)),
            },
        );
    }

    pub fn run_confirm(&mut self, c: Confirm) {
        match c {
            Confirm::Accept {
                id,
                root,
                worktree,
                branch,
                target,
                message,
                ..
            } => {
                self.notify(trf!("accepting {id}…", id = id), false);
                let id2 = id.clone();
                self.in_background(
                    move || {
                        git::accept(&root, &worktree, &branch, &target, &message)
                            .map_err(|e| format!("{e:#}"))
                    },
                    move |result| Reply::Accepted { id: id2, result },
                );
            }
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
                            trf!(
                                "{label} (#{seq}; revert that to undo)",
                                label = s.label,
                                seq = s.seq
                            ),
                            false,
                        ),
                        Ok(_) => Reply::Notice(tr("reverted").into(), false),
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
                        Ok(_) => Some(Reply::Notice(trf!("removed {id}", id = id), false)),
                        Err(e) => Some(Reply::Notice(e, true)),
                    },
                );
            }
            Confirm::RemoveProject { root, .. } => self.remove_project(&root),
            Confirm::TrashFile { path, .. } => self.trash_file(&path),
            Confirm::DiscardHunk { dir, hunk, .. } => self.git_op(move |_| {
                git::discard_hunk(&dir, &hunk)
                    .map(|_| trf!("discarded a hunk of {file}", file = hunk.file))
            }),
            Confirm::Discard { dir, change, .. } => self.git_op(move |_| {
                git::discard_file(&dir, &change)
                    .map(|_| trf!("discarded {file}", file = change.path))
            }),
            Confirm::CloseDirty { tab, .. } => self.close_tab(tab, true),
            Confirm::RestartDaemon { .. } => {
                // The connection loop notices the daemon is gone and starts
                // the one built alongside this app.
                self.call(Request::Shutdown, |_| None);
                self.notify(tr("restarting the daemon…"), false);
            }
        }
    }
}

impl App {
    /// Fonts, backdrop and blur from the settings, then persist any change.
    fn apply_settings(&mut self, ui: &egui::Ui, frame: &eframe::Frame) {
        let s = &self.settings;
        let lang = crate::i18n::Lang::from_code(&s.language);
        crate::i18n::set(lang);
        let want = (s.font.clone(), s.font_weight, s.bold_weight, lang);
        if self.applied_font.as_ref() != Some(&want) {
            self.font_error = self
                .fonts
                .apply(ui.ctx(), want.0.as_ref(), want.1, want.2, want.3)
                .err();
            self.applied_font = Some(want);
        }
        let sch = theme::scheme(&s.scheme);
        // See-through costs the compositor a lot: whatever is behind the
        // window is blended (and blurred) on every refresh. Nobody is looking
        // through muster while another app is in front, so then it rests
        // opaque, which macOS composites like any other window.
        let resting = self.transparent && !ui.ctx().input(|i| i.viewport().focused.unwrap_or(true));
        let alpha = if resting {
            1.0
        } else {
            s.opacity.clamp(0.3, 1.0)
        };
        theme::set_backdrop(self.transparent, sch.bg, alpha);
        if self.transparent {
            if self.resting_opaque != Some(resting) {
                set_window_opaque(frame, resting);
                self.resting_opaque = Some(resting);
            }
            let r = if resting {
                0
            } else {
                s.blur.clamp(0.0, crate::settings::BLUR_MAX).round() as i64
            };
            if self.applied_blur != Some(r) {
                set_window_blur(frame, r);
                self.applied_blur = Some(r);
            }
        }
        if self.settings != self.saved_settings {
            match self.settings.save() {
                Ok(()) => {
                    self.saved_settings = self.settings.clone();
                    self.config_seen = config_mtime();
                }
                Err(e) => {
                    // Don't retry every frame; the next change tries again.
                    self.saved_settings = self.settings.clone();
                    self.notify(trf!("couldn't save settings: {e}", e = e), true);
                }
            }
        }
    }
}

/// Review comments as one message: the agent's prompt takes a single line.
pub fn review_message(list: &[Remark]) -> String {
    let mut text = String::from(
        "Review comments on your changes. Address each one, then tell me what you changed:",
    );
    for (i, r) in list.iter().enumerate() {
        let at = match r.line {
            Some(l) => format!("{}:{l}", r.file),
            None => r.file.clone(),
        };
        text.push_str(&format!(
            " ({}) {at}, on `{}`: {}",
            i + 1,
            r.quote,
            r.text.trim()
        ));
    }
    text
}

fn config_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(muster_core::paths::config())
        .and_then(|m| m.modified())
        .ok()
}

impl App {
    /// Pick up config.toml edited outside the app, every couple of seconds.
    fn watch_config(&mut self) {
        for p in std::mem::take(&mut self.config_problems) {
            self.notify(p, true);
        }
        if self.config_checked.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.config_checked = Instant::now();
        let now = config_mtime();
        if now == self.config_seen {
            return;
        }
        self.config_seen = now;
        let (s, problems) = Settings::load();
        self.settings = s.clone();
        self.saved_settings = s;
        if problems.is_empty() {
            self.notify(tr("reloaded config.toml"), false);
        }
        for p in problems {
            self.notify(p, true);
        }
    }
}

/// A Notification Center notification from Muster.app. osascript was used
/// before, but macOS files those under Script Editor, which then opens when
/// one is clicked. Without a bundle (`cargo run`) there's nothing to post as,
/// and UNUserNotificationCenter throws, so it's skipped there.
#[cfg(target_os = "macos")]
fn notify_system(title: &str, body: &str) {
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNUserNotificationCenter,
    };

    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    // A repeated identifier replaces the earlier notification.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&format!("muster-{stamp}")),
        &content,
        None,
    );
    let center = UNUserNotificationCenter::currentNotificationCenter();
    // Asks only the first time; after that it answers with the user's choice
    // from System Settings straight away.
    let post = block2::RcBlock::new({
        let center = center.clone();
        move |granted: Bool, _: *mut NSError| {
            if granted.as_bool() {
                center.addNotificationRequest_withCompletionHandler(&request, None);
            }
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &post);
}

/// A desktop notification through notify-send, as keel does.
#[cfg(not(target_os = "macos"))]
fn notify_system(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=muster", title, body])
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
    /// Tell the network thread which terminals are on screen: the cards
    /// showing one (just the maximized card when zoomed), and none while
    /// the window is minimized or covered.
    fn publish_shown(&self, ctx: &egui::Context) {
        let hidden = ctx.input(|i| {
            let v = i.viewport();
            v.minimized == Some(true) || v.occluded == Some(true)
        });
        let now: std::collections::HashSet<String> = if hidden {
            Default::default()
        } else if self.zoom {
            self.cards
                .get(self.active)
                .filter(|c| c.child == Child::Terminal)
                .map(|c| c.item.clone())
                .into_iter()
                .collect()
        } else {
            self.terminal_cards().into_iter().collect()
        };
        if std::env::var_os("MUSTER_TRACE_REPAINT").is_some() {
            let v = ctx.input(|i| {
                (
                    i.viewport().minimized,
                    i.viewport().occluded,
                    i.viewport().focused,
                )
            });
            eprintln!("shown {:?} (minimized, occluded, focused) = {v:?}", now);
        }
        if let Ok(mut s) = self.shown.write() {
            if *s != now {
                *s = now;
            }
        }
    }

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
        .filter(|p| is_dir_cached(p))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&t.cwd))
}

/// `is_dir`, remembered for a couple of seconds: the sidebar asks for every
/// task on every frame.
fn is_dir_cached(p: &str) -> bool {
    thread_local! {
        static SEEN: std::cell::RefCell<HashMap<String, (Instant, bool)>> = Default::default();
    }
    SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        if let Some((at, v)) = seen.get(p) {
            if at.elapsed() < Duration::from_secs(2) {
                return *v;
            }
        }
        let v = Path::new(p).is_dir();
        if seen.len() > 512 {
            seen.clear();
        }
        seen.insert(p.to_string(), (Instant::now(), v));
        v
    })
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

/// Tell macOS the window has no see-through parts, so it skips blending
/// what's behind it.
#[cfg(target_os = "macos")]
fn set_window_opaque(frame: &eframe::Frame, opaque: bool) {
    use objc2::runtime::{AnyObject, Bool};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = frame.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let window: *mut AnyObject = objc2::msg_send![view, window];
        if !window.is_null() {
            let _: () = objc2::msg_send![window, setOpaque: Bool::new(opaque)];
            // A see-through window's shadow is traced from its pixels every
            // time it redraws, which costs the compositor more than the
            // see-through itself. See-through windows go without.
            let _: () = objc2::msg_send![window, setHasShadow: Bool::new(false)];
        }
        // The compositor goes by the layers, not the window's flag: wgpu's
        // Metal layer (the view's, or one under it) was made see-through.
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if !layer.is_null() {
            let _: () = objc2::msg_send![layer, setOpaque: Bool::new(opaque)];
            let subs: *mut AnyObject = objc2::msg_send![layer, sublayers];
            if !subs.is_null() {
                let n: usize = objc2::msg_send![subs, count];
                for i in 0..n {
                    let sub: *mut AnyObject = objc2::msg_send![subs, objectAtIndex: i];
                    let _: () = objc2::msg_send![sub, setOpaque: Bool::new(opaque)];
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn set_window_opaque(_frame: &eframe::Frame, _opaque: bool) {}

/// How much of the window the title bar covers. Asked every frame because
/// it changes going full screen, where the bar hides and this is 0.
#[cfg(target_os = "macos")]
fn titlebar_height(frame: &eframe::Frame) -> f32 {
    use objc2::encode::{Encode, Encoding};
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct(
            "CGRect",
            &[
                Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]),
                Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]),
            ],
        );
    }
    let Ok(handle) = frame.window_handle() else {
        return 0.0;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return 0.0;
    };
    let view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let window: *mut AnyObject = objc2::msg_send![view, window];
        if window.is_null() {
            return 0.0;
        }
        let whole: Rect = objc2::msg_send![window, frame];
        let below: Rect = objc2::msg_send![window, contentLayoutRect];
        (whole.h - below.h).max(0.0) as f32
    }
}

#[cfg(not(target_os = "macos"))]
fn titlebar_height(_frame: &eframe::Frame) -> f32 {
    0.0
}

/// A double-click on the title bar we draw ourselves. winit's view takes the
/// clicks there and never passes them on, so AppKit's own handling of it
/// doesn't run. Follows System Settings → Desktop & Dock; "Fill" falls back to
/// zoom.
pub fn titlebar_double_clicked(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    let action = {
        use objc2_foundation::{NSString, NSUserDefaults};
        NSUserDefaults::standardUserDefaults()
            .stringForKey(&NSString::from_str("AppleActionOnDoubleClick"))
            .map(|s| s.to_string())
    };
    #[cfg(not(target_os = "macos"))]
    let action: Option<String> = None;
    match action.as_deref() {
        Some("None") => {}
        Some("Minimize") => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
        _ => {
            let zoomed = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!zoomed));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if std::env::var_os("MUSTER_TRACE_REPAINT").is_some() {
            eprintln!("frame {:?}", ui.ctx().repaint_causes());
        }
        self.pump();
        self.apply_settings(ui, frame);
        if self.transparent {
            self.titlebar = titlebar_height(frame);
        }
        if let Some(t) = theme::backdrop() {
            ui.painter().rect_filled(ui.max_rect(), 0.0, t);
        }
        crate::views::draw(ui, self);
        self.publish_shown(ui.ctx());
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
        if let Some(p) = self.devshot.as_mut().and_then(|d| d.dropped.take()) {
            raw.dropped_files
                .push(std::sync::Arc::new(crate::devshot::Dropped(p)));
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
    #[test]
    fn a_synchronized_update_is_drawn_whole() {
        let mut t = super::TermState::new("sync", 3, 20);
        let screen = |t: &super::TermState| t.view.parser.lock().unwrap().screen().contents();
        t.take(b"\x1b[?2026hhalf");
        assert_eq!(screen(&t), "", "drawn before the update ended");
        // The end marker split across two reads.
        t.take(b" done\x1b[?20");
        assert_eq!(screen(&t), "");
        t.take(b"26l");
        assert_eq!(screen(&t), "half done");
        // One that's never ended still shows, after a moment.
        t.take(b"\x1b[?2026h more");
        t.held_since = Some(std::time::Instant::now() - super::SYNC_LIMIT * 2);
        t.take(b"!");
        assert_eq!(screen(&t), "half done more!");
    }

    #[test]
    fn review_comments_go_as_one_numbered_line() {
        let r = |file: &str, line, quote: &str, text: &str| super::Remark {
            file: file.into(),
            line,
            quote: quote.into(),
            text: text.into(),
        };
        let m = super::review_message(&[
            r("src/a.rs", Some(42), "+ let x = 1;", " name it count\n"),
            r("README.md", None, "+ hi", "drop this"),
        ]);
        assert!(
            m.contains("(1) src/a.rs:42, on `+ let x = 1;`: name it count"),
            "{m}"
        );
        assert!(m.contains("(2) README.md, on `+ hi`: drop this"), "{m}");
    }

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
