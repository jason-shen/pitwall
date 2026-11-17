//! TUI state and input handling.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use pitwall_core::client::Client;
use pitwall_core::config::Config;
use pitwall_core::proto::{
    Approval, ApprovalSource, NewTask, Push, Request, Response, SnapshotInfo, TaskInfo, TaskStatus,
};
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedSender;

use super::Ev;
use pitwall_core::editor::Editor;
use pitwall_core::git::{self, Change, DiffMode, ParsedDiff};
use pitwall_core::tree::Tree;

pub enum View {
    Term(String),
    Editor(Box<Editor>),
    Diff(Box<DiffView>),
    Timeline(TimelineView),
}

pub struct DiffView {
    pub dir: PathBuf,
    pub mode: DiffMode,
    pub path: Option<String>,
    pub task: Option<String>,
    /// Text supplied by the daemon (snapshot diffs); not refreshable from git.
    pub fixed: Option<String>,
    pub parsed: ParsedDiff,
    pub cursor: usize,
    pub scroll: usize,
    pub follow: bool,
    pub error: Option<String>,
}

impl DiffView {
    pub fn title(&self) -> String {
        if let Some(t) = &self.fixed {
            return t.clone();
        }
        let who = self
            .task
            .as_deref()
            .map(|t| format!(" {t}"))
            .unwrap_or_default();
        match &self.path {
            Some(p) => format!("diff{who}: {} ({})", p, self.mode.label()),
            None => format!("diff{who} ({})", self.mode.label()),
        }
    }

    pub fn reload(&mut self) {
        if self.fixed.is_some() {
            return;
        }
        match git::diff(&self.dir, &self.mode, self.path.as_deref()) {
            Ok(t) => {
                self.parsed = git::parse(&t);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        self.cursor = self.cursor.min(self.parsed.lines.len().saturating_sub(1));
    }
}

pub struct TimelineView {
    pub task: String,
    pub snaps: Vec<SnapshotInfo>,
    pub sel: usize,
    /// First visible row (set by the renderer).
    pub offset: usize,
}

pub struct TermState {
    pub parser: vt100::Parser,
    pub scroll: usize,
    pub size: (u16, u16),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Pane,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Agents,
    Files,
    Changes,
}

pub enum InputKind {
    Commit,
    Find,
    Goto,
}

pub enum ConfirmKind {
    Merge { root: PathBuf, branch: String },
    Revert { task: String, snapshot: String },
    Remove { id: String, prune: bool },
    Discard { dir: PathBuf, change: Change },
    CloseDirty { tab: usize },
}

pub enum Modal {
    NewAgent {
        agents: Vec<String>,
        agent: usize,
        name: String,
        prompt: String,
        worktree: bool,
        field: usize,
    },
    Input {
        kind: InputKind,
        title: String,
        value: String,
    },
    Confirm {
        kind: ConfirmKind,
        text: String,
    },
    Help,
    Approvals {
        sel: usize,
    },
}

pub enum Reply {
    Snapshots {
        task: String,
        list: Vec<SnapshotInfo>,
    },
    DiffText {
        title: String,
        task: String,
        text: String,
    },
    Created(TaskInfo),
    Notice(String, bool),
}

pub struct GitData {
    pub dir: PathBuf,
    pub changes: Result<Vec<Change>, String>,
    pub files: Vec<String>,
    pub truncated: bool,
    pub branch: Option<String>,
    pub root: Option<PathBuf>,
    pub ahead: Option<usize>,
}

/// Screen regions from the last draw, for mouse hit-testing.
#[derive(Default)]
pub struct Hits {
    pub sidebar: Rect,
    pub rows: Vec<(Rect, Section, usize)>,
    pub tabs: Vec<(Rect, usize)>,
    pub panes: Vec<(Rect, usize)>,
}

pub struct App {
    pub client: Client,
    pub tx: UnboundedSender<Ev>,
    pub cfg: Config,
    pub launch_dir: PathBuf,
    pub tasks: BTreeMap<String, TaskInfo>,
    pub approvals: BTreeMap<u64, Approval>,
    pub terms: HashMap<String, TermState>,
    pub tabs: Vec<View>,
    pub panes: Vec<Option<usize>>,
    pub active: usize,
    pub focus: Focus,
    pub section: Section,
    pub agent_sel: usize,
    pub file_sel: usize,
    pub change_sel: usize,
    pub tree: Tree,
    pub changes: Vec<Change>,
    pub git_error: Option<String>,
    pub branch: Option<String>,
    /// Commits on the agent's branch since its base.
    pub ahead: Option<usize>,
    pub ctx_root: Option<PathBuf>,
    pub ctx_task: Option<String>,
    git_inflight: bool,
    last_git: Instant,
    pub zoom: bool,
    pub prefix_armed: bool,
    prefix: (KeyCode, KeyModifiers),
    pub prefix_label: String,
    pub modal: Option<Modal>,
    pub notice: Option<(String, Instant, bool)>,
    pub hits: Hits,
    pub dirty: bool,
    pub quit: bool,
    pub bell: bool,
    pub external: Option<(PathBuf, usize)>,
    pub frame: usize,
    pub connected: bool,
}

fn parse_prefix(s: &str) -> (KeyCode, KeyModifiers) {
    let s = s.trim().to_lowercase();
    let key = s
        .strip_prefix("c-")
        .or_else(|| s.strip_prefix("ctrl-"))
        .or_else(|| s.strip_prefix("ctrl+"))
        .unwrap_or(&s);
    let code = match key {
        "space" => KeyCode::Char(' '),
        k if k.chars().count() == 1 => KeyCode::Char(k.chars().next().unwrap()),
        _ => KeyCode::Char('b'),
    };
    (code, KeyModifiers::CONTROL)
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap()
}

impl App {
    pub fn new(
        client: Client,
        tx: UnboundedSender<Ev>,
        cfg: Config,
        tasks: Vec<TaskInfo>,
        approvals: Vec<Approval>,
    ) -> App {
        let launch_dir = std::env::current_dir().unwrap_or_default();
        let prefix = parse_prefix(&cfg.tui.prefix);
        let prefix_label = format!(
            "^{}",
            match prefix.0 {
                KeyCode::Char(' ') => "space".to_string(),
                KeyCode::Char(c) => c.to_string(),
                _ => "b".into(),
            }
        );
        let mut app = App {
            client,
            tx,
            cfg,
            launch_dir: launch_dir.clone(),
            tasks: tasks.into_iter().map(|t| (t.id.clone(), t)).collect(),
            approvals: approvals.into_iter().map(|a| (a.id, a)).collect(),
            terms: HashMap::new(),
            tabs: vec![],
            panes: vec![None],
            active: 0,
            focus: Focus::Sidebar,
            section: Section::Agents,
            agent_sel: 0,
            file_sel: 0,
            change_sel: 0,
            tree: Tree::default(),
            changes: vec![],
            git_error: None,
            branch: None,
            ahead: None,
            ctx_root: git::root(&launch_dir),
            ctx_task: None,
            git_inflight: false,
            last_git: Instant::now() - Duration::from_secs(60),
            zoom: false,
            prefix_armed: false,
            prefix,
            prefix_label,
            modal: None,
            notice: None,
            hits: Hits::default(),
            dirty: true,
            quit: false,
            bell: false,
            external: None,
            frame: 0,
            connected: true,
        };
        // Open the agent that most needs attention in this repo, if any.
        if let Some(id) = app
            .agent_ids()
            .into_iter()
            .find(|id| app.tasks[id].attention)
            .or_else(|| app.agent_ids().into_iter().next())
        {
            app.agent_sel = app.agent_ids().iter().position(|x| *x == id).unwrap_or(0);
            app.set_ctx_task(Some(id.clone()));
            app.open(View::Term(id));
            app.focus = Focus::Sidebar;
        }
        app
    }

    // ── context ─────────────────────────────────────────────────────────

    /// Agents in display order: this repo's tasks first, then everything else, oldest first.
    pub fn agent_ids(&self) -> Vec<String> {
        let here = git::root(&self.launch_dir).map(|p| p.display().to_string());
        let mut v: Vec<&TaskInfo> = self.tasks.values().collect();
        v.sort_by_key(|t| (t.repo != here, t.created_unix, t.id.clone()));
        v.into_iter().map(|t| t.id.clone()).collect()
    }

    pub fn ctx_dir(&self) -> PathBuf {
        self.ctx_task
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .map(|t| PathBuf::from(&t.cwd))
            .unwrap_or_else(|| self.launch_dir.clone())
    }

    fn set_ctx_task(&mut self, id: Option<String>) {
        if self.ctx_task != id {
            self.ctx_task = id;
            self.last_git = Instant::now() - Duration::from_secs(60);
            self.file_sel = 0;
            self.change_sel = 0;
            self.request_git();
        }
    }

    pub fn notify(&mut self, msg: impl Into<String>, error: bool) {
        self.notice = Some((msg.into(), Instant::now(), error));
        self.dirty = true;
    }

    // ── tabs & panes ────────────────────────────────────────────────────

    fn same_view(a: &View, b: &View) -> bool {
        match (a, b) {
            (View::Term(x), View::Term(y)) => x == y,
            (View::Editor(x), View::Editor(y)) => x.path == y.path,
            (View::Timeline(x), View::Timeline(y)) => x.task == y.task,
            (View::Diff(x), View::Diff(y)) => {
                x.fixed.is_none()
                    && y.fixed.is_none()
                    && x.dir == y.dir
                    && x.mode == y.mode
                    && x.path == y.path
            }
            _ => false,
        }
    }

    pub fn open(&mut self, v: View) -> usize {
        let idx = match self.tabs.iter().position(|t| Self::same_view(t, &v)) {
            Some(i) => {
                if let (View::Diff(d), View::Diff(_)) = (&mut self.tabs[i], &v) {
                    d.reload();
                }
                i
            }
            None => {
                self.tabs.push(v);
                self.tabs.len() - 1
            }
        };
        self.panes[self.active] = Some(idx);
        self.focus = Focus::Pane;
        self.on_active_changed();
        self.dirty = true;
        idx
    }

    pub fn active_view(&self) -> Option<&View> {
        self.panes
            .get(self.active)
            .copied()
            .flatten()
            .and_then(|i| self.tabs.get(i))
    }

    fn active_view_mut(&mut self) -> Option<&mut View> {
        let i = self.panes.get(self.active).copied().flatten()?;
        self.tabs.get_mut(i)
    }

    fn on_active_changed(&mut self) {
        if let Some(View::Term(id)) = self.active_view() {
            let id = id.clone();
            if let Some(pos) = self.agent_ids().iter().position(|x| *x == id) {
                self.agent_sel = pos;
            }
            self.set_ctx_task(Some(id));
        } else if let Some(View::Timeline(t)) = self.active_view() {
            let id = t.task.clone();
            self.set_ctx_task(Some(id));
        }
    }

    fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.panes[self.active] = Some(idx);
            self.focus = Focus::Pane;
            self.on_active_changed();
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let cur = self.panes[self.active].unwrap_or(0) as isize;
        let n = self.tabs.len() as isize;
        self.select_tab(((cur + delta).rem_euclid(n)) as usize);
    }

    fn close_tab(&mut self, idx: usize, force: bool) {
        if idx >= self.tabs.len() {
            return;
        }
        if let View::Editor(e) = &self.tabs[idx] {
            if e.dirty && !force {
                self.modal = Some(Modal::Confirm {
                    kind: ConfirmKind::CloseDirty { tab: idx },
                    text: format!("{} has unsaved changes. Close anyway?", e.name()),
                });
                return;
            }
        }
        if let View::Term(id) = &self.tabs[idx] {
            self.client.send(Request::Detach { id: id.clone() });
            self.terms.remove(id);
        }
        self.tabs.remove(idx);
        for p in self.panes.iter_mut() {
            *p = match *p {
                Some(i) if i == idx => {
                    if self.tabs.is_empty() {
                        None
                    } else {
                        Some(i.min(self.tabs.len() - 1))
                    }
                }
                Some(i) if i > idx => Some(i - 1),
                other => other,
            };
        }
        if self.tabs.is_empty() {
            self.focus = Focus::Sidebar;
        }
        self.on_active_changed();
    }

    fn toggle_split(&mut self) {
        if self.panes.len() == 1 {
            // Prefer the most recent non-terminal tab (editor/diff) next to the agent.
            let cur = self.panes[0];
            let other = (0..self.tabs.len())
                .rev()
                .find(|&i| Some(i) != cur && !matches!(self.tabs[i], View::Term(_)))
                .or_else(|| (0..self.tabs.len()).rev().find(|&i| Some(i) != cur))
                .or(cur);
            self.panes.push(other);
            self.active = 1;
        } else {
            let keep = self.panes[self.active];
            self.panes = vec![keep];
            self.active = 0;
        }
        self.on_active_changed();
    }

    // ── terminals ───────────────────────────────────────────────────────

    /// Called after each draw with the inner size of every visible terminal pane.
    pub fn sync_term(&mut self, id: &str, rows: u16, cols: u16) {
        match self.terms.get_mut(id) {
            None => {
                self.terms.insert(
                    id.to_string(),
                    TermState {
                        parser: vt100::Parser::new(nz(rows), nz(cols), 5000),
                        scroll: 0,
                        size: (rows, cols),
                    },
                );
                self.client.send(Request::Attach {
                    id: id.to_string(),
                    rows,
                    cols,
                    repaints: false,
                });
            }
            Some(t) if t.size != (rows, cols) => {
                t.size = (rows, cols);
                t.parser.set_size(nz(rows), nz(cols));
                self.client.send(Request::Resize {
                    id: id.to_string(),
                    rows,
                    cols,
                });
            }
            _ => {}
        }
    }

    fn send_input(&mut self, id: &str, data: Vec<u8>) {
        if let Some(t) = self.terms.get_mut(id) {
            if t.scroll > 0 {
                t.scroll = 0;
                t.parser.screen_mut().set_scrollback(0);
            }
        }
        self.client.send(Request::Input {
            id: id.to_string(),
            data,
        });
    }

    // ── async helpers ───────────────────────────────────────────────────

    fn spawn_call<F>(&self, req: Request, f: F)
    where
        F: FnOnce(Result<Response, String>) -> Option<Reply> + Send + 'static,
    {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = client.call(req).await.map_err(|e| format!("{e:#}"));
            if let Some(reply) = f(r) {
                let _ = tx.send(Ev::Reply(reply));
            }
        });
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
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let root = git::root(&dir);
            let changes = if root.is_some() {
                git::status(&dir).map_err(|e| format!("{e:#}"))
            } else {
                Ok(vec![])
            };
            let (files, truncated) = pitwall_core::tree::list(&dir);
            let branch = git::current_branch(&dir);
            let ahead = base.map(|b| git::ahead_count(&dir, &b));
            let _ = tx.send(Ev::Git(GitData {
                dir,
                changes,
                files,
                truncated,
                branch,
                root,
                ahead,
            }));
        });
    }

    fn load_snapshots(&self, task: &str) {
        let t = task.to_string();
        self.spawn_call(Request::Snapshots { id: t.clone() }, move |r| match r {
            Ok(Response::Snapshots(list)) => Some(Reply::Snapshots { task: t, list }),
            Err(e) => Some(Reply::Notice(e, true)),
            _ => None,
        });
    }

    // ── events ──────────────────────────────────────────────────────────

    pub fn handle(&mut self, ev: Ev) {
        self.dirty = true;
        match ev {
            Ev::Input(e) => self.on_input(e),
            Ev::Push(p) => self.on_push(p),
            Ev::Reply(r) => self.on_reply(r),
            Ev::Git(g) => self.on_git(g),
            Ev::Tick => self.on_tick(),
            Ev::Disconnected => {
                self.connected = false;
                self.notify(
                    "lost connection to the pitwall daemon — restart pitwall",
                    true,
                );
            }
        }
    }

    fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if self.last_git.elapsed() > Duration::from_millis(1500) {
            self.request_git();
        }
        let theme = self.cfg.tui.theme.clone();
        let mut reloaded = vec![];
        for v in self.tabs.iter_mut() {
            if let View::Editor(e) = v {
                if e.check_disk() {
                    reloaded.push(e.name());
                }
                e.rehighlight_if_due(&theme);
            }
        }
        if !reloaded.is_empty() {
            self.notify(
                format!("reloaded {} (changed on disk)", reloaded.join(", ")),
                false,
            );
        }
        if let Some((_, at, _)) = &self.notice {
            if at.elapsed() > Duration::from_secs(6) {
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
        let changed = match &g.changes {
            Ok(c) => *c != self.changes,
            Err(_) => false,
        };
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
        self.tree.set(&g.dir, g.files, g.truncated);
        self.file_sel = self.file_sel.min(self.tree.rows.len().saturating_sub(1));
        self.change_sel = self.change_sel.min(self.changes.len().saturating_sub(1));
        if changed {
            let dir = g.dir.clone();
            for v in self.tabs.iter_mut() {
                if let View::Diff(d) = v {
                    if d.dir == dir {
                        d.reload();
                    }
                }
            }
        }
    }

    fn on_push(&mut self, p: Push) {
        match p {
            Push::Screen {
                id,
                rows,
                cols,
                data,
            } => {
                let mut parser = vt100::Parser::new(nz(rows), nz(cols), 5000);
                parser.process(&data);
                let size = self.terms.get(&id).map(|t| t.size).unwrap_or((rows, cols));
                self.terms.insert(
                    id,
                    TermState {
                        parser,
                        scroll: 0,
                        size,
                    },
                );
            }
            Push::Output { id, data } => {
                if let Some(t) = self.terms.get_mut(&id) {
                    t.parser.process(&data);
                }
            }
            Push::TaskUpdated(t) => {
                let was = self.tasks.get(&t.id).map(|o| o.attention).unwrap_or(false);
                if t.attention && !was {
                    let watching = self.focus == Focus::Pane
                        && matches!(self.active_view(), Some(View::Term(id)) if *id == t.id);
                    if !watching {
                        self.bell = self.cfg.tui.bell;
                        let why = match &t.status {
                            TaskStatus::Waiting { reason } => reason.clone(),
                            TaskStatus::Idle => "finished its turn".into(),
                            other => other.label(),
                        };
                        self.notify(format!("{} needs you: {why}", t.id), false);
                    }
                }
                self.tasks.insert(t.id.clone(), t);
            }
            Push::TaskRemoved { id } => {
                self.tasks.remove(&id);
                while let Some(i) = self.tabs.iter().position(|v| matches!(v, View::Term(x) | View::Timeline(TimelineView { task: x, .. }) if *x == id)) {
                    self.close_tab(i, true);
                }
                if self.ctx_task.as_deref() == Some(&id) {
                    self.set_ctx_task(None);
                }
                self.agent_sel = self.agent_sel.min(self.tasks.len().saturating_sub(1));
            }
            Push::ApprovalAdded(a) => {
                self.bell = self.cfg.tui.bell;
                self.approvals.insert(a.id, a);
            }
            Push::ApprovalResolved { id, .. } => {
                self.approvals.remove(&id);
            }
            Push::SnapshotTaken { task, snapshot } => {
                for v in self.tabs.iter_mut() {
                    if let View::Timeline(t) = v {
                        if t.task == task && !t.snaps.iter().any(|s| s.id == snapshot.id) {
                            t.snaps.insert(0, snapshot.clone());
                            if t.sel > 0 {
                                t.sel += 1;
                            }
                        }
                    }
                }
            }
            Push::Ops(_) | Push::Message { .. } => {}
        }
    }

    fn on_reply(&mut self, r: Reply) {
        match r {
            Reply::Snapshots { task, list } => {
                for v in self.tabs.iter_mut() {
                    if let View::Timeline(t) = v {
                        if t.task == task {
                            t.snaps = list.clone();
                            t.sel = t.sel.min(list.len().saturating_sub(1));
                        }
                    }
                }
            }
            Reply::DiffText { title, task, text } => {
                let dir = self
                    .tasks
                    .get(&task)
                    .map(|t| PathBuf::from(&t.cwd))
                    .unwrap_or_default();
                let parsed = git::parse(&text);
                self.open(View::Diff(Box::new(DiffView {
                    dir,
                    mode: DiffMode::Unstaged,
                    path: None,
                    task: Some(task),
                    fixed: Some(title),
                    parsed,
                    cursor: 0,
                    scroll: 0,
                    follow: true,
                    error: None,
                })));
            }
            Reply::Created(t) => {
                let id = t.id.clone();
                self.tasks.insert(id.clone(), t);
                self.agent_sel = self.agent_ids().iter().position(|x| *x == id).unwrap_or(0);
                self.open(View::Term(id.clone()));
                self.set_ctx_task(Some(id.clone()));
                self.notify(format!("started {id}"), false);
            }
            Reply::Notice(m, e) => self.notify(m, e),
        }
    }

    // ── input ───────────────────────────────────────────────────────────

    fn on_input(&mut self, e: Event) {
        match e {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(s) => self.on_paste(s),
            _ => {}
        }
    }

    fn is_prefix(&self, k: &KeyEvent) -> bool {
        k.modifiers.contains(KeyModifiers::CONTROL)
            && match (k.code, self.prefix.0) {
                (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
                _ => false,
            }
    }

    fn on_paste(&mut self, s: String) {
        if let Some(m) = &mut self.modal {
            match m {
                Modal::Input { value, .. } => value.push_str(s.lines().next().unwrap_or("")),
                Modal::NewAgent {
                    name,
                    prompt,
                    field,
                    ..
                } => {
                    if *field == 2 {
                        prompt.push_str(&s.replace('\n', " "))
                    } else if *field == 1 {
                        name.push_str(s.lines().next().unwrap_or(""))
                    }
                }
                _ => {}
            }
            return;
        }
        if self.focus != Focus::Pane {
            return;
        }
        match self.active_view_mut() {
            Some(View::Term(id)) => {
                let id = id.clone();
                let bracketed = self
                    .terms
                    .get(&id)
                    .map(|t| t.parser.screen().bracketed_paste())
                    .unwrap_or(false);
                let data = if bracketed {
                    format!("\x1b[200~{s}\x1b[201~")
                } else {
                    s.replace('\n', "\r")
                };
                self.send_input(&id, data.into_bytes());
            }
            Some(View::Editor(e)) => {
                e.insert_str(&s);
                e.follow = true;
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.modal.is_some() {
            self.modal_key(k);
            return;
        }
        if self.prefix_armed {
            self.prefix_armed = false;
            if self.is_prefix(&k) {
                if let Some(View::Term(id)) = self.active_view() {
                    let id = id.clone();
                    if let Some(b) = super::term::encode(&k, false) {
                        self.send_input(&id, b);
                    }
                }
            } else {
                self.command(k);
            }
            return;
        }
        if self.is_prefix(&k) {
            self.prefix_armed = true;
            return;
        }
        match self.focus {
            Focus::Sidebar => self.sidebar_key(k),
            Focus::Pane => match self.active_view() {
                Some(View::Term(_)) => self.term_key(k),
                Some(View::Editor(_)) => self.editor_key(k),
                Some(View::Diff(_)) => self.diff_key(k),
                Some(View::Timeline(_)) => self.timeline_key(k),
                None => self.sidebar_key(k),
            },
        }
    }

    /// Prefix commands (work from anywhere).
    fn command(&mut self, k: KeyEvent) {
        let code = match k.code {
            KeyCode::Left => KeyCode::Char('h'),
            KeyCode::Right => KeyCode::Char('l'),
            c => c,
        };
        match code {
            KeyCode::Char('c') => self.new_agent_modal(),
            KeyCode::Char('t') => self.new_shell(),
            KeyCode::Char('n') => self.cycle_tab(1),
            KeyCode::Char('p') => self.cycle_tab(-1),
            KeyCode::Char(d @ '1'..='9') => self.select_tab(d as usize - '1' as usize),
            KeyCode::Char('x') => {
                if let Some(i) = self.panes[self.active] {
                    self.close_tab(i, false)
                }
            }
            KeyCode::Char('s') => self.toggle_split(),
            KeyCode::Char('o') => {
                if self.panes.len() > 1 {
                    self.active = 1 - self.active;
                    self.focus = Focus::Pane;
                    self.on_active_changed();
                }
            }
            KeyCode::Char('z') => self.zoom = !self.zoom,
            KeyCode::Char('h') => {
                self.zoom = false;
                self.focus = Focus::Sidebar;
            }
            KeyCode::Char('l') => {
                if self.active_view().is_some() {
                    self.focus = Focus::Pane
                }
            }
            KeyCode::Char('w') => self.focus_section(Section::Agents),
            KeyCode::Char('f') => self.focus_section(Section::Files),
            KeyCode::Char('g') => self.focus_section(Section::Changes),
            KeyCode::Char('d') => self.open_diff(None),
            KeyCode::Char('u') => self.open_timeline(),
            KeyCode::Char('e') => self.external_edit(),
            KeyCode::Char('a') => self.next_attention(),
            KeyCode::Char('y') => self.decide_first(true),
            KeyCode::Char('r') => self.decide_first(false),
            KeyCode::Char('A') => self.modal = Some(Modal::Approvals { sel: 0 }),
            KeyCode::Char('m') => self.merge_prompt(),
            KeyCode::Char('K') => {
                if let Some(id) = self.ctx_task.clone() {
                    self.client.send(Request::Kill { id: id.clone() });
                    self.notify(format!("killed {id}"), false);
                }
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn focus_section(&mut self, s: Section) {
        self.zoom = false;
        self.focus = Focus::Sidebar;
        self.section = s;
    }

    fn next_attention(&mut self) {
        let ids = self.agent_ids();
        let cur = match self.active_view() {
            Some(View::Term(id)) => ids.iter().position(|x| x == id),
            _ => None,
        };
        let start = cur.map(|c| c + 1).unwrap_or(0);
        let n = ids.len();
        for i in 0..n {
            let id = &ids[(start + i) % n];
            if self.tasks[id].attention {
                let id = id.clone();
                self.agent_sel = (start + i) % n;
                self.open(View::Term(id));
                return;
            }
        }
        self.notify("nothing needs you", false);
    }

    fn decide_first(&mut self, allow: bool) {
        // Prefer approvals for the task you're looking at.
        let ctx = self.ctx_task.clone();
        let pick = self
            .approvals
            .values()
            .find(|a| matches!(&a.source, ApprovalSource::Task { task_id, .. } if Some(task_id) == ctx.as_ref()))
            .or_else(|| self.approvals.values().next())
            .map(|a| (a.id, a.command.clone()));
        match pick {
            Some((id, cmd)) => {
                self.client.send(Request::Decide { id, allow });
                self.notify(
                    format!("{} `{cmd}`", if allow { "approved" } else { "rejected" }),
                    false,
                );
            }
            None => self.notify("no pending approvals", false),
        }
    }

    // ── actions ─────────────────────────────────────────────────────────

    fn new_agent_modal(&mut self) {
        let mut agents: Vec<String> = self
            .cfg
            .agents
            .keys()
            .filter(|a| *a != "shell")
            .cloned()
            .collect();
        agents.sort_by_key(|a| (a != "claude", a.clone()));
        let in_repo = git::root(&self.ctx_dir()).is_some();
        self.modal = Some(Modal::NewAgent {
            agents,
            agent: 0,
            name: String::new(),
            prompt: String::new(),
            worktree: in_repo,
            field: 1,
        });
    }

    fn create_task(&mut self, agent: String, name: String, prompt: Option<String>, worktree: bool) {
        let dir = self.ctx_dir();
        // New agents branch from the *main* repo, not from another task's worktree.
        let base_dir = self
            .ctx_task
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .and_then(|t| t.repo.clone())
            .map(PathBuf::from)
            .unwrap_or(dir.clone());
        let repo = git::root(&base_dir).map(|p| p.display().to_string());
        let size = self
            .hits
            .panes
            .first()
            .map(|(r, _)| (r.height.saturating_sub(2), r.width.saturating_sub(2)))
            .unwrap_or((40, 120));
        let req = NewTask {
            name,
            agent,
            no_worktree: !worktree || repo.is_none(),
            cwd: Some(if worktree {
                base_dir.display().to_string()
            } else {
                dir.display().to_string()
            }),
            repo,
            base: None,
            args: vec![],
            prompt,
            rows: size.0.max(10),
            cols: size.1.max(40),
            brief: None,
            host: None,
        };
        self.spawn_call(Request::NewTask(req), |r| match r {
            Ok(Response::Task(t)) => Some(Reply::Created(t)),
            Err(e) => Some(Reply::Notice(e, true)),
            _ => None,
        });
    }

    fn new_shell(&mut self) {
        let n = self.tasks.values().filter(|t| t.agent == "shell").count() + 1;
        let mut name = format!("sh-{n}");
        let mut i = n;
        while self.tasks.contains_key(&name) {
            i += 1;
            name = format!("sh-{i}");
        }
        self.create_task("shell".into(), name, None, false);
    }

    fn open_diff(&mut self, path: Option<String>) {
        let dir = self.ctx_dir();
        if git::root(&dir).is_none() {
            return self.notify("not a git repository", true);
        }
        let task = self.ctx_task.clone();
        let base = task
            .as_ref()
            .and_then(|id| self.tasks.get(id))
            .and_then(|t| t.base.clone());
        let mode = match (&path, base) {
            (None, Some(b)) => DiffMode::Base(b),
            _ => {
                let staged_only = path
                    .as_ref()
                    .and_then(|p| self.changes.iter().find(|c| &c.path == p))
                    .map(|c| c.staged() && !c.unstaged())
                    .unwrap_or(false);
                if staged_only {
                    DiffMode::Staged
                } else {
                    DiffMode::Unstaged
                }
            }
        };
        let mut d = DiffView {
            dir,
            mode,
            path,
            task,
            fixed: None,
            parsed: git::parse(""),
            cursor: 0,
            scroll: 0,
            follow: true,
            error: None,
        };
        d.reload();
        self.open(View::Diff(Box::new(d)));
    }

    fn open_timeline(&mut self) {
        let Some(task) = self.ctx_task.clone() else {
            return self.notify("select an agent first", true);
        };
        self.load_snapshots(&task);
        self.open(View::Timeline(TimelineView {
            task,
            snaps: vec![],
            sel: 0,
            offset: 0,
        }));
    }

    fn open_file(&mut self, rel: &str, line: Option<usize>) {
        let path = self.ctx_dir().join(rel);
        if path.is_dir() {
            return;
        }
        self.tree.reveal(rel);
        if let Some(i) = self.tree.rows.iter().position(|r| r.rel == rel) {
            self.file_sel = i;
        }
        match Editor::open(&path, self.cfg.tui.tab_width) {
            Ok(mut e) => {
                if let Some(l) = line {
                    e.goto(l);
                }
                let idx = self.open(View::Editor(Box::new(e)));
                if let (Some(l), View::Editor(ed)) = (line, &mut self.tabs[idx]) {
                    ed.goto(l);
                    ed.follow = true;
                }
            }
            Err(e) => self.notify(format!("open {rel}: {e:#}"), true),
        }
    }

    fn external_edit(&mut self) {
        let target = match self.active_view() {
            Some(View::Editor(e)) if self.focus == Focus::Pane => Some((e.path.clone(), e.cy + 1)),
            _ => None,
        };
        let target = target.or_else(|| match self.section {
            Section::Files => self
                .tree
                .rows
                .get(self.file_sel)
                .filter(|r| !r.is_dir)
                .map(|r| (self.ctx_dir().join(&r.rel), 1)),
            Section::Changes => self
                .changes
                .get(self.change_sel)
                .map(|c| (self.ctx_dir().join(&c.path), 1)),
            _ => None,
        });
        match target {
            Some(t) => self.external = Some(t),
            None => self.notify("open a file first", true),
        }
    }

    pub fn after_external(&mut self, path: &Path) {
        for v in self.tabs.iter_mut() {
            if let View::Editor(e) = v {
                if e.path == path && !e.dirty {
                    e.reload();
                }
            }
        }
        self.request_git();
    }

    fn merge_prompt(&mut self) {
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
        let dirty = self.changes.len();
        let mut text = format!("Merge {branch} into {target} in {}?", root.display());
        if dirty > 0 {
            text.push_str(&format!(
                "\n{dirty} uncommitted change(s) in the task are NOT included — commit them first."
            ));
        }
        self.modal = Some(Modal::Confirm {
            kind: ConfirmKind::Merge { root, branch },
            text,
        });
    }

    fn git_op(&mut self, f: impl FnOnce(&Path) -> anyhow::Result<String>) {
        let dir = self.ctx_dir();
        match f(&dir) {
            Ok(msg) => {
                if !msg.is_empty() {
                    self.notify(msg, false);
                }
            }
            Err(e) => self.notify(format!("{e:#}"), true),
        }
        self.last_git = Instant::now() - Duration::from_secs(60);
        self.git_inflight = false;
        self.request_git();
        for v in self.tabs.iter_mut() {
            if let View::Diff(d) = v {
                d.reload();
            }
        }
    }

    // ── sidebar ─────────────────────────────────────────────────────────

    fn sidebar_len(&self) -> usize {
        match self.section {
            Section::Agents => self.tasks.len(),
            Section::Files => self.tree.rows.len(),
            Section::Changes => self.changes.len(),
        }
    }

    fn sidebar_move(&mut self, delta: isize) {
        let n = self.sidebar_len();
        if n == 0 {
            return;
        }
        let sel = match self.section {
            Section::Agents => &mut self.agent_sel,
            Section::Files => &mut self.file_sel,
            Section::Changes => &mut self.change_sel,
        };
        *sel = (*sel as isize + delta).clamp(0, n as isize - 1) as usize;
        if self.section == Section::Agents {
            let id = self.agent_ids().get(self.agent_sel).cloned();
            self.set_ctx_task(id);
        }
    }

    fn sidebar_activate(&mut self) {
        match self.section {
            Section::Agents => {
                if let Some(id) = self.agent_ids().get(self.agent_sel).cloned() {
                    self.set_ctx_task(Some(id.clone()));
                    self.open(View::Term(id));
                }
            }
            Section::Files => {
                if let Some(r) = self.tree.rows.get(self.file_sel).cloned() {
                    if r.is_dir {
                        self.tree.toggle(&r.rel);
                    } else {
                        self.open_file(&r.rel, None);
                    }
                }
            }
            Section::Changes => {
                if let Some(c) = self.changes.get(self.change_sel).cloned() {
                    self.open_diff(Some(c.path));
                }
            }
        }
    }

    fn sidebar_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.sidebar_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.sidebar_move(-1),
            KeyCode::PageDown => self.sidebar_move(10),
            KeyCode::PageUp => self.sidebar_move(-10),
            KeyCode::Tab => {
                self.section = match self.section {
                    Section::Agents => Section::Files,
                    Section::Files => Section::Changes,
                    Section::Changes => Section::Agents,
                }
            }
            KeyCode::BackTab => {
                self.section = match self.section {
                    Section::Agents => Section::Changes,
                    Section::Files => Section::Agents,
                    Section::Changes => Section::Files,
                }
            }
            KeyCode::Enter => self.sidebar_activate(),
            KeyCode::Char('l') | KeyCode::Right => {
                if self.section == Section::Files {
                    if let Some(r) = self.tree.rows.get(self.file_sel).cloned() {
                        if r.is_dir && !r.expanded {
                            self.tree.toggle(&r.rel);
                            return;
                        }
                    }
                }
                if self.active_view().is_some() {
                    self.focus = Focus::Pane;
                }
            }
            KeyCode::Char('h') | KeyCode::Left if self.section == Section::Files => {
                if let Some(r) = self.tree.rows.get(self.file_sel).cloned() {
                    if r.is_dir && r.expanded {
                        self.tree.toggle(&r.rel);
                    } else if let Some((parent, _)) = r.rel.rsplit_once('/') {
                        if let Some(i) = self.tree.rows.iter().position(|x| x.rel == parent) {
                            self.file_sel = i;
                        }
                    }
                }
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('e') => self.external_edit(),
            KeyCode::Char('d') => self.open_diff(None),
            KeyCode::Char('u') => self.open_timeline(),
            _ => match self.section {
                Section::Agents => match k.code {
                    KeyCode::Char('c') | KeyCode::Char('n') => self.new_agent_modal(),
                    KeyCode::Char('t') => self.new_shell(),
                    KeyCode::Char('K') => {
                        if let Some(id) = self.agent_ids().get(self.agent_sel).cloned() {
                            self.client.send(Request::Kill { id: id.clone() });
                            self.notify(format!("killed {id}"), false);
                        }
                    }
                    KeyCode::Char('D') | KeyCode::Delete => {
                        if let Some(id) = self.agent_ids().get(self.agent_sel).cloned() {
                            let has_wt = self
                                .tasks
                                .get(&id)
                                .and_then(|t| t.branch.as_ref())
                                .is_some();
                            self.modal = Some(Modal::Confirm {
                                kind: ConfirmKind::Remove {
                                    id: id.clone(),
                                    prune: has_wt,
                                },
                                text: if has_wt {
                                    format!("Remove {id} and delete its worktree? (branch and snapshots are kept)")
                                } else {
                                    format!("Remove {id}?")
                                },
                            });
                        }
                    }
                    KeyCode::Char('m') => self.merge_prompt(),
                    KeyCode::Char('a') => self.next_attention(),
                    KeyCode::Char('y') => self.decide_first(true),
                    KeyCode::Char('r') => self.decide_first(false),
                    _ => {}
                },
                Section::Files => {}
                Section::Changes => match k.code {
                    KeyCode::Char('s') | KeyCode::Char(' ') => {
                        if let Some(c) = self.changes.get(self.change_sel).cloned() {
                            self.git_op(|d| {
                                if c.unstaged() || c.untracked() {
                                    git::stage_file(d, &c.path)
                                        .map(|_| format!("staged {}", c.path))
                                } else {
                                    git::unstage_file(d, &c.path)
                                        .map(|_| format!("unstaged {}", c.path))
                                }
                            });
                        }
                    }
                    KeyCode::Char('A') => {
                        self.git_op(|d| git::stage_all(d).map(|_| "staged everything".into()))
                    }
                    KeyCode::Char('c') => {
                        if !self.changes.iter().any(|c| c.staged()) {
                            self.notify(
                                "nothing staged — press s on a file or A to stage all",
                                true,
                            );
                        } else {
                            self.modal = Some(Modal::Input {
                                kind: InputKind::Commit,
                                title: "Commit message".into(),
                                value: String::new(),
                            });
                        }
                    }
                    KeyCode::Char('m') => self.merge_prompt(),
                    KeyCode::Char('x') => {
                        if let Some(c) = self.changes.get(self.change_sel).cloned() {
                            let text = format!("Discard changes to {}? This cannot be undone with git (pitwall snapshots still have it).", c.path);
                            self.modal = Some(Modal::Confirm {
                                kind: ConfirmKind::Discard {
                                    dir: self.ctx_dir(),
                                    change: c,
                                },
                                text,
                            });
                        }
                    }
                    _ => {}
                },
            },
        }
    }

    // ── panes ───────────────────────────────────────────────────────────

    fn term_key(&mut self, k: KeyEvent) {
        let Some(View::Term(id)) = self.active_view() else {
            return;
        };
        let id = id.clone();
        // Shift+PageUp/Down scroll locally.
        if k.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(k.code, KeyCode::PageUp | KeyCode::PageDown)
        {
            let up = k.code == KeyCode::PageUp;
            self.scroll_term(&id, if up { 10 } else { -10 });
            return;
        }
        let app_cursor = self
            .terms
            .get(&id)
            .map(|t| t.parser.screen().application_cursor())
            .unwrap_or(false);
        if let Some(b) = super::term::encode(&k, app_cursor) {
            self.send_input(&id, b);
        }
    }

    fn scroll_term(&mut self, id: &str, lines: isize) {
        if let Some(t) = self.terms.get_mut(id) {
            let v = (t.scroll as isize + lines).max(0) as usize;
            t.parser.screen_mut().set_scrollback(v);
            t.scroll = t.parser.screen().scrollback();
        }
    }

    fn editor_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let mut action: Option<&str> = None;
        let mut notice: Option<(String, bool)> = None;
        {
            let Some(View::Editor(e)) = self.active_view_mut() else {
                return;
            };
            let page = 20;
            match k.code {
                KeyCode::Char('s') if ctrl => match e.save() {
                    Ok(_) => notice = Some((format!("saved {}", e.name()), false)),
                    Err(err) => notice = Some((format!("save failed: {err:#}"), true)),
                },
                KeyCode::Char('z') if ctrl && shift => e.redo(),
                KeyCode::Char('z') if ctrl => e.undo(),
                KeyCode::Char('y') if ctrl => e.redo(),
                KeyCode::Char('f') if ctrl => action = Some("find"),
                KeyCode::Char('g') if ctrl => action = Some("goto"),
                KeyCode::Char('n') if ctrl => action = Some("findnext"),
                KeyCode::F(3) => action = Some("findnext"),
                KeyCode::Char('e') if ctrl => action = Some("external"),
                KeyCode::Char('k') if ctrl => e.delete_line(),
                KeyCode::Char('r') if ctrl => {
                    e.reload();
                    notice = Some(("reloaded from disk".into(), false));
                }
                KeyCode::Char(c) if !ctrl && !alt => e.insert_char(c),
                KeyCode::Enter => e.newline(),
                KeyCode::Tab => e.tab(),
                KeyCode::Backspace => e.backspace(),
                KeyCode::Delete => e.delete(),
                KeyCode::Left if ctrl || alt => e.word_left(),
                KeyCode::Right if ctrl || alt => e.word_right(),
                KeyCode::Left => e.left(),
                KeyCode::Right => e.right(),
                KeyCode::Up => e.vertical(-1),
                KeyCode::Down => e.vertical(1),
                KeyCode::Home if ctrl => e.top(),
                KeyCode::End if ctrl => e.bottom(),
                KeyCode::Home => e.home(),
                KeyCode::End => e.end(),
                KeyCode::PageUp => e.vertical(-page),
                KeyCode::PageDown => e.vertical(page),
                KeyCode::Esc => action = Some("sidebar"),
                _ => {}
            }
            e.follow = true;
        }
        if let Some((m, err)) = notice {
            self.notify(m, err);
            self.request_git();
        }
        match action {
            Some("find") => {
                self.modal = Some(Modal::Input {
                    kind: InputKind::Find,
                    title: "Find".into(),
                    value: String::new(),
                })
            }
            Some("goto") => {
                self.modal = Some(Modal::Input {
                    kind: InputKind::Goto,
                    title: "Go to line".into(),
                    value: String::new(),
                })
            }
            Some("findnext") => {
                if let Some(View::Editor(e)) = self.active_view_mut() {
                    if let Some(q) = e.last_search.clone() {
                        if !e.find_next(&q) {
                            self.notify(format!("`{q}` not found"), true);
                        }
                    }
                }
            }
            Some("external") => self.external_edit(),
            Some("sidebar") => self.focus = Focus::Sidebar,
            _ => {}
        }
    }

    fn diff_key(&mut self, k: KeyEvent) {
        let mut op: Option<(char, git::Hunk)> = None;
        let mut file_op: Option<(char, String)> = None;
        let mut open: Option<(String, usize)> = None;
        let mut to_sidebar = false;
        let base_for = match self.active_view() {
            Some(View::Diff(d)) => d
                .task
                .as_ref()
                .and_then(|t| self.tasks.get(t))
                .and_then(|t| t.base.clone()),
            _ => None,
        };
        {
            let Some(View::Diff(d)) = self.active_view_mut() else {
                return;
            };
            let n = d.parsed.lines.len();
            let hunk_starts: Vec<usize> = d
                .parsed
                .lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.kind == git::LineKind::Hunk)
                .map(|(i, _)| i)
                .collect();
            match k.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    d.cursor = (d.cursor + 1).min(n.saturating_sub(1))
                }
                KeyCode::Char('k') | KeyCode::Up => d.cursor = d.cursor.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    d.cursor = (d.cursor + 20).min(n.saturating_sub(1))
                }
                KeyCode::PageUp => d.cursor = d.cursor.saturating_sub(20),
                KeyCode::Char('g') | KeyCode::Home => d.cursor = 0,
                KeyCode::Char('G') | KeyCode::End => d.cursor = n.saturating_sub(1),
                KeyCode::Char('n') | KeyCode::Char(']') => {
                    if let Some(&h) = hunk_starts.iter().find(|&&h| h > d.cursor) {
                        d.cursor = h;
                    }
                }
                KeyCode::Char('p') | KeyCode::Char('[') => {
                    if let Some(&h) = hunk_starts.iter().rev().find(|&&h| h < d.cursor) {
                        d.cursor = h;
                    }
                }
                KeyCode::Char('1') if d.fixed.is_none() => {
                    d.mode = DiffMode::Unstaged;
                    d.reload();
                }
                KeyCode::Char('2') if d.fixed.is_none() => {
                    d.mode = DiffMode::Staged;
                    d.reload();
                }
                KeyCode::Char('3') if d.fixed.is_none() => match base_for.clone() {
                    Some(b) => {
                        d.mode = DiffMode::Base(b);
                        d.reload();
                    }
                    None => d.error = Some("no base: this view isn't for an agent worktree".into()),
                },
                KeyCode::Char('r') => d.reload(),
                KeyCode::Char('s') | KeyCode::Char('u') if d.fixed.is_none() => {
                    let line = d.parsed.lines.get(d.cursor);
                    if let Some(h) = line
                        .and_then(|l| l.hunk)
                        .and_then(|i| d.parsed.hunks.get(i))
                    {
                        let c = if let KeyCode::Char(c) = k.code {
                            c
                        } else {
                            's'
                        };
                        op = Some((c, h.clone()));
                    }
                }
                KeyCode::Char('S') | KeyCode::Char('U') if d.fixed.is_none() => {
                    if let Some(f) = d.parsed.lines.get(d.cursor).and_then(|l| l.file.clone()) {
                        let c = if let KeyCode::Char(c) = k.code {
                            c
                        } else {
                            'S'
                        };
                        file_op = Some((c, f));
                    }
                }
                KeyCode::Enter | KeyCode::Char('e') => {
                    if let Some(l) = d.parsed.lines.get(d.cursor) {
                        if let Some(f) = &l.file {
                            open = Some((f.clone(), l.new_line.unwrap_or(1)));
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('h') => to_sidebar = true,
                _ => {}
            }
            d.follow = true;
        }
        if to_sidebar {
            self.focus = Focus::Sidebar;
        }
        if let Some((c, h)) = op {
            let mode = match self.active_view() {
                Some(View::Diff(d)) => d.mode.clone(),
                _ => DiffMode::Unstaged,
            };
            match (c, &mode) {
                ('s', DiffMode::Unstaged) => {
                    self.git_op(|dir| git::stage_hunk(dir, &h).map(|_| "staged hunk".into()))
                }
                ('u', DiffMode::Staged) => {
                    self.git_op(|dir| git::unstage_hunk(dir, &h).map(|_| "unstaged hunk".into()))
                }
                ('s', _) => self.notify(
                    "switch to unstaged (1) to stage hunks, or S to stage the file",
                    true,
                ),
                _ => self.notify("switch to staged (2) to unstage hunks", true),
            }
        }
        if let Some((c, f)) = file_op {
            // diff paths are repo-relative; make them relative to the context dir
            let root = git::root(&self.ctx_dir()).unwrap_or_else(|| self.ctx_dir());
            let abs = root.join(&f);
            let rel = abs
                .strip_prefix(self.ctx_dir())
                .map(|p| p.display().to_string())
                .unwrap_or(f.clone());
            if c == 'S' {
                self.git_op(|dir| git::stage_file(dir, &rel).map(|_| format!("staged {rel}")));
            } else {
                self.git_op(|dir| git::unstage_file(dir, &rel).map(|_| format!("unstaged {rel}")));
            }
        }
        if let Some((f, line)) = open {
            let root = git::root(&self.ctx_dir()).unwrap_or_else(|| self.ctx_dir());
            let abs = root.join(&f);
            let rel = abs
                .strip_prefix(self.ctx_dir())
                .map(|p| p.display().to_string())
                .unwrap_or(f);
            self.open_file(&rel, Some(line));
        }
    }

    fn timeline_key(&mut self, k: KeyEvent) {
        let Some(View::Timeline(t)) = self.active_view_mut() else {
            return;
        };
        let n = t.snaps.len();
        let task = t.task.clone();
        let cur = t.snaps.get(t.sel).cloned();
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => t.sel = (t.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => t.sel = t.sel.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => t.sel = 0,
            KeyCode::Char('G') | KeyCode::End => t.sel = n.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('d') => {
                if let Some(s) = cur {
                    let title = format!("{task} #{}: {}", s.seq, s.label);
                    let t2 = task.clone();
                    self.spawn_call(
                        Request::Diff {
                            id: task,
                            from: Some(format!("{}^", s.id)),
                            to: Some(s.id.clone()),
                        },
                        move |r| match r {
                            Ok(Response::Diff(text)) => Some(Reply::DiffText {
                                title,
                                task: t2,
                                text,
                            }),
                            Err(e) => Some(Reply::Notice(e, true)),
                            _ => None,
                        },
                    );
                }
            }
            KeyCode::Char('D') => {
                let t2 = task.clone();
                self.spawn_call(
                    Request::Diff {
                        id: task.clone(),
                        from: None,
                        to: None,
                    },
                    move |r| match r {
                        Ok(Response::Diff(text)) => Some(Reply::DiffText {
                            title: format!("{t2}: everything since start"),
                            task: t2,
                            text,
                        }),
                        Err(e) => Some(Reply::Notice(e, true)),
                        _ => None,
                    },
                );
            }
            KeyCode::Char('r') => {
                if let Some(s) = cur {
                    self.modal = Some(Modal::Confirm {
                        kind: ConfirmKind::Revert { task, snapshot: s.id.clone() },
                        text: format!("Revert files to #{} \"{}\"?\nA safety snapshot is taken first, so this can be undone.", s.seq, s.label),
                    });
                }
            }
            KeyCode::Char('c') => {
                self.spawn_call(
                    Request::TakeSnapshot {
                        id: task,
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
            KeyCode::Char('R') => self.load_snapshots(&task),
            KeyCode::Esc | KeyCode::Char('h') => self.focus = Focus::Sidebar,
            _ => {}
        }
    }

    // ── modals ──────────────────────────────────────────────────────────

    fn modal_key(&mut self, k: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match modal {
            Modal::Help => {}
            Modal::Approvals { mut sel } => {
                let ids: Vec<u64> = self.approvals.keys().copied().collect();
                match k.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        sel = (sel + 1).min(ids.len().saturating_sub(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Char('y') | KeyCode::Enter => {
                        if let Some(id) = ids.get(sel) {
                            self.client.send(Request::Decide {
                                id: *id,
                                allow: true,
                            });
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('r') => {
                        if let Some(id) = ids.get(sel) {
                            self.client.send(Request::Decide {
                                id: *id,
                                allow: false,
                            });
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => return,
                    _ => {}
                }
                if ids.len() > 1
                    || !matches!(
                        k.code,
                        KeyCode::Char('y')
                            | KeyCode::Char('n')
                            | KeyCode::Char('r')
                            | KeyCode::Enter
                    )
                {
                    self.modal = Some(Modal::Approvals { sel });
                }
            }
            Modal::Confirm { kind, text } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirm(kind),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::Confirm { kind, text }),
            },
            Modal::Input {
                kind,
                title,
                mut value,
            } => match k.code {
                KeyCode::Esc => {}
                KeyCode::Enter => self.submit_input(kind, value),
                KeyCode::Backspace => {
                    value.pop();
                    self.modal = Some(Modal::Input { kind, title, value });
                }
                KeyCode::Char('u') if ctrl => {
                    self.modal = Some(Modal::Input {
                        kind,
                        title,
                        value: String::new(),
                    })
                }
                KeyCode::Char(c) if !ctrl => {
                    value.push(c);
                    self.modal = Some(Modal::Input { kind, title, value });
                }
                _ => self.modal = Some(Modal::Input { kind, title, value }),
            },
            Modal::NewAgent {
                agents,
                mut agent,
                mut name,
                mut prompt,
                mut worktree,
                mut field,
            } => {
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Tab | KeyCode::Down => field = (field + 1) % 4,
                    KeyCode::BackTab | KeyCode::Up => field = (field + 3) % 4,
                    KeyCode::Left if field == 0 => {
                        agent = (agent + agents.len() - 1) % agents.len().max(1)
                    }
                    KeyCode::Right if field == 0 => agent = (agent + 1) % agents.len().max(1),
                    KeyCode::Char(' ') if field == 0 => agent = (agent + 1) % agents.len().max(1),
                    KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right if field == 3 => {
                        worktree = !worktree
                    }
                    KeyCode::Enter => {
                        let a = agents
                            .get(agent)
                            .cloned()
                            .unwrap_or_else(|| "claude".into());
                        let nm = if name.trim().is_empty() {
                            format!("{a}-{}", self.tasks.len() + 1)
                        } else {
                            name.trim().to_string()
                        };
                        let p = Some(prompt.trim().to_string()).filter(|s| !s.is_empty());
                        self.create_task(a, nm, p, worktree);
                        return;
                    }
                    KeyCode::Backspace => {
                        if field == 1 {
                            name.pop();
                        } else if field == 2 {
                            prompt.pop();
                        }
                    }
                    KeyCode::Char('u') if ctrl => {
                        if field == 1 {
                            name.clear()
                        } else if field == 2 {
                            prompt.clear()
                        }
                    }
                    KeyCode::Char(c) if !ctrl => {
                        if field == 1 {
                            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                                name.push(c);
                            } else if c == ' ' {
                                name.push('-');
                            }
                        } else if field == 2 {
                            prompt.push(c);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::NewAgent {
                    agents,
                    agent,
                    name,
                    prompt,
                    worktree,
                    field,
                });
            }
        }
    }

    fn submit_input(&mut self, kind: InputKind, value: String) {
        match kind {
            InputKind::Commit => {
                if value.trim().is_empty() {
                    return self.notify("empty commit message", true);
                }
                self.git_op(|d| git::commit(d, value.trim()).map(|m| format!("committed {m}")));
            }
            InputKind::Find => {
                let mut miss = false;
                if let Some(View::Editor(e)) = self.active_view_mut() {
                    miss = !e.find_next(&value);
                    e.follow = true;
                }
                if miss {
                    self.notify(format!("`{value}` not found"), true);
                }
            }
            InputKind::Goto => {
                if let (Ok(n), Some(View::Editor(e))) =
                    (value.trim().parse::<usize>(), self.active_view_mut())
                {
                    e.goto(n);
                    e.follow = true;
                }
            }
        }
    }

    fn confirm(&mut self, kind: ConfirmKind) {
        match kind {
            ConfirmKind::Merge { root, branch } => match git::merge(&root, &branch) {
                Ok(m) => self.notify(m, false),
                Err(e) => self.notify(format!("{e:#}"), true),
            },
            ConfirmKind::Revert { task, snapshot } => {
                let t2 = task.clone();
                let tx = self.tx.clone();
                let client = self.client.clone();
                tokio::spawn(async move {
                    let r = client
                        .call(Request::Revert {
                            id: task.clone(),
                            snapshot,
                        })
                        .await;
                    let reply = match r {
                        Ok(Response::Snapshot(Some(s))) => Reply::Notice(
                            format!(
                                "{} (#{} — revert it from the timeline to undo)",
                                s.label, s.seq
                            ),
                            false,
                        ),
                        Ok(_) => Reply::Notice("revert done".into(), false),
                        Err(e) => Reply::Notice(format!("{e:#}"), true),
                    };
                    let _ = tx.send(Ev::Reply(reply));
                    if let Ok(Response::Snapshots(list)) =
                        client.call(Request::Snapshots { id: t2.clone() }).await
                    {
                        let _ = tx.send(Ev::Reply(Reply::Snapshots { task: t2, list }));
                    }
                });
                self.request_git();
            }
            ConfirmKind::Remove { id, prune } => {
                self.spawn_call(
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
            ConfirmKind::Discard { dir, change } => match git::discard_file(&dir, &change) {
                Ok(_) => {
                    self.notify(format!("discarded {}", change.path), false);
                    self.git_op(|_| Ok(String::new()));
                }
                Err(e) => self.notify(format!("{e:#}"), true),
            },
            ConfirmKind::CloseDirty { tab } => self.close_tab(tab, true),
        }
    }

    // ── mouse ───────────────────────────────────────────────────────────

    fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.modal.is_some() {
                    return;
                }
                if let Some((_, sec, i)) =
                    self.hits.rows.iter().find(|(r, _, _)| inside(r)).cloned()
                {
                    self.focus = Focus::Sidebar;
                    self.section = sec;
                    match sec {
                        Section::Agents => self.agent_sel = i,
                        Section::Files => self.file_sel = i,
                        Section::Changes => self.change_sel = i,
                    }
                    self.sidebar_activate();
                    return;
                }
                if inside(&self.hits.sidebar) {
                    self.focus = Focus::Sidebar;
                    return;
                }
                if let Some((_, i)) = self.hits.tabs.iter().find(|(r, _)| inside(r)).cloned() {
                    self.select_tab(i);
                    return;
                }
                if let Some((r, p)) = self.hits.panes.iter().find(|(r, _)| inside(r)).cloned() {
                    self.active = p;
                    self.focus = Focus::Pane;
                    self.on_active_changed();
                    let (row, col) = ((y - r.y) as usize, (x - r.x) as usize);
                    match self.active_view_mut() {
                        Some(View::Editor(e)) => {
                            let gutter = format!("{}", e.lines.len()).len() + 2;
                            if col >= gutter {
                                e.click(row, col - gutter);
                            }
                        }
                        Some(View::Diff(d)) => {
                            d.cursor = (d.scroll + row).min(d.parsed.lines.len().saturating_sub(1))
                        }
                        Some(View::Timeline(t)) if row >= 2 => {
                            t.sel = (t.offset + row - 2).min(t.snaps.len().saturating_sub(1));
                        }
                        _ => {}
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                if let Some((_, p)) = self.hits.panes.iter().find(|(r, _)| inside(r)).cloned() {
                    let Some(idx) = self.panes.get(p).copied().flatten() else {
                        return;
                    };
                    match &mut self.tabs[idx] {
                        View::Term(id) => {
                            let id = id.clone();
                            self.scroll_term(&id, if up { 3 } else { -3 });
                        }
                        View::Editor(e) => {
                            e.follow = false;
                            e.scroll(if up { -3 } else { 3 });
                        }
                        View::Diff(d) => {
                            d.follow = false;
                            let max = d.parsed.lines.len().saturating_sub(1);
                            d.scroll = if up {
                                d.scroll.saturating_sub(3)
                            } else {
                                (d.scroll + 3).min(max)
                            };
                        }
                        View::Timeline(t) => {
                            t.sel = if up {
                                t.sel.saturating_sub(1)
                            } else {
                                (t.sel + 1).min(t.snaps.len().saturating_sub(1))
                            };
                        }
                    }
                } else if inside(&self.hits.sidebar) {
                    self.sidebar_move(if up { -1 } else { 1 });
                }
            }
            _ => {}
        }
    }
}
