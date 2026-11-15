//! The muster daemon: owns agent PTYs, snapshots, approvals and fleet sessions.
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
use muster_core::config::Config;
use muster_core::proto::{
    Approval, ApprovalSource, NewTask, Push, SnapshotInfo, TaskInfo, TaskStatus, Tier,
};
use muster_core::{now_unix, paths};
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
        }
    }

    pub fn emit(&self, p: Push) {
        let _ = self.events.send(p);
    }

    pub fn emit_task(&self, t: &Task) {
        self.emit(Push::TaskUpdated(t.info()));
    }

    /// Look up by exact id, then by unique prefix.
    pub fn task(&self, id: &str) -> Result<Arc<Task>> {
        let tasks = self.tasks.lock().unwrap();
        if let Some(t) = tasks.get(id) {
            return Ok(t.clone());
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
        tasks.iter().map(|t| t.info()).collect()
    }

    pub async fn new_task(self: &Arc<Self>, req: NewTask) -> Result<TaskInfo> {
        if req.name.trim().is_empty() {
            bail!("task name is required");
        }
        let id = {
            let tasks = self.tasks.lock().unwrap();
            let base = task::slug(&req.name);
            let mut id = base.clone();
            let mut n = 2;
            while tasks.contains_key(&id) || paths::snapshots().join(format!("{id}.git")).exists() {
                id = format!("{base}-{n}");
                n += 1;
            }
            id
        };
        let cfg = self.cfg.clone();
        let id2 = id.clone();
        let t = tokio::task::spawn_blocking(move || Task::spawn(id2, &req, &cfg)).await??;
        self.tasks.lock().unwrap().insert(id.clone(), t.clone());
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
        let changed = {
            let mut st = t.state.lock().unwrap();
            if st.status != status && !matches!(st.status, TaskStatus::Exited { .. }) {
                st.status = status;
                st.status_since = Instant::now();
                true
            } else {
                false
            }
        };
        if changed {
            self.emit_task(t);
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

    pub fn approvals(&self) -> Vec<Approval> {
        self.approvals
            .lock()
            .unwrap()
            .values()
            .map(|p| p.info.clone())
            .collect()
    }

    pub async fn remove_task(self: &Arc<Self>, id: &str, prune: bool) -> Result<()> {
        let t = self.task(id)?;
        t.kill();
        self.watchers.lock().unwrap().remove(&t.id);
        self.tasks.lock().unwrap().remove(&t.id);
        if prune {
            if let Some(repo) = &t.repo {
                if t.branch.is_some() {
                    let (repo, cwd) = (repo.clone(), t.cwd.clone());
                    tokio::task::spawn_blocking(move || task::remove_worktree(&repo, &cwd))
                        .await??;
                }
            }
        }
        self.audit.log(serde_json::json!({"source": "task", "event": "removed", "task": t.id, "pruned": prune}));
        self.emit(Push::TaskRemoved { id: t.id.clone() });
        Ok(())
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
            let (status, since_output, since_status, pending) = {
                let st = t.state.lock().unwrap();
                (
                    st.status.clone(),
                    st.last_output.elapsed(),
                    st.status_since.elapsed(),
                    st.pending_approvals,
                )
            };
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

pub async fn run() -> Result<()> {
    std::fs::create_dir_all(paths::home())?;
    let cfg = Config::load()?;
    let sock = paths::socket();
    if sock.exists() {
        if socket_alive(&sock).await {
            bail!("muster daemon already running at {}", sock.display());
        }
        let _ = std::fs::remove_file(&sock);
    }

    let (snap_tx, snap_rx) = mpsc::unbounded_channel();
    let d = Arc::new(Daemon::new(cfg, snap_tx));
    tracing::info!(
        "muster {} listening on {}",
        muster_core::VERSION,
        sock.display()
    );

    tokio::spawn(ticker(d.clone()));
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
