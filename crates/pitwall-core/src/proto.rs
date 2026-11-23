//! Wire protocol between the daemon (`pitwall daemon`) and its clients
//! (CLI, GUI, Claude Code hooks). Frames are length-prefixed MessagePack.

use serde::{Deserialize, Serialize};

/// How risky a shell command is.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    ReadOnly,
    Mutating,
    Destructive,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::ReadOnly => "read-only",
            Tier::Mutating => "mutating",
            Tier::Destructive => "destructive",
        }
    }
}

/// What the policy says to do with a command.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    /// Output matched a prompt pattern, or the agent reported it needs you.
    Waiting {
        reason: String,
    },
    /// No output for a while / agent finished its turn.
    Idle,
    Exited {
        code: Option<i32>,
    },
}

impl TaskStatus {
    pub fn label(&self) -> String {
        match self {
            TaskStatus::Running => "running".into(),
            TaskStatus::Waiting { .. } => "waiting".into(),
            TaskStatus::Idle => "idle".into(),
            TaskStatus::Exited { code } => match code {
                Some(c) => format!("exited({c})"),
                None => "exited".into(),
            },
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskInfo {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub cwd: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    /// Commit the task's branch started from (for "everything this task changed" diffs).
    #[serde(default)]
    pub base: Option<String>,
    pub status: TaskStatus,
    /// True when the task wants a human (waiting, idle agent, exited, or has pending approvals).
    pub attention: bool,
    pub created_unix: i64,
    pub last_activity_unix: i64,
    pub snapshots: u32,
    pub pending_approvals: u32,
    /// Last non-empty line on screen, for list previews.
    pub preview: String,
    /// Where the terminal is right now: the cwd of the PTY's foreground
    /// process, so it follows `cd`. None from older daemons or once exited.
    #[serde(default)]
    pub live_cwd: Option<String>,
    /// Name of the program in the foreground ("zsh", "claude", "nvim"), with
    /// node/python/bun unwrapped to the script they run. None from older
    /// daemons or once exited.
    #[serde(default)]
    pub fg: Option<String>,
    /// While waiting on a numbered prompt, its choices as (key, label), so
    /// they can be answered without opening the terminal.
    #[serde(default)]
    pub choices: Vec<(String, String)>,
    /// The title the program gave its terminal (OSC 0/2). Claude Code sets it
    /// to what the session is about, as herdr shows under each agent.
    #[serde(default)]
    pub title: Option<String>,
    /// The brief it was handed, if it was started from one.
    #[serde(default)]
    pub brief: Option<String>,
    /// The branch its worktree forked from, which is where Accept merges it.
    #[serde(default)]
    pub base_branch: Option<String>,
    /// It finished a turn and nothing has been submitted since. Unlike an
    /// idle status, a redraw while it sits at its prompt doesn't set this.
    #[serde(default)]
    pub turn_done: bool,
    /// Auto mode: pitwall approves its commands without asking.
    #[serde(default)]
    pub auto: bool,
    /// The SSH host this terminal is connected to, for a session opened
    /// from the host list.
    #[serde(default)]
    pub host: Option<String>,
    /// The worktree its work is in: an agent's cwd, or the one a terminal
    /// lent to an agent started in it.
    #[serde(default)]
    pub worktree: Option<String>,
    /// An agent was running in this terminal when the daemon last stopped,
    /// and stopped with it.
    #[serde(default)]
    pub resume: Option<ResumeOffer>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ResumeOffer {
    /// The program, as typed: "claude", "codex".
    pub agent: String,
    /// Typed into the terminal to start it again where it was.
    pub line: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SnapshotInfo {
    /// Full commit id in the task's shadow repo.
    pub id: String,
    pub seq: u32,
    pub label: String,
    pub unix: i64,
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
}

impl SnapshotInfo {
    pub fn short(&self) -> &str {
        &self.id[..self.id.len().min(8)]
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ApprovalSource {
    /// A Claude Code PreToolUse hook inside a task.
    Task { task_id: String, tool: String },
    /// An ops investigation wants to run something on hosts.
    Ops { session: u64, hosts: Vec<String> },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Approval {
    pub id: u64,
    pub source: ApprovalSource,
    pub command: String,
    pub tier: Tier,
    pub reason: String,
    pub created_unix: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HostInfo {
    pub name: String,
    pub addr: String,
    pub user: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HostResult {
    pub host: String,
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub ms: u64,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NewTask {
    pub name: String,
    /// Key into `[agents]` in config (claude, codex, opencode, shell, ...).
    pub agent: String,
    /// Git repo to branch a worktree from. If None, runs in `cwd` directly.
    pub repo: Option<String>,
    /// Working directory when not using a worktree.
    pub cwd: Option<String>,
    /// Base ref for the worktree branch (default HEAD).
    pub base: Option<String>,
    /// Run in the repo itself instead of a fresh worktree.
    pub no_worktree: bool,
    pub args: Vec<String>,
    /// Initial prompt for the agent.
    pub prompt: Option<String>,
    pub rows: u16,
    pub cols: u16,
    /// The brief this agent is building, so the two stay linked.
    #[serde(default)]
    pub brief: Option<String>,
    /// Run `ssh` with `args` instead of the agent's command: an interactive
    /// session to this host. Only with the shell agent.
    #[serde(default)]
    pub host: Option<String>,
    /// The worktree's branch. None picks one from the name.
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HookRequest {
    /// From `PITWALL_TASK_ID` in the hook's environment.
    pub task_id: Option<String>,
    /// Raw hook JSON from Claude Code's stdin.
    pub payload: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HookReply {
    /// None = no opinion, let the agent's normal permission flow run.
    pub decision: Option<Decision>,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum OpsEvent {
    Started {
        session: u64,
        question: String,
    },
    Thinking {
        session: u64,
        text: String,
    },
    Command {
        session: u64,
        hosts: Vec<String>,
        command: String,
        tier: Tier,
        decision: Decision,
        why: String,
    },
    Results {
        session: u64,
        command: String,
        results: Vec<HostResult>,
    },
    Blocked {
        session: u64,
        command: String,
        reason: String,
    },
    Finished {
        session: u64,
        answer: String,
        runbook: Option<String>,
    },
    Failed {
        session: u64,
        error: String,
    },
}

impl OpsEvent {
    pub fn session(&self) -> u64 {
        match self {
            OpsEvent::Started { session, .. }
            | OpsEvent::Thinking { session, .. }
            | OpsEvent::Command { session, .. }
            | OpsEvent::Results { session, .. }
            | OpsEvent::Blocked { session, .. }
            | OpsEvent::Finished { session, .. }
            | OpsEvent::Failed { session, .. } => *session,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Request {
    Ping,
    Shutdown,
    /// Start receiving `Push` events (task updates, approvals, ops).
    Subscribe,

    ListTasks,
    NewTask(NewTask),
    Kill {
        id: String,
    },
    Remove {
        id: String,
        prune_worktree: bool,
    },
    /// A terminal's own worktree for an agent about to start in it, made
    /// from the repo `dir` is in. The one it already has comes back again.
    /// Answers `Text` with the worktree's path.
    LendWorktree {
        id: String,
        dir: String,
    },
    /// This connection is a window that can ask you what to call a
    /// terminal's worktree branch: `AskWorktreeName` comes to it.
    NameWorktrees,
    /// The answer to `AskWorktreeName`. None runs the agent in the checkout.
    WorktreeName {
        id: String,
        name: Option<String>,
    },
    /// Remove a terminal's worktree, and its branch once merged.
    ReturnWorktree {
        id: String,
        /// Quit the agent still running in it first, instead of refusing.
        #[serde(default)]
        quit: bool,
    },

    Attach {
        id: String,
        rows: u16,
        cols: u16,
        /// Send a fresh screen after the daemon reflows the terminal for a
        /// new width. A window drawing its own grid wants it; a real terminal
        /// (`pitwall attach`) reflows by itself.
        #[serde(default)]
        repaints: bool,
    },
    Detach {
        id: String,
    },
    Input {
        id: String,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    Resize {
        id: String,
        rows: u16,
        cols: u16,
    },
    /// Type a message into an agent's prompt and submit it. `from` is the
    /// agent sending it, when it's an agent.
    Prompt {
        id: String,
        text: String,
        from: Option<String>,
    },
    /// The steps of the brief a task was handed, ticking or unticking
    /// `step` (from 1) first when given. Answers with the list.
    Brief {
        id: String,
        step: Option<usize>,
        done: bool,
    },
    /// The last `lines` non-blank lines of a task's screen.
    Read {
        id: String,
        lines: usize,
    },

    Snapshots {
        id: String,
    },
    TakeSnapshot {
        id: String,
        label: String,
    },
    Revert {
        id: String,
        snapshot: String,
    },
    /// Diff between two snapshots. `from` defaults to the first snapshot,
    /// `to` to the current working tree. `"<id>^"` means "parent of".
    Diff {
        id: String,
        from: Option<String>,
        to: Option<String>,
    },

    Approvals,
    Decide {
        id: u64,
        allow: bool,
    },
    /// Turn a task's auto mode on or off. On approves what it's waiting on too.
    SetAuto {
        id: String,
        on: bool,
    },
    /// Drop a terminal's offer to resume the agent it had.
    DismissResume {
        id: String,
    },

    Hook(HookRequest),
    Classify {
        command: String,
    },

    Hosts,
    /// Run a command across hosts. `confirmed` = a human already approved it.
    FleetRun {
        selector: String,
        command: String,
        confirmed: bool,
    },
    OpsAsk {
        question: String,
        selector: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Response {
    Pong {
        version: String,
    },
    Ok,
    Error(String),
    Tasks(Vec<TaskInfo>),
    Task(TaskInfo),
    Snapshots(Vec<SnapshotInfo>),
    Snapshot(Option<SnapshotInfo>),
    Diff(String),
    Text(String),
    Approvals(Vec<Approval>),
    Hook(HookReply),
    Classified {
        tier: Tier,
        decision: Decision,
        reason: String,
    },
    Hosts(Vec<HostInfo>),
    Fleet(Vec<HostResult>),
    NeedsConfirm {
        tier: Tier,
        decision: Decision,
        reason: String,
        hosts: Vec<String>,
    },
    OpsStarted {
        session: u64,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Push {
    /// Full screen repaint for an attached task (sent on attach / after lag).
    Screen {
        id: String,
        rows: u16,
        cols: u16,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// Raw PTY output for an attached task.
    Output {
        id: String,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    TaskUpdated(TaskInfo),
    TaskRemoved {
        id: String,
    },
    ApprovalAdded(Approval),
    ApprovalResolved {
        id: u64,
        allow: bool,
    },
    SnapshotTaken {
        task: String,
        snapshot: SnapshotInfo,
    },
    Ops(OpsEvent),
    /// One agent (or `pitwall tell` from a shell) messaged another.
    Message {
        from: Option<String>,
        to: String,
        text: String,
    },
    /// An agent is starting in terminal `id` and its worktree needs a
    /// branch. Only sent to connections that asked with `NameWorktrees`.
    AskWorktreeName {
        id: String,
        repo: String,
        suggest: String,
    },
    /// Answered, from this window or another, or given up on.
    WorktreeNamed {
        id: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientFrame {
    pub req: u64,
    pub body: Request,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ServerFrame {
    Reply { req: u64, body: Response },
    Push(Push),
}
