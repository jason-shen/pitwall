//! The pitwall daemon: owns agent PTYs, snapshots, approvals and fleet sessions.
//! Clients (CLI, GUI, hooks) talk to it over a Unix socket.

pub mod audit;
pub mod fleet;
pub mod hook;
pub mod ops;
pub mod redact;
pub mod server;
pub mod snapshot;
pub mod task;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use pitwall_core::config::Config;
use pitwall_core::proto::{
    Approval, ApprovalSource, NewTask, Push, SnapshotInfo, TaskInfo, TaskStatus, Tier,
};
use pitwall_core::{now_unix, paths};
use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use tokio::sync::{broadcast, mpsc, oneshot};

use task::Task;

struct PendingApproval {
    info: Approval,
    tx: Option<oneshot::Sender<bool>>,
}

pub struct Daemon {
    pub cfg: Config,
    pub tasks: Mutex<BTreeMap<String, Arc<Task>>>,
    approvals: Mutex<BTreeMap<u64, PendingApproval>>,
    next_approval: AtomicU64,
    pub next_ops: AtomicU64,
    pub events: broadcast::Sender<Push>,
    pub audit: audit::Audit,
    watchers: Mutex<HashMap<String, Debouncer<RecommendedWatcher>>>,
    snap_tx: mpsc::UnboundedSender<String>,
    /// Ids handed out to tasks still starting, so two at once can't get the same one.
    starting: Mutex<std::collections::HashSet<String>>,
    /// Saving waits for restore, or a task started meanwhile would write a
    /// list that's missing the ones not back yet.
    restoring: std::sync::atomic::AtomicBool,
    /// Saved tasks that couldn't start again (folder gone, agent missing).
    /// Kept in tasks.json so one bad start doesn't forget them.
    unrestored: Mutex<Vec<task::Saved>>,
}

impl Daemon {
    fn new(cfg: Config, snap_tx: mpsc::UnboundedSender<String>) -> Daemon {
        let (events, _) = broadcast::channel(4096);
        Daemon {
            cfg,
            tasks: Mutex::new(BTreeMap::new()),
            approvals: Mutex::new(BTreeMap::new()),
            next_approval: AtomicU64::new(1),
            next_ops: AtomicU64::new(1),
            events,
            audit: audit::Audit::open(&paths::audit_log()),
            watchers: Mutex::new(HashMap::new()),
            snap_tx,
            starting: Default::default(),
            restoring: std::sync::atomic::AtomicBool::new(true),
            unrestored: Mutex::new(vec![]),
        }
    }

    pub fn emit(&self, p: Push) {
        let _ = self.events.send(p);
    }

    pub fn emit_task(&self, t: &Task) {
        self.emit(Push::TaskUpdated(t.info()));
    }

    /// A task's place changed: tell the windows, and keep it for a restart.
    pub fn saved_changed(&self, t: &Task) {
        self.emit_task(t);
        self.save();
    }

    /// Look up by exact id, then by unique prefix.
    pub fn task(&self, id: &str) -> Result<Arc<Task>> {
        let tasks = self.tasks.lock().unwrap();
        if let Some(t) = tasks.get(id) {
            return Ok(t.clone());
        }
        // `fix` that wasn't restored mustn't turn into the live `fix-2`.
        if self.unrestored.lock().unwrap().iter().any(|s| s.id == id) {
            return Err(anyhow!(
                "{id} wasn't restored (its folder or agent is missing)"
            ));
        }
        let matches: Vec<_> = tasks.iter().filter(|(k, _)| k.starts_with(id)).collect();
        match matches.len() {
            1 => Ok(matches[0].1.clone()),
            0 => Err(anyhow!("no task `{id}`")),
            _ => Err(anyhow!("`{id}` is ambiguous")),
        }
    }

    pub fn list(&self) -> Vec<TaskInfo> {
        let tasks: Vec<_> = self.tasks.lock().unwrap().values().cloned().collect();
        let mut out: Vec<TaskInfo> = tasks.iter().map(|t| t.info()).collect();
        // Listed so they can be seen and removed, not silently carried along.
        out.extend(
            self.unrestored
                .lock()
                .unwrap()
                .iter()
                .map(task::Saved::info),
        );
        out
    }

    pub async fn new_task(self: &Arc<Self>, req: NewTask) -> Result<TaskInfo> {
        if req.name.trim().is_empty() {
            bail!("task name is required");
        }
        let id = {
            let tasks = self.tasks.lock().unwrap();
            let mut starting = self.starting.lock().unwrap();
            let kept: std::collections::HashSet<String> = self
                .unrestored
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.id.clone())
                .collect();
            let base = task::slug(&req.name);
            let mut id = base.clone();
            let mut n = 2;
            while tasks.contains_key(&id)
                || starting.contains(&id)
                || kept.contains(&id)
                || paths::snapshots().join(format!("{id}.git")).exists()
            {
                id = format!("{base}-{n}");
                n += 1;
            }
            starting.insert(id.clone());
            id
        };
        let cfg = self.cfg.clone();
        let id2 = id.clone();
        let spawned = tokio::task::spawn_blocking(move || Task::spawn(id2, &req, &cfg)).await;
        let t = match spawned {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                self.starting.lock().unwrap().remove(&id);
                return Err(e);
            }
            Err(e) => {
                self.starting.lock().unwrap().remove(&id);
                return Err(e.into());
            }
        };
        self.tasks.lock().unwrap().insert(id.clone(), t.clone());
        self.starting.lock().unwrap().remove(&id);
        self.save();
        if self.cfg.snapshots.auto && t.snaps.is_some() {
            if let Err(e) = self.watch(&t) {
                tracing::warn!("file watcher for {id} failed: {e:#}");
            }
        }
        self.audit.log(serde_json::json!({
            "source": "task", "event": "started", "task": id, "agent": t.agent, "cwd": t.cwd, "branch": t.branch,
        }));
        let info = t.info();
        self.emit(Push::TaskUpdated(info.clone()));
        Ok(info)
    }

    fn watch(&self, t: &Arc<Task>) -> Result<()> {
        let id = t.id.clone();
        let tx = self.snap_tx.clone();
        let root = t.cwd.clone();
        let skip: Vec<String> = self
            .cfg
            .snapshots
            .exclude
            .iter()
            .map(|e| e.trim_end_matches('/').to_string())
            .collect();
        let mut deb = new_debouncer(
            Duration::from_millis(self.cfg.snapshots.debounce_ms.max(200)),
            move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                let relevant = events.iter().any(|e| {
                    let rel = e.path.strip_prefix(&root).unwrap_or(&e.path);
                    !rel.components().any(|c| {
                        let s = c.as_os_str().to_string_lossy();
                        s == ".git" || skip.iter().any(|k| *k == s)
                    })
                });
                if relevant {
                    let _ = tx.send(id.clone());
                }
            },
        )?;
        deb.watcher().watch(&t.cwd, RecursiveMode::Recursive)?;
        self.watchers.lock().unwrap().insert(t.id.clone(), deb);
        Ok(())
    }

    /// Take a snapshot off the async runtime and broadcast it.
    pub async fn snapshot(self: &Arc<Self>, t: &Arc<Task>, label: String) -> Option<SnapshotInfo> {
        t.snaps.as_ref()?;
        let t2 = t.clone();
        let res =
            tokio::task::spawn_blocking(move || t2.snaps.as_ref().unwrap().take(&label)).await;
        match res {
            Ok(Ok(Some(s))) => {
                t.state.lock().unwrap().snap_count = s.seq;
                self.emit(Push::SnapshotTaken {
                    task: t.id.clone(),
                    snapshot: s.clone(),
                });
                self.emit_task(t);
                Some(s)
            }
            Ok(Ok(None)) => None,
            Ok(Err(e)) => {
                tracing::warn!("snapshot for {} failed: {e:#}", t.id);
                None
            }
            Err(e) => {
                tracing::warn!("snapshot task panicked: {e}");
                None
            }
        }
    }

    pub fn set_status(&self, t: &Task, status: TaskStatus) {
        let (changed, first_turn) = {
            let mut st = t.state.lock().unwrap();
            if st.status != status && !matches!(st.status, TaskStatus::Exited { .. }) {
                // Quiet after a prompt or after you typed: a turn ended.
                // Quiet from the start is just an agent at its prompt.
                // An agent with hooks says itself when a turn ends (Stop);
                // for those, only that counts.
                let first_turn = status == TaskStatus::Idle
                    && !st.had_turn
                    && t.agent_cfg.hooks.is_none()
                    && (t.prompt.is_some() || st.had_input);
                st.had_turn |= first_turn;
                st.status = status;
                st.status_since = Instant::now();
                (true, first_turn)
            } else {
                (false, false)
            }
        };
        if changed {
            self.emit_task(t);
        }
        // From now on a restart resumes its session instead of starting over.
        if first_turn {
            self.save();
        }
    }

    /// Claude's Stop hook: a turn really did end, whatever started it.
    pub fn turn_ended(&self, t: &Task) {
        let first = {
            let mut st = t.state.lock().unwrap();
            let first = !st.had_turn;
            st.had_turn = true;
            // Ended even if pitwall didn't see what started it.
            st.turn_open = true;
            st.close_turn();
            first
        };
        if first {
            self.save();
        }
    }

    /// Put a command in the inbox and wait for a human. None = timed out.
    pub async fn request_approval(
        self: &Arc<Self>,
        source: ApprovalSource,
        command: String,
        tier: Tier,
        reason: String,
        timeout: Duration,
    ) -> Option<bool> {
        let id = self.next_approval.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let info = Approval {
            id,
            source: source.clone(),
            command,
            tier,
            reason,
            created_unix: now_unix(),
        };
        self.approvals.lock().unwrap().insert(
            id,
            PendingApproval {
                info: info.clone(),
                tx: Some(tx),
            },
        );
        let task = match &source {
            ApprovalSource::Task { task_id, .. } => self.task(task_id).ok(),
            _ => None,
        };
        if let Some(t) = &task {
            t.state.lock().unwrap().pending_approvals += 1;
            self.set_status(
                t,
                TaskStatus::Waiting {
                    reason: format!("approve: {}", info.command),
                },
            );
            self.emit_task(t);
        }
        self.emit(Push::ApprovalAdded(info));

        let result = tokio::time::timeout(timeout, rx)
            .await
            .ok()
            .and_then(|r| r.ok());

        self.approvals.lock().unwrap().remove(&id);
        if let Some(t) = &task {
            {
                let mut st = t.state.lock().unwrap();
                st.pending_approvals = st.pending_approvals.saturating_sub(1);
            }
            self.set_status(t, TaskStatus::Running);
            self.emit_task(t);
        }
        self.emit(Push::ApprovalResolved {
            id,
            allow: result.unwrap_or(false),
        });
        result
    }

    pub fn decide(&self, id: u64, allow: bool) -> Result<()> {
        let tx = self
            .approvals
            .lock()
            .unwrap()
            .get_mut(&id)
            .and_then(|p| p.tx.take())
            .ok_or_else(|| anyhow!("no pending approval #{id}"))?;
        let _ = tx.send(allow);
        Ok(())
    }

    /// Turning it on also lets through whatever the task is waiting on, so
    /// the "auto mode" answer to an approval approves that one as well.
    pub fn set_auto(&self, id: &str, on: bool) -> Result<()> {
        let t = self.task(id)?;
        t.auto.store(on, Ordering::Relaxed);
        if on {
            let waiting: Vec<u64> = self
                .approvals
                .lock()
                .unwrap()
                .values()
                .filter(|p| {
                    matches!(&p.info.source, ApprovalSource::Task { task_id, .. } if task_id == &t.id)
                })
                .map(|p| p.info.id)
                .collect();
            for a in waiting {
                let _ = self.decide(a, true);
            }
        }
        self.save();
        self.emit_task(&t);
        Ok(())
    }

    pub fn approvals(&self) -> Vec<Approval> {
        self.approvals
            .lock()
            .unwrap()
            .values()
            .map(|p| p.info.clone())
            .collect()
    }

    pub async fn remove_task(self: &Arc<Self>, id: &str, prune: bool) -> Result<()> {
        let kept_exactly = self.unrestored.lock().unwrap().iter().any(|s| s.id == id);
        let found = if kept_exactly {
            Err(anyhow!("not live"))
        } else {
            self.task(id)
        };
        let (id, place, lent) = match found {
            Ok(t) => {
                t.kill();
                self.watchers.lock().unwrap().remove(&t.id);
                self.tasks.lock().unwrap().remove(&t.id);
                let s = t.saved();
                (s.id, s.place, s.lent)
            }
            // One that couldn't be restored: there's no process, just the entry.
            Err(e) => {
                let mut kept = self.unrestored.lock().unwrap();
                let Some(i) = kept.iter().position(|s| s.id == id) else {
                    return Err(e);
                };
                let s = kept.remove(i);
                (s.id, s.place, s.lent)
            }
        };
        self.save();
        let mut problem = None;
        if prune {
            // A terminal's lent worktree goes the way an agent's own does.
            let place = lent.unwrap_or(place);
            if let (Some(repo), Some(branch)) = (place.repo, place.branch) {
                let cwd = place.cwd;
                let r =
                    tokio::task::spawn_blocking(move || task::remove_branch(&repo, &cwd, &branch))
                        .await;
                // The task is gone either way; say what's left behind instead
                // of stopping here with it half removed.
                match r {
                    Ok(Err(e)) => problem = Some(format!("{e:#}")),
                    Err(e) => problem = Some(e.to_string()),
                    Ok(Ok(())) => {}
                }
            }
            // Nothing can revert to these once the task is gone, and leaving
            // them makes the next task with this name `<name>-2`.
            let store = paths::snapshots().join(format!("{id}.git"));
            if let Err(e) = std::fs::remove_dir_all(&store) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("couldn't remove {}: {e}", store.display());
                }
            }
        }
        self.audit.log(
            serde_json::json!({"source": "task", "event": "removed", "task": id, "pruned": prune}),
        );
        self.emit(Push::TaskRemoved { id: id.clone() });
        match problem {
            Some(p) => bail!("removed {id}, but its worktree is still there: {p}"),
            None => Ok(()),
        }
    }

    /// Write the task list for the next daemon to bring back. Only on start
    /// and remove: at shutdown the agents may already be dying, and a task
    /// that exited on its own is still listed until it's removed.
    fn save(&self) {
        if self.restoring.load(Ordering::SeqCst) {
            return;
        }
        // Removing several tasks at once saves from several threads. They
        // share the temp file, so without this one rename fails and the
        // list left on disk can be from before the last removal.
        static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = SAVING.lock().unwrap_or_else(|e| e.into_inner());
        let mut saved: Vec<task::Saved> = self
            .tasks
            .lock()
            .unwrap()
            .values()
            .map(|t| t.saved())
            .collect();
        saved.extend(self.unrestored.lock().unwrap().iter().cloned());
        let path = paths::home().join("tasks.json");
        let tmp = path.with_extension("json.tmp");
        let res = serde_json::to_vec_pretty(&saved)
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(std::fs::write(&tmp, b)?))
            .and_then(|_| Ok(std::fs::rename(&tmp, &path)?));
        if let Err(e) = res {
            tracing::warn!("couldn't save tasks: {e:#}");
        }
    }

    /// The tasks the last daemon had, with their ids held back so nothing
    /// started meanwhile can take one. Called before the socket opens.
    fn load_saved(&self) -> Vec<task::Saved> {
        let path = paths::home().join("tasks.json");
        let saved: Vec<task::Saved> = match std::fs::read(&path) {
            Err(_) => vec![],
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("ignoring {}: {e}", path.display());
                vec![]
            }),
        };
        let mut starting = self.starting.lock().unwrap();
        starting.extend(saved.iter().map(|s| s.id.clone()));
        saved
    }

    /// Start again the tasks the last daemon had, each in its own folder.
    async fn restore(self: &Arc<Self>, saved: Vec<task::Saved>) {
        for s in saved {
            let id = s.id.clone();
            if !s.place.cwd.is_dir() {
                tracing::warn!(
                    "not restoring {id} for now: {} is gone",
                    s.place.cwd.display()
                );
                self.unrestored.lock().unwrap().push(s);
                self.starting.lock().unwrap().remove(&id);
                continue;
            }
            let cfg = self.cfg.clone();
            let keep = s.clone();
            match tokio::task::spawn_blocking(move || Task::resume(s, &cfg)).await {
                Ok(Ok(t)) => {
                    if self.cfg.snapshots.auto && t.snaps.is_some() {
                        if let Err(e) = self.watch(&t) {
                            tracing::warn!("file watcher for {id} failed: {e:#}");
                        }
                    }
                    tracing::info!("restored {id}");
                    self.tasks.lock().unwrap().insert(id.clone(), t.clone());
                    self.emit_task(&t);
                }
                Ok(Err(e)) => {
                    tracing::warn!("couldn't restore {id}, keeping it for next time: {e:#}");
                    self.unrestored.lock().unwrap().push(keep);
                }
                Err(e) => {
                    tracing::warn!("couldn't restore {id}, keeping it for next time: {e}");
                    self.unrestored.lock().unwrap().push(keep);
                }
            }
            self.starting.lock().unwrap().remove(&id);
        }
        self.restoring.store(false, Ordering::SeqCst);
        self.save();
    }

    fn shutdown(&self) {
        for t in self.tasks.lock().unwrap().values() {
            t.kill();
        }
        let _ = std::fs::remove_file(paths::socket());
    }
}

/// Watch for idle/prompt/exit transitions once a second.
async fn ticker(d: Arc<Daemon>) {
    // What was last pushed per task: status, preview, live cwd, foreground program.
    type Seen = (
        TaskStatus,
        String,
        Option<String>,
        Option<String>,
        Vec<(String, String)>,
        Option<String>,
    );
    let mut reported: HashMap<String, Seen> = HashMap::new();
    let mut iv = tokio::time::interval(Duration::from_secs(1));
    let idle = Duration::from_secs(d.cfg.attention.idle_secs.max(1));
    loop {
        iv.tick().await;
        let tasks: Vec<Arc<Task>> = d.tasks.lock().unwrap().values().cloned().collect();
        reported.retain(|k, _| tasks.iter().any(|t| &t.id == k));
        for t in tasks {
            let (status, since_output, since_status, pending, turn_open) = {
                let st = t.state.lock().unwrap();
                (
                    st.status.clone(),
                    st.last_output.elapsed(),
                    st.status_since.elapsed(),
                    st.pending_approvals,
                    st.turn_open,
                )
            };
            // An agent at its prompt redraws now and then (a status line, a
            // resize). That's not work unless something started a turn.
            let redraw_only = t.agent != "shell" && !turn_open;
            let next = match &status {
                TaskStatus::Exited { .. } => None,
                TaskStatus::Running if since_output >= idle => {
                    let tail = t.tail_text(6);
                    let hit = tail.iter().rev().find(|line| {
                        d.cfg
                            .attention
                            .patterns
                            .iter()
                            .any(|p| line.contains(p.as_str()))
                    });
                    Some(match hit {
                        Some(line) => TaskStatus::Waiting {
                            reason: line.trim().chars().take(160).collect(),
                        },
                        None => TaskStatus::Idle,
                    })
                }
                // Fresh output after we flagged it means it's working again.
                TaskStatus::Waiting { .. } | TaskStatus::Idle
                    if pending == 0
                        && !redraw_only
                        && since_output + Duration::from_millis(300) < since_status
                        && since_output < idle =>
                {
                    Some(TaskStatus::Running)
                }
                _ => None,
            };
            if let Some(n) = next {
                let turn_end =
                    matches!(status, TaskStatus::Running) && !matches!(n, TaskStatus::Running);
                // With hooks, only Stop ends a turn: an agent can go quiet
                // mid-turn (a long tool call) and still be at it.
                if turn_end && t.agent_cfg.hooks.is_none() {
                    t.state.lock().unwrap().close_turn();
                }
                d.set_status(&t, n);
                if turn_end && d.cfg.snapshots.auto {
                    d.snapshot(&t, "turn ended".into()).await;
                }
            }

            let info = t.info();
            let prev = reported.get(&t.id);
            if prev
                .map(|(s, p, c, f, o, ti)| {
                    s != &info.status
                        || p != &info.preview
                        || c != &info.live_cwd
                        || f != &info.fg
                        || o != &info.choices
                        || ti != &info.title
                })
                .unwrap_or(true)
            {
                if matches!(info.status, TaskStatus::Exited { .. })
                    && !matches!(prev.map(|p| &p.0), Some(TaskStatus::Exited { .. }))
                {
                    d.audit.log(serde_json::json!({"source": "task", "event": "exited", "task": t.id, "status": info.status.label()}));
                    if t.snaps.is_some() {
                        d.snapshot(&t, "task exited".into()).await;
                    }
                }
                reported.insert(
                    t.id.clone(),
                    (
                        info.status.clone(),
                        info.preview.clone(),
                        info.live_cwd.clone(),
                        info.fg.clone(),
                        info.choices.clone(),
                        info.title.clone(),
                    ),
                );
                d.emit(Push::TaskUpdated(info));
            }
        }
    }
}

/// Debounced file-change snapshots.
async fn snapshot_worker(d: Arc<Daemon>, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(id) = rx.recv().await {
        // Coalesce bursts.
        let mut ids = vec![id];
        while let Ok(more) = rx.try_recv() {
            if !ids.contains(&more) {
                ids.push(more);
            }
        }
        for id in ids {
            if let Ok(t) = d.task(&id) {
                d.snapshot(&t, "files changed".into()).await;
            }
        }
    }
}

async fn socket_alive(p: &Path) -> bool {
    tokio::net::UnixStream::connect(p).await.is_ok()
}

/// One script per agent CLI, rewritten at every start since the path to
/// this binary changes when the app is updated or moved.
fn write_shims() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = paths::shims();
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    // Single quotes, so a space or $ in the app's path stays literal.
    let exe = format!("'{}'", exe.display().to_string().replace('\'', "'\\''"));
    for name in pitwall_core::agents::AGENT_PROGRAMS {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nexec {exe} wrap {name} \"$@\"\n"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    let init = paths::shell_init();
    std::fs::create_dir_all(init.join("zsh"))?;
    std::fs::write(init.join("zsh/.zshenv"), ZSHENV)?;
    std::fs::write(init.join("bashrc"), BASHRC)?;
    Ok(())
}

// A ~/.zshrc that puts ~/.local/bin first (where Claude Code installs
// itself) would hide the shims, so they go back in front before each
// prompt. zsh reads each startup file from ZDOTDIR as it gets to it, so
// putting yours back here means the rest load from where they always do.
const ZSHENV: &str = r#"# Written by pitwall for its terminals.
if [[ -n "$PITWALL_USER_ZDOTDIR" ]]; then ZDOTDIR="$PITWALL_USER_ZDOTDIR"; else unset ZDOTDIR; fi
unset PITWALL_USER_ZDOTDIR
[[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
_pitwall_shims() { [[ -n "$PITWALL_SHIMS" ]] && path=("$PITWALL_SHIMS" ${path:#$PITWALL_SHIMS}) }
typeset -ga precmd_functions
precmd_functions+=(_pitwall_shims)
"#;

const BASHRC: &str = r#"# Written by pitwall for its terminals.
[ -f ~/.bashrc ] && . ~/.bashrc
_pitwall_shims() {
  [ -n "$PITWALL_SHIMS" ] || return
  case ":$PATH:" in ":$PITWALL_SHIMS:"*) ;; *) PATH="$PITWALL_SHIMS:${PATH//$PITWALL_SHIMS:/}" ;; esac
}
PROMPT_COMMAND="_pitwall_shims${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
"#;

pub async fn run() -> Result<()> {
    std::fs::create_dir_all(paths::home())?;
    let cfg = Config::load()?;
    let sock = paths::socket();
    if sock.exists() {
        if socket_alive(&sock).await {
            bail!("pitwall daemon already running at {}", sock.display());
        }
        let _ = std::fs::remove_file(&sock);
    }

    if let Err(e) = write_shims() {
        tracing::warn!("agents started in terminals won't get worktrees: {e:#}");
    }
    let (snap_tx, snap_rx) = mpsc::unbounded_channel();
    let d = Arc::new(Daemon::new(cfg, snap_tx));
    tracing::info!(
        "pitwall {} listening on {}",
        pitwall_core::VERSION,
        sock.display()
    );

    tokio::spawn(ticker(d.clone()));
    {
        let d = d.clone();
        let saved = d.load_saved();
        tokio::spawn(async move { d.restore(saved).await });
    }
    tokio::spawn(snapshot_worker(d.clone(), snap_rx));

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let res = tokio::select! {
        r = server::serve(d.clone(), &sock) => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = term.recv() => Ok(()),
    };
    tracing::info!("shutting down");
    d.shutdown();
    res
}
