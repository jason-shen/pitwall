//! An agent task: a PTY running an agent CLI inside an (optionally isolated) worktree,
//! with a server-side vt100 screen so any client can attach and get a full repaint.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use pitwall_core::config::{AgentConfig, Config};
use pitwall_core::proto::{NewTask, ResumeOffer, TaskInfo, TaskStatus};
use pitwall_core::{now_unix, paths};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::snapshot::Store;

/// Keeps the title a program gives its terminal (vt100's default callbacks
/// drop it), and answers the terminal's queries. The daemon answers them,
/// not a window: a program asking where its cursor is (Codex does at start)
/// gets one answer whether or not anything is showing it.
#[derive(Default)]
pub struct Titles {
    pub title: Option<String>,
    pub replies: Vec<u8>,
    pub keys: pitwall_core::kitty::Keys,
}

impl vt100::Callbacks for Titles {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
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
            ('n', None) if first == 6 => {
                let (r, col) = screen.cursor_position();
                self.replies
                    .extend_from_slice(format!("\x1b[{};{}R", r + 1, col + 1).as_bytes());
            }
            ('n', None) if first == 5 => self.replies.extend_from_slice(b"\x1b[0n"),
            ('c', None) if first == 0 => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            ('c', Some(b'>')) => self.replies.extend_from_slice(b"\x1b[>0;10;1c"),
            ('u', marker) => {
                if let Some(r) = self.keys.csi(marker, params) {
                    self.replies.extend_from_slice(&r);
                }
            }
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        // Foreground and background colour queries, which programs use to
        // pick a light or dark palette. pitwall's themes are dark.
        match params {
            [b"10", b"?"] => self
                .replies
                .extend_from_slice(b"\x1b]10;rgb:d4d4/d7d7/dddd\x1b\\"),
            [b"11", b"?"] => self
                .replies
                .extend_from_slice(b"\x1b]11;rgb:1414/1616/1a1a\x1b\\"),
            _ => {}
        }
    }
}

pub struct TaskState {
    pub status: TaskStatus,
    pub last_output: Instant,
    pub last_activity_unix: i64,
    pub pending_approvals: u32,
    pub snap_count: u32,
    pub status_since: Instant,
    /// It has finished a turn, so the agent has a session to resume.
    pub had_turn: bool,
    /// Someone typed into it or messaged it. With no prompt and no input,
    /// going quiet isn't a turn: it's just sitting at its prompt.
    pub had_input: bool,
    /// Something was submitted (Enter, a paste, a message, its starting
    /// prompt) and the turn it started hasn't ended yet.
    pub turn_open: bool,
    /// A turn ended and nothing has been submitted since: the agent is
    /// waiting on you. Going quiet after a redraw isn't this.
    pub turn_done: bool,
}

impl TaskState {
    pub fn open_turn(&mut self) {
        self.had_input = true;
        self.turn_open = true;
        self.turn_done = false;
    }

    pub fn close_turn(&mut self) {
        if self.turn_open {
            self.turn_open = false;
            self.turn_done = true;
        }
    }
}

/// Keys that submit: Return, as plain CR or in the kitty keyboard protocol.
pub fn submits(data: &[u8]) -> bool {
    data.contains(&b'\r') || data.windows(5).any(|w| w == b"\x1b[13u" || w == b"\x1b[13;")
}

pub struct Task {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub agent_cfg: AgentConfig,
    pub cwd: PathBuf,
    pub repo: Option<PathBuf>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub base_branch: Option<String>,
    pub brief: Option<String>,
    /// The task it was started with, for starting it again if it never got going.
    pub prompt: Option<String>,
    /// Its own arguments (an editor card's `-c "exec nvim …"`), kept for a restart.
    pub args: Vec<String>,
    /// An SSH session's host: it runs `ssh` with `args` instead of the agent.
    pub host: Option<String>,
    pub created_unix: i64,
    pid: Option<u32>,
    pub state: Mutex<TaskState>,
    pub term: Mutex<vt100::Parser<Titles>>,
    /// Bumped when the terminal is reflowed for a new size, so attached
    /// windows fetch the new screen.
    pub reflowed: tokio::sync::watch::Sender<u64>,
    /// Held while a message is typed in, so two arriving at once don't
    /// interleave their keystrokes.
    pub typing: tokio::sync::Mutex<()>,
    pub out_tx: broadcast::Sender<Vec<u8>>,
    writer: Mutex<Box<dyn Write + Send>>,
    /// Answers to the terminal's queries, written by a thread of their own:
    /// writing them from the reader would stall it (and so the program)
    /// when a program asks without reading. Bounded; extras are dropped.
    replies: std::sync::mpsc::SyncSender<Vec<u8>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pub snaps: Option<Store>,
    /// Auto mode: commands pitwall would ask about are approved without asking.
    pub auto: AtomicBool,
    /// A terminal's worktree, made when an agent was started in it, so the
    /// agent works on a branch of its own as one pitwall started would.
    pub lent: Mutex<Option<Place>>,
    /// The agent running in this terminal, kept in tasks.json, and when the
    /// shell got the terminal back from it.
    ran: Mutex<(Option<Ran>, Option<Instant>)>,
    /// The agent that was running here when the last daemon stopped, until
    /// it's resumed or dismissed.
    pub offer: Mutex<Option<Ran>>,
}

pub fn slug(s: &str) -> String {
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
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "task".into()
    } else {
        out.chars().take(40).collect()
    }
}

fn nz(v: u16, d: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::new(d).unwrap())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("run git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Create a branch + worktree under ~/.pitwall/worktrees: `branch` if it's
/// given, else one named after `id`.
pub fn create_worktree(
    repo: &Path,
    id: &str,
    branch: Option<&str>,
    base: Option<&str>,
) -> Result<(PathBuf, PathBuf, String, String, Option<String>)> {
    let root = PathBuf::from(
        git(repo, &["rev-parse", "--show-toplevel"]).context("not a git repository")?,
    );
    let repo_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    let chosen = branch.map(str::trim).filter(|b| !b.is_empty());
    if let Some(b) = chosen {
        if let Some(why) = pitwall_core::git::new_branch_problem(&root, b) {
            bail!("{b}: {why}");
        }
    }
    // Pick names that don't exist yet (old runs may have left one behind).
    // A name you chose is used as it is; only its folder gets a number.
    let stem = slug(chosen.unwrap_or(id));
    let mut n = 1;
    let (branch, path) = loop {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let branch = match chosen {
            Some(b) => b.to_string(),
            None => format!("{id}{suffix}"),
        };
        let path = paths::worktrees().join(format!("{}-{stem}{suffix}", slug(&repo_name)));
        let exists = chosen.is_none()
            && git(
                &root,
                &[
                    "rev-parse",
                    "-q",
                    "--verify",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .is_ok();
        if !exists && !path.exists() {
            break (branch, path);
        }
        n += 1;
    };
    std::fs::create_dir_all(paths::worktrees())?;
    // The branch it forks from is where its work goes back to.
    let base_branch = match base {
        None => git(&root, &["symbolic-ref", "--short", "-q", "HEAD"]).ok(),
        Some(b) => git(
            &root,
            &["rev-parse", "-q", "--verify", &format!("refs/heads/{b}")],
        )
        .ok()
        .map(|_| b.to_string()),
    };
    let base = base.unwrap_or("HEAD");
    let base_sha = git(&root, &["rev-parse", &format!("{base}^{{commit}}")])
        .with_context(|| format!("unknown base ref `{base}`"))?;
    let p = path.to_string_lossy().to_string();
    git(
        &root,
        &["worktree", "add", "-q", "-b", &branch, &p, &base_sha],
    )
    .with_context(|| format!("create worktree (does branch {branch} already exist?)"))?;
    Ok((root, path, branch, base_sha, base_branch))
}

fn lent_for(lent: Option<&Place>, id: &str, dir: &Path) -> Option<Result<PathBuf>> {
    let l = lent.filter(|l| l.cwd.is_dir())?;
    let root = git(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(PathBuf::from);
    if root.as_ref() == l.repo.as_ref() || dir.starts_with(&l.cwd) {
        return Some(Ok(l.cwd.clone()));
    }
    Some(Err(anyhow!(
        "{id} already has a worktree for {}; accept or remove it first",
        l.repo.as_deref().unwrap_or(Path::new("?")).display()
    )))
}

/// Remove a task's worktree, and its branch if that's merged. `-d` refuses
/// an unmerged branch, so work that never made it back isn't lost.
pub fn remove_branch(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    remove_worktree(repo, path)?;
    if let Err(e) = git(repo, &["branch", "-d", branch]) {
        tracing::info!("kept {branch}: {e:#}");
    }
    Ok(())
}

pub fn remove_worktree(repo: &Path, path: &Path) -> Result<()> {
    git(
        repo,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )?;
    Ok(())
}

/// Resize the terminal, rewrapping what's in it: lines that had wrapped
/// are joined and wrapped again at the new width, history included, so
/// narrowing doesn't cut lines and widening joins them back up. vt100 can't
/// do this in place, so it's a fresh terminal fed the old one's lines. A
/// full-screen program redraws itself on the resize; that's left alone.
pub fn reflow(term: &mut vt100::Parser<Titles>, rows: u16, cols: u16) {
    if term.screen().alternate_screen() {
        term.set_size(nz(rows, 40), nz(cols, 120));
        return;
    }
    let (old_rows, old_cols) = {
        let (r, c) = term.screen().size();
        (r.get(), c.get())
    };
    let (cur_r, cur_c) = term.screen().cursor_position();
    let modes = term.screen().input_mode_formatted();
    let cap = term.screen().scrollback_capacity();

    // Every row, oldest first: its formatted text, its width in cells, and
    // whether it ran on into the next row.
    struct Row {
        text: String,
        width: usize,
        wrapped: bool,
    }
    let read = |s: &vt100::Screen, r: u16| -> Row {
        let mut width = 0;
        for c in 0..old_cols {
            if let Some(cell) = s.cell(r, c) {
                if cell.has_contents() {
                    width = c as usize + if cell.is_wide() { 2 } else { 1 };
                }
            }
        }
        let wrapped = s.row_wrapped(r);
        Row {
            text: s
                .rows_formatted(0, old_cols)
                .nth(r as usize)
                .unwrap_or_default(),
            width: if wrapped { old_cols as usize } else { width },
            wrapped,
        }
    };
    let mut all: Vec<Row> = vec![];
    {
        let screen = term.screen_mut();
        let have = screen.scrollback_len();
        for k in (1..=have).rev() {
            screen.set_scrollback(k);
            all.push(read(screen, 0));
        }
        screen.set_scrollback(0);
        for r in 0..old_rows {
            all.push(read(screen, r));
        }
    }
    let cursor_row = all.len() - old_rows as usize + cur_r as usize;

    // Rows into lines; where the cursor sits as (line, cell in that line).
    struct Line {
        text: String,
        width: usize,
    }
    let mut lines: Vec<Line> = vec![];
    let mut cursor = (0, 0);
    let mut open = false;
    for (i, row) in all.iter().enumerate() {
        if !open {
            lines.push(Line {
                text: String::new(),
                width: 0,
            });
        }
        let n = lines.len() - 1;
        let line = &mut lines[n];
        if i == cursor_row {
            cursor = (n, line.width + cur_c as usize);
        }
        line.text.push_str(&row.text);
        line.text.push_str("\x1b[m");
        line.width += row.width;
        open = row.wrapped;
    }
    // Blank lines below both the cursor and any text aren't kept.
    while lines.len() > cursor.0 + 1 && lines.last().is_some_and(|l| l.width == 0) {
        lines.pop();
    }

    let rows_of = |w: usize| w.div_ceil(cols as usize).max(1);
    let mut fresh = vt100::Parser::new_with_callbacks(
        nz(rows, 40),
        nz(cols, 120),
        cap,
        std::mem::take(term.callbacks_mut()),
    );
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(&l.text);
    }
    // Back from the end of the text to the cursor's cell: up past the lines
    // after it, then to its row and column within its own line.
    let (line, at) = cursor;
    let below: usize = lines[line + 1..].iter().map(|l| rows_of(l.width)).sum();
    let end_row = rows_of(lines[line].width) - 1;
    let want_row = at / cols as usize;
    let up = below + end_row.saturating_sub(want_row);
    out.push('\r');
    if up > 0 {
        out.push_str(&format!("\x1b[{up}A"));
    }
    if want_row > end_row {
        out.push_str(&format!("\x1b[{}B", want_row - end_row));
    }
    out.push_str(&format!("\x1b[{}G", at % cols as usize + 1));
    out.push_str(&modes);
    fresh.process(out.as_bytes());
    // The query answers this replay produced (none, but to be sure) aren't
    // the program's to see.
    fresh.callbacks_mut().replies.clear();
    *term = fresh;
}

/// Bytes that rebuild a terminal from nothing: the scrollback (printed as
/// lines, so it scrolls into the client's own), then the screen and its
/// modes. A full-screen program is put back on the alternate screen, so the
/// wheel goes to it again rather than to an empty scrollback.
fn repaint(term: &mut vt100::Parser<Titles>, rows: u16, cols: u16) -> Vec<u8> {
    // Reattaching after a lag shouldn't mean waiting on megabytes of history.
    const KEEP: usize = 2000;
    let mut data = Vec::new();
    let alt = term.screen().alternate_screen();
    if !alt {
        let screen = term.screen_mut();
        let have = screen.scrollback_len().min(KEEP);
        for k in (1..=have).rev() {
            screen.set_scrollback(k);
            if let Some(row) = screen.rows_formatted(0, cols).next() {
                data.extend_from_slice(row.as_bytes());
            }
            data.extend_from_slice(b"\x1b[m\r\n");
        }
        screen.set_scrollback(0);
        // Push the last of them off the screen into the scrollback too.
        if have > 0 {
            data.extend(std::iter::repeat_n(b'\n', rows.saturating_sub(1) as usize));
        }
    }
    data.extend_from_slice(b"\x1b[H\x1b[2J");
    if alt {
        data.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[2J");
    }
    data.extend_from_slice(term.screen().state_formatted().as_bytes());
    // The keys a program asked for, so a window attached now sends them too.
    data.extend_from_slice(&term.callbacks().keys.restore());
    data
}

fn agent_config(cfg: &Config, agent: &str) -> Result<AgentConfig> {
    cfg.agents
        .get(agent)
        .cloned()
        .ok_or_else(|| anyhow!("unknown agent `{agent}` (configure it under [agents.{agent}])"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn resolve_command(cmd: &str) -> String {
    if cmd == "$SHELL" {
        return std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    }
    if let Some(var) = cmd.strip_prefix('$') {
        if let Ok(v) = std::env::var(var) {
            return v;
        }
    }
    if cmd.contains('/') {
        return cmd.to_string();
    }
    // Once per command, so the log says which copy agents run.
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static FOUND: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let found = FOUND.get_or_init(Default::default);
    if let Some(p) = found.lock().unwrap().get(cmd) {
        return p.clone();
    }
    let p = pitwall_core::shellenv::resolve(cmd).display().to_string();
    tracing::info!("agent command {cmd} is {p}");
    found.lock().unwrap().insert(cmd.to_string(), p.clone());
    p
}

/// `--settings` JSON that wires Claude Code's hooks to `pitwall hook claude`.
fn claude_settings(exe: &Path) -> String {
    let cmd = format!(
        "'{}' hook claude",
        exe.display().to_string().replace('\'', "'\\''")
    );
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": cmd, "timeout": 600 }] }],
            "Stop": [{ "hooks": [{ "type": "command", "command": cmd, "timeout": 30 }] }],
            "Notification": [{ "hooks": [{ "type": "command", "command": cmd, "timeout": 30 }] }],
        }
    })
    .to_string()
}

/// Whether this Codex starts with these hooks. An older or newer Codex that
/// can't read them would refuse to start at all, so they're tried first
/// with `features list`, which loads the config and nothing else (no model,
/// no network). Asked once per Codex.
fn codex_accepts(program: &str, hooks: &[String]) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Some(ok) = seen.lock().unwrap().get(program) {
        return *ok;
    }
    let mut c = Command::new(program);
    for h in hooks {
        c.arg("-c").arg(h);
    }
    let ok = c
        .args(["features", "list"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return Some(status.success()),
                    Ok(None) if start.elapsed() < std::time::Duration::from_secs(15) => {
                        std::thread::sleep(std::time::Duration::from_millis(50))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return None;
                    }
                }
            }
        })
        .unwrap_or(false);
    seen.lock().unwrap().insert(program.to_string(), ok);
    ok
}

/// `-c` overrides that point Codex's hooks at pitwall.
fn codex_hooks(exe: &Path) -> Vec<String> {
    let cmd = format!(
        "'{}' hook claude",
        exe.display().to_string().replace('\'', "'\\''")
    );
    // A JSON string is a valid TOML basic string.
    let cmd = serde_json::to_string(&cmd).unwrap_or_default();
    vec![
        format!("hooks.PreToolUse=[{{matcher=\".*\", hooks=[{{type=\"command\", command={cmd}, timeout=600}}]}}]"),
        format!("hooks.PermissionRequest=[{{matcher=\".*\", hooks=[{{type=\"command\", command={cmd}, timeout=30}}]}}]"),
        format!("hooks.Stop=[{{hooks=[{{type=\"command\", command={cmd}, timeout=30}}]}}]"),
    ]
}

/// Where a task runs.
#[derive(Clone, Serialize, Deserialize)]
pub struct Place {
    pub cwd: PathBuf,
    pub repo: Option<PathBuf>,
    pub branch: Option<String>,
    pub base: Option<String>,
    #[serde(default)]
    pub base_branch: Option<String>,
}

/// What the daemon keeps of a task in tasks.json, to bring it back after a
/// restart.
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    pub id: String,
    pub name: String,
    pub agent: String,
    #[serde(flatten)]
    pub place: Place,
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub had_turn: bool,
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub lent: Option<Place>,
    pub created_unix: i64,
    /// The agent a terminal had in it, so it can be offered back.
    #[serde(default)]
    pub ran: Option<Ran>,
}

/// An agent seen running in a terminal: what it was, where, and the
/// arguments that pick its session back up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ran {
    pub program: String,
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

impl Ran {
    /// What's typed into the terminal to resume it. The cd is so the shell
    /// is left there afterwards too.
    fn line(&self) -> String {
        let mut line = format!(
            "cd {} && {}",
            sh_quote(&self.cwd.to_string_lossy()),
            self.program
        );
        for a in &self.args {
            line.push(' ');
            line.push_str(&sh_quote(a));
        }
        line
    }
}

fn sh_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The agent in a terminal's foreground and how to resume it, if it's one
/// pitwall knows how to resume.
fn running_agent(cfg: &Config, pid: u32) -> Option<Ran> {
    let program = proc_program(pid)?;
    if !pitwall_core::agents::AGENT_PROGRAMS.contains(&program.as_str()) {
        return None;
    }
    let args = match pitwall_core::claude::live_session(pid).filter(|_| program == "claude") {
        Some(id) => vec!["--resume".into(), id],
        None => cfg
            .agents
            .iter()
            .find(|(k, a)| {
                *k == &program
                    || Path::new(&a.command).file_name().is_some_and(|n| n == program.as_str())
            })
            .map(|(_, a)| a.resume_args.clone())
            .filter(|a| !a.is_empty())?,
    };
    Some(Ran {
        program,
        cwd: proc_cwd(pid)?,
        args,
    })
}

impl Saved {
    /// How a task that couldn't be restored shows in the list.
    pub fn info(&self) -> TaskInfo {
        TaskInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            agent: self.agent.clone(),
            cwd: self.place.cwd.display().to_string(),
            repo: self.place.repo.as_ref().map(|p| p.display().to_string()),
            branch: self.place.branch.clone(),
            base: self.place.base.clone(),
            status: TaskStatus::Exited { code: None },
            attention: false,
            created_unix: self.created_unix,
            last_activity_unix: self.created_unix,
            snapshots: 0,
            pending_approvals: 0,
            preview:
                "not restored: its folder or agent is missing (remove it, or fix that and restart)"
                    .into(),
            live_cwd: None,
            fg: None,
            choices: vec![],
            title: None,
            brief: self.brief.clone(),
            base_branch: self.place.base_branch.clone(),
            turn_done: false,
            auto: self.auto,
            host: self.host.clone(),
            worktree: self
                .lent
                .as_ref()
                .map(|l| &l.cwd)
                .or(self.place.branch.as_ref().map(|_| &self.place.cwd))
                .map(|p| p.display().to_string()),
            resume: None,
        }
    }
}

impl Task {
    pub fn spawn(id: String, req: &NewTask, cfg: &Config) -> Result<Arc<Task>> {
        let agent_cfg = agent_config(cfg, &req.agent)?;
        let place = match (&req.repo, req.no_worktree) {
            (Some(r), false) => {
                let (root, wt, br, base, base_branch) = create_worktree(
                    &paths::expand(r),
                    &id,
                    req.branch.as_deref(),
                    req.base.as_deref(),
                )?;
                Place {
                    cwd: wt,
                    repo: Some(root),
                    branch: Some(br),
                    base: Some(base),
                    base_branch,
                }
            }
            (repo, _) => {
                let p = req
                    .cwd
                    .as_deref()
                    .or(repo.as_deref())
                    .map(paths::expand)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let root = git(&p, &["rev-parse", "--show-toplevel"])
                    .ok()
                    .map(PathBuf::from);
                Place {
                    cwd: p,
                    repo: root,
                    branch: None,
                    base: None,
                    base_branch: None,
                }
            }
        };
        let made = place.branch.clone().zip(place.repo.clone());
        let started = Self::start(id, req, cfg, agent_cfg, place.clone(), &[], now_unix());
        // A worktree for an agent that never started would sit there unlisted.
        if let (Err(_), Some((branch, repo))) = (&started, made) {
            let _ = remove_worktree(&repo, &place.cwd);
            let _ = git(&repo, &["branch", "-D", &branch]);
        }
        started
    }

    /// Start a saved task again in its own folder, picking up its last
    /// session where the agent can.
    pub fn resume(saved: Saved, cfg: &Config) -> Result<Arc<Task>> {
        let agent_cfg = agent_config(cfg, &saved.agent)?;
        // `claude --continue` with no session behind it just exits, as it
        // does for an agent that was still on its trust prompt: those start
        // over with their task instead.
        let resume = saved.had_turn && !agent_cfg.resume_args.is_empty();
        // Started with `--resume <id>`, it already names its session, and
        // Claude keeps that id as the session goes on. `--continue` on top
        // could pick up some other session in the same folder.
        let named = pitwall_core::claude::resume_id(&saved.args).is_some();
        let req = NewTask {
            name: saved.name,
            agent: saved.agent,
            brief: saved.brief,
            prompt: if resume || (named && saved.had_turn) {
                None
            } else {
                saved.prompt
            },
            args: saved.args,
            host: saved.host,
            ..Default::default()
        };
        let extra = if resume && !named {
            agent_cfg.resume_args.clone()
        } else {
            vec![]
        };
        let t = Self::start(
            saved.id,
            &req,
            cfg,
            agent_cfg,
            saved.place,
            &extra,
            saved.created_unix,
        )?;
        {
            // A resumed session sits at its prompt until you give it something.
            let mut st = t.state.lock().unwrap();
            st.had_turn = saved.had_turn;
            st.turn_open = false;
        }
        t.auto.store(saved.auto, Ordering::Relaxed);
        // A worktree removed by hand while the daemon was down isn't lent any more.
        *t.lent.lock().unwrap() = saved.lent.filter(|l| l.cwd.is_dir());
        // Its terminal is a new shell now; the agent that was in it went
        // with the last daemon.
        *t.offer.lock().unwrap() = saved.ran.filter(|r| r.cwd.is_dir());
        Ok(t)
    }

    pub fn saved(&self) -> Saved {
        Saved {
            id: self.id.clone(),
            name: self.name.clone(),
            agent: self.agent.clone(),
            place: Place {
                cwd: self.cwd.clone(),
                repo: self.repo.clone(),
                branch: self.branch.clone(),
                base: self.base.clone(),
                base_branch: self.base_branch.clone(),
            },
            brief: self.brief.clone(),
            prompt: self.prompt.clone(),
            args: self.args.clone(),
            had_turn: self.state.lock().unwrap().had_turn,
            auto: self.auto.load(Ordering::Relaxed),
            host: self.host.clone(),
            lent: self.lent.lock().unwrap().clone(),
            created_unix: self.created_unix,
            // Not yet taken up, it's still on offer after another restart.
            ran: self
                .ran
                .lock()
                .unwrap()
                .0
                .clone()
                .or_else(|| self.offer.lock().unwrap().clone()),
        }
    }

    /// The worktree this terminal already has for the repo `dir` is in.
    /// None when it has none yet; an error when it has one for another repo.
    pub fn lent_for(&self, dir: &Path) -> Option<Result<PathBuf>> {
        lent_for(self.lent.lock().unwrap().as_ref(), &self.id, dir)
    }

    /// The worktree for an agent starting in this terminal from `dir`: the
    /// one it has, or a new branch off whatever the repo has checked out.
    pub fn lend_worktree(&self, dir: &Path, branch: Option<&str>) -> Result<PathBuf> {
        let mut lent = self.lent.lock().unwrap();
        if let Some(had) = lent_for(lent.as_ref(), &self.id, dir) {
            return had;
        }
        let (root, path, branch, base, base_branch) = create_worktree(dir, &self.id, branch, None)?;
        *lent = Some(Place {
            cwd: path.clone(),
            repo: Some(root),
            branch: Some(branch),
            base: Some(base),
            base_branch,
        });
        Ok(path)
    }

    /// Remove the lent worktree. Its branch goes only if it's merged, so
    /// work that never made it back is still there to find.
    pub fn return_worktree(&self, quit: bool) -> Result<()> {
        let Some(l) = self.lent.lock().unwrap().clone() else {
            bail!("{} has no worktree", self.id);
        };
        // A process's cwd comes back with symlinks resolved (/private/var
        // for /var on macOS).
        let wt = l.cwd.canonicalize().unwrap_or_else(|_| l.cwd.clone());
        let inside = || self.live_cwd().is_some_and(|c| c.starts_with(&wt));
        // An agent left running would carry on in a folder that's gone.
        if quit && inside() {
            self.quit_foreground();
        }
        if inside() {
            bail!("{} is still in its worktree; quit the agent or cd out first", self.id);
        }
        if let (Some(repo), Some(branch)) = (&l.repo, &l.branch) {
            if l.cwd.is_dir() {
                remove_branch(repo, &l.cwd, branch)?;
            } else if let Err(e) = git(repo, &["branch", "-d", branch]) {
                tracing::info!("kept {branch}: {e:#}");
            }
        }
        *self.lent.lock().unwrap() = None;
        Ok(())
    }

    fn start(
        id: String,
        req: &NewTask,
        cfg: &Config,
        agent_cfg: AgentConfig,
        place: Place,
        extra: &[String],
        created_unix: i64,
    ) -> Result<Arc<Task>> {
        let Place {
            cwd,
            repo,
            branch,
            base,
            base_branch,
        } = place;
        if !cwd.is_dir() {
            bail!("working directory {} does not exist", cwd.display());
        }

        let rows = if req.rows == 0 { 40 } else { req.rows };
        let cols = if req.cols == 0 { 120 } else { req.cols };
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("openpty: {e}"))?;

        // An SSH session is a plain terminal that runs ssh instead of $SHELL,
        // so it's treated as a shell everywhere else.
        if req.host.is_some() && req.agent != "shell" {
            bail!("an SSH session runs as a shell, not {}", req.agent);
        }
        let program = match &req.host {
            Some(_) => "ssh".to_string(),
            None => resolve_command(&agent_cfg.command),
        };
        let own_args: &[String] = if req.host.is_some() {
            &[]
        } else {
            &agent_cfg.args
        };
        let mut cmd = CommandBuilder::new(&program);
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pitwall"));
        // An interactive terminal (not an editor card's `-c …`) loads your
        // startup files through pitwall's, which keep the shims first.
        let plain_terminal = req.agent == "shell" && req.host.is_none() && req.args.is_empty();
        let shell = Path::new(&program)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if plain_terminal {
            match shell.as_str() {
                // A login shell, as Terminal.app and iTerm2 start. Homebrew
                // puts itself on PATH in .zprofile, which an interactive
                // shell alone never reads, so without it brew's programs
                // were only found when the daemon had your PATH to hand on.
                "zsh" if cfg!(target_os = "macos") => {
                    cmd.arg("-l");
                }
                "bash" => {
                    cmd.arg("--rcfile");
                    cmd.arg(paths::shell_init().join("bashrc"));
                }
                "fish" => {
                    cmd.arg("--init-command");
                    cmd.arg("set -gx PATH $PITWALL_SHIMS (string match -v -- $PITWALL_SHIMS $PATH)");
                }
                _ => {}
            }
        }
        if agent_cfg.hooks.as_deref() == Some("claude") {
            cmd.arg("--settings");
            cmd.arg(claude_settings(&exe));
            // So it knows the other agents are there and how to reach them.
            cmd.arg("--append-system-prompt");
            cmd.arg(pitwall_core::agents::GUIDE);
        }
        if agent_cfg.hooks.as_deref() == Some("codex") {
            // Codex's hooks speak Claude's format, so the same bridge serves
            // both. Codex asks you once to trust a hook it hasn't seen.
            let hooks = codex_hooks(&exe);
            if codex_accepts(&program, &hooks) {
                for c in hooks {
                    cmd.arg("-c");
                    cmd.arg(c);
                }
            } else {
                tracing::warn!(
                    "{program} rejected pitwall's hook config; starting {id} without pitwall's policy"
                );
            }
        }
        for a in own_args.iter().chain(&req.args).chain(extra) {
            cmd.arg(a);
        }
        if let (Some(p), "arg") = (&req.prompt, agent_cfg.prompt_mode.as_str()) {
            cmd.arg(p);
        }
        cmd.cwd(&cwd);
        // CommandBuilder starts from the daemon's environment, so the other
        // terminal's variables have to be removed, not just left uncopied.
        // vars_os: one variable that isn't UTF-8 would make vars() panic.
        for (k, v) in std::env::vars_os() {
            if k.to_str().is_some_and(other_terminal_var) {
                cmd.env_remove(&k);
            } else {
                cmd.env(k, v);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "pitwall");
        cmd.env("TERM_PROGRAM_VERSION", pitwall_core::VERSION);
        cmd.env("PITWALL_TASK_ID", &id);
        cmd.env("PITWALL_SOCKET", paths::socket());
        // Your shell's PATH, whatever started the daemon, with pitwall's
        // own folder first so agents can run `pitwall`.
        if let Some(dir) = exe.parent() {
            let path = pitwall_core::shellenv::path();
            // In a plain terminal, `claude` and friends go through pitwall
            // first, which gives them a worktree (see `pitwall wrap`).
            let shims = if req.agent == "shell" && req.host.is_none() {
                format!("{}:", paths::shims().display())
            } else {
                String::new()
            };
            cmd.env("PATH", format!("{shims}{}:{path}", dir.display()));
        }
        if req.agent == "shell" && req.host.is_none() {
            cmd.env("PITWALL_SHIMS", paths::shims());
        }
        if plain_terminal && shell == "zsh" {
            if let Some(z) = std::env::var_os("ZDOTDIR") {
                cmd.env("PITWALL_USER_ZDOTDIR", z);
            }
            cmd.env("ZDOTDIR", paths::shell_init().join("zsh"));
        }
        for (k, v) in &agent_cfg.env {
            cmd.env(k, v);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("failed to start `{program}`: {e}"))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let pid = child.process_id();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow!("pty reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow!("pty writer: {e}"))?;

        // Never snapshot $HOME, / or anything containing pitwall's own data dir.
        let home = dirs_home();
        let unsafe_root =
            cwd == Path::new("/") || Some(&cwd) == home.as_ref() || paths::home().starts_with(&cwd);
        // Plain shells are driven by the human; only agents get an undo timeline.
        let snaps = if req.agent == "shell" {
            None
        } else if unsafe_root {
            tracing::warn!(
                "snapshots disabled for {id}: {} is too broad to snapshot",
                cwd.display()
            );
            None
        } else {
            match Store::open(
                paths::snapshots().join(format!("{id}.git")),
                &cwd,
                &cfg.snapshots.exclude,
            ) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("snapshots disabled for {id}: {e:#}");
                    None
                }
            }
        };

        let (out_tx, _) = broadcast::channel(1024);
        let (replies, replies_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(32);
        let task = Arc::new(Task {
            id: id.clone(),
            name: req.name.clone(),
            agent: req.agent.clone(),
            agent_cfg: agent_cfg.clone(),
            cwd,
            repo,
            branch,
            base,
            base_branch,
            brief: req.brief.clone(),
            prompt: req.prompt.clone(),
            args: req.args.clone(),
            host: req.host.clone(),
            created_unix,
            pid,
            state: Mutex::new(TaskState {
                status: TaskStatus::Running,
                last_output: Instant::now(),
                last_activity_unix: now_unix(),
                pending_approvals: 0,
                snap_count: 0,
                status_since: Instant::now(),
                had_turn: false,
                had_input: false,
                turn_open: req.prompt.is_some(),
                turn_done: false,
            }),
            term: Mutex::new(vt100::Parser::new_with_callbacks(
                nz(rows, 40),
                nz(cols, 120),
                5000,
                Titles::default(),
            )),
            typing: tokio::sync::Mutex::new(()),
            reflowed: tokio::sync::watch::channel(0).0,
            out_tx,
            writer: Mutex::new(writer),
            replies,
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            snaps,
            auto: AtomicBool::new(false),
            lent: Mutex::new(None),
            ran: Mutex::new((None, None)),
            offer: Mutex::new(None),
        });

        // Weak, so the task can go; its sender going ends this thread.
        let weak = Arc::downgrade(&task);
        std::thread::Builder::new()
            .name(format!("replies-{id}"))
            .spawn(move || {
                for r in replies_rx {
                    match weak.upgrade() {
                        Some(t) => {
                            let _ = t.write(&r);
                        }
                        None => break,
                    }
                }
            })?;

        // Baseline snapshot before the agent touches anything.
        if let Some(s) = &task.snaps {
            if let Err(e) = s.take("task started") {
                tracing::warn!("baseline snapshot failed: {e:#}");
            }
            task.state.lock().unwrap().snap_count = s.count();
        }

        // PTY reader thread.
        let t = task.clone();
        std::thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let replies = {
                                let mut term = t.term.lock().unwrap();
                                term.process(&buf[..n]);
                                // Send under the lock so attach snapshots never miss or duplicate bytes.
                                let _ = t.out_tx.send(buf[..n].to_vec());
                                std::mem::take(&mut term.callbacks_mut().replies)
                            };
                            if !replies.is_empty() {
                                let _ = t.replies.try_send(replies);
                            }
                            let mut st = t.state.lock().unwrap();
                            st.last_output = Instant::now();
                            st.last_activity_unix = now_unix();
                        }
                    }
                }
            })?;

        // Exit watcher thread.
        let t = task.clone();
        std::thread::Builder::new()
            .name(format!("wait-{id}"))
            .spawn(move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                t.state.lock().unwrap().status = TaskStatus::Exited { code };
            })?;

        // Prompt typed into TUIs that don't take it as an argument.
        if let (Some(p), "type") = (&req.prompt, agent_cfg.prompt_mode.as_str()) {
            let t = task.clone();
            let p = p.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let _ = t.write(p.as_bytes());
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = t.write(b"\r");
            });
        }

        Ok(task)
    }

    pub fn write(&self, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        if rows == 0 || cols == 0 {
            return Ok(());
        }
        let mut term = self.term.lock().unwrap();
        let (r, c) = term.screen().size();
        if (r.get(), c.get()) == (rows, cols) {
            return Ok(());
        }
        reflow(&mut term, rows, cols);
        drop(term);
        self.reflowed.send_modify(|n| *n += 1);
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("resize: {e}"))
    }

    pub fn kill(&self) {
        let _ = self.killer.lock().unwrap().kill();
    }

    /// Full-screen repaint bytes plus a receiver positioned right after them.
    pub fn attach(&self) -> (u16, u16, Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let mut term = self.term.lock().unwrap();
        let rx = self.out_tx.subscribe();
        let (r, c) = term.screen().size();
        let data = repaint(&mut term, r.get(), c.get());
        (r.get(), c.get(), data, rx)
    }

    /// Bottom lines of the visible screen (for prompt detection / previews).
    pub fn tail_text(&self, lines: usize) -> Vec<String> {
        let term = self.term.lock().unwrap();
        let screen = term.screen();
        let (_, cols) = screen.size();
        let rows: Vec<String> = screen
            .rows(0, cols.get())
            .map(|r| r.trim_end().to_string())
            .collect();
        let mut out: Vec<String> = rows.into_iter().filter(|r| !r.trim().is_empty()).collect();
        let n = out.len();
        out.drain(..n.saturating_sub(lines));
        out
    }

    /// The terminal's title without the status glyph agents put in front
    /// ("✳ Fix the login loop" from Claude Code).
    pub fn title(&self) -> Option<String> {
        let raw = self.term.lock().unwrap().callbacks().title.clone()?;
        let t = raw
            .trim_start_matches(|c: char| !c.is_alphanumeric() && !c.is_ascii_punctuation())
            .trim()
            .to_string();
        (!t.is_empty()).then_some(t)
    }

    /// What an SSH session is running as far as anyone here can tell: an
    /// agent recognised on its screen, or just ssh. Locally the foreground
    /// process is always ssh, so a remote claude would never show as one.
    fn remote_program(&self) -> String {
        let bottom = self.tail_text(6).join("\n");
        let term = self.term.lock().unwrap();
        let title = term.callbacks().title.clone().unwrap_or_default();
        let screen = term.screen().contents();
        drop(term);
        pitwall_core::agents::agent_on_screen(&title, &bottom, &screen)
            .unwrap_or("ssh")
            .to_string()
    }

    pub fn info(&self) -> TaskInfo {
        let st = self.state.lock().unwrap();
        let attention = st.pending_approvals > 0
            || match &st.status {
                TaskStatus::Running => false,
                TaskStatus::Waiting { .. } | TaskStatus::Exited { .. } => true,
                TaskStatus::Idle => self.agent_cfg.attention_on_idle && st.turn_done,
            };
        let turn_done = st.turn_done;
        let status = st.status.clone();
        let last = st.last_activity_unix;
        let pending = st.pending_approvals;
        let snapshots = st.snap_count;
        let exited = matches!(st.status, TaskStatus::Exited { .. });
        drop(st);
        let choices = if matches!(status, TaskStatus::Waiting { .. }) {
            pitwall_core::prompt::options(&self.tail_text(12).join("\n"))
                .into_iter()
                .map(|(k, l)| (k.to_string(), l))
                .collect()
        } else {
            vec![]
        };
        let (live_cwd, fg) = if exited {
            (None, None)
        } else if self.host.is_some() {
            (self.live_cwd(), Some(self.remote_program()))
        } else {
            (self.live_cwd(), self.foreground())
        };
        // A terminal's lent worktree stands in for the one an agent is made with.
        let lent = self.lent.lock().unwrap().clone();
        let own = || Place {
            cwd: self.cwd.clone(),
            repo: self.repo.clone(),
            branch: self.branch.clone(),
            base: self.base.clone(),
            base_branch: self.base_branch.clone(),
        };
        let place = lent.clone().unwrap_or_else(own);
        let worktree = match (&lent, &self.branch) {
            (Some(l), _) => Some(l.cwd.display().to_string()),
            (None, Some(_)) => Some(self.cwd.display().to_string()),
            (None, None) => None,
        };
        TaskInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            agent: self.agent.clone(),
            cwd: self.cwd.display().to_string(),
            repo: place.repo.as_ref().map(|p| p.display().to_string()),
            branch: place.branch.clone(),
            base: place.base.clone(),
            status,
            attention,
            created_unix: self.created_unix,
            last_activity_unix: last,
            snapshots,
            pending_approvals: pending,
            preview: self.tail_text(1).pop().unwrap_or_default(),
            live_cwd: live_cwd.map(|p| p.display().to_string()),
            fg,
            choices,
            title: self.title(),
            brief: self.brief.clone(),
            turn_done,
            auto: self.auto.load(Ordering::Relaxed),
            base_branch: place.base_branch,
            host: self.host.clone(),
            worktree,
            resume: self.offer.lock().unwrap().as_ref().map(|r| ResumeOffer {
                agent: r.program.clone(),
                line: r.line(),
            }),
        }
    }

    /// Stop whatever runs in the foreground, unless that's the shell itself.
    /// SIGTERM first, which Claude and Codex exit cleanly on, and SIGKILL if
    /// it's still there a few seconds later.
    fn quit_foreground(&self) {
        let Some(group) = self.fg_pid().filter(|p| Some(*p) != self.pid) else {
            return;
        };
        for (signal, wait) in [(libc::SIGTERM, 5), (libc::SIGKILL, 2)] {
            unsafe { libc::killpg(group as libc::pid_t, signal) };
            let until = Instant::now() + Duration::from_secs(wait);
            while Instant::now() < until {
                let gone = unsafe { libc::killpg(group as libc::pid_t, 0) } != 0;
                if gone || self.fg_pid() != Some(group) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn fg_pid(&self) -> Option<u32> {
        self.master
            .lock()
            .unwrap()
            .process_group_leader()
            .filter(|p| *p > 0)
            .map(|p| p as u32)
    }

    /// Keep track of the agent running in this terminal, for offering it
    /// back if the daemon stops with it. True when what tasks.json holds
    /// changed.
    pub fn note_agent(&self, cfg: &Config) -> bool {
        if self.agent != "shell" || self.host.is_some() {
            return false;
        }
        let now = self.fg_pid().and_then(|pid| running_agent(cfg, pid));
        let mut ran = self.ran.lock().unwrap();
        match now {
            Some(r) => {
                ran.1 = None;
                // Started again by hand: nothing left to offer.
                let took_offer = self.offer.lock().unwrap().take().is_some();
                let changed = ran.0.as_ref() != Some(&r);
                ran.0 = Some(r);
                changed || took_offer
            }
            None if ran.0.is_some() => {
                // At shutdown the agent can go a moment before the daemon
                // does. Only once the shell has had the terminal back for a
                // while was it quit.
                let since = *ran.1.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(10) {
                    *ran = (None, None);
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    /// What's running in the foreground, so a `claude` typed into a plain
    /// terminal can be shown as an agent.
    pub fn foreground(&self) -> Option<String> {
        self.fg_pid().or(self.pid).and_then(proc_program)
    }

    /// The directory of whatever runs in the foreground (the shell at its
    /// prompt, or vim/claude/... started from it). Falls back to the process we
    /// spawned when the foreground one has gone or can't be read.
    pub fn live_cwd(&self) -> Option<PathBuf> {
        self.fg_pid()
            .and_then(proc_cwd)
            .or_else(|| self.pid.and_then(proc_cwd))
    }
}

#[cfg(target_os = "macos")]
fn proc_cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    // libc splits the MAXPATHLEN buffer into 32x32 for old-rustc reasons; it's
    // one contiguous NUL-terminated string.
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    let s = path.to_str().ok()?;
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Variables that say which terminal the *daemon* was started from. Passed
/// on, a shell in pitwall would think it's inside that terminal: with TMUX
/// set, tools act tmux-aware and a `tmux` command drives the outer session.
fn other_terminal_var(k: &str) -> bool {
    const EXACT: &[&str] = &[
        "TMUX",
        "TMUX_PANE",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "TERM_SESSION_ID",
        "LC_TERMINAL",
        "LC_TERMINAL_VERSION",
        "STY",
        "WINDOW",
        "WINDOWID",
        "VTE_VERSION",
        "WT_SESSION",
        "TERMINAL_EMULATOR",
        "COLUMNS",
        "LINES",
        // A Claude Code session pitwall was started from. Its agents would
        // count as that session's children, and Claude doesn't save a
        // child's transcript, so `--continue` finds nothing after a restart.
        "CLAUDECODE",
        "CLAUDE_PID",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDE_CODE_CHILD_SESSION",
    ];
    const PREFIX: &[&str] = &[
        "TMUX_",
        "ITERM_",
        "KITTY_",
        "WEZTERM_",
        "GHOSTTY_",
        "ALACRITTY_",
        "KONSOLE_",
        "VSCODE_",
        "CLAUDE_CODE_SESSION",
        "CLAUDE_CODE_MESSAGING_",
    ];
    EXACT.contains(&k) || PREFIX.iter().any(|p| k.starts_with(p))
}

/// Interpreters whose script, not themselves, says what the program is.
const RUNNERS: &[&str] = &["node", "bun", "deno", "python", "python3", "ruby"];

/// A program's name as you'd type it. argv[0] comes first because the
/// executable's own name can be anything: Claude Code's native build runs
/// from `…/claude/versions/2.1.325`. For `node codex.js` and the like it's
/// the script's name without its extension.
fn program_name(comm: &str, argv: &[String]) -> String {
    let base = |s: &str| {
        let f = std::path::Path::new(s.trim_start_matches('-'))
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        const SCRIPT: &[&str] = &[".js", ".mjs", ".cjs", ".ts", ".py", ".rb"];
        match SCRIPT.iter().find(|x| f.ends_with(*x)) {
            Some(x) => f[..f.len() - x.len()].to_string(),
            None => f,
        }
    };
    let name = argv
        .first()
        .map(|a| base(a))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| base(comm));
    if RUNNERS.contains(&name.as_str()) {
        if let Some(script) = argv.iter().skip(1).find(|a| !a.starts_with('-')) {
            let s = base(script);
            if !s.is_empty() {
                return s;
            }
        }
    }
    name
}

#[cfg(target_os = "macos")]
fn proc_program(pid: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe {
        libc::proc_name(
            pid as libc::c_int,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return None;
    }
    let comm = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    Some(program_name(&comm, &proc_argv(pid)))
}

/// argv via KERN_PROCARGS2: argc, the exec path, padding, then argv.
#[cfg(target_os = "macos")]
fn proc_argv(pid: u32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = buf.len();
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    if !ok || len < 4 {
        return vec![];
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
    let mut parts = buf[4..len].split(|b| *b == 0).filter(|s| !s.is_empty());
    parts.next(); // exec path
    parts
        .take(argc)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[cfg(target_os = "linux")]
fn proc_program(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let argv: Vec<String> = std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    Some(program_name(comm.trim(), &argv))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn proc_program(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn proc_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn proc_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    #[test]
    fn proc_cwd_follows_the_process() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .current_dir(&dir)
            .spawn()
            .unwrap();
        let got = super::proc_cwd(child.id()).map(|p| p.canonicalize().unwrap());
        let _ = child.kill();
        assert_eq!(got, Some(dir));
    }

    #[test]
    fn a_repaint_brings_back_scrollback_and_the_alternate_screen() {
        let mut daemon = vt100::Parser::new_with_callbacks(
            std::num::NonZeroU16::new(5).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            100,
            super::Titles::default(),
        );
        for i in 0..12 {
            daemon.process(format!("line {i}\r\n").as_bytes());
        }
        let mut client = vt100::Parser::new(
            std::num::NonZeroU16::new(5).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            1000,
        );
        client.process(&super::repaint(&mut daemon, 5, 20));
        assert_eq!(client.screen().contents(), daemon.screen().contents());
        client.screen_mut().set_scrollback(usize::MAX);
        assert!(
            client.screen().contents().starts_with("line 0"),
            "{}",
            client.screen().contents()
        );
        assert_eq!(
            client.screen().scrollback_len(),
            daemon.screen().scrollback_len()
        );

        daemon.process(b"\x1b[?1049hfull screen");
        let mut client = vt100::Parser::new(
            std::num::NonZeroU16::new(5).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            1000,
        );
        client.process(&super::repaint(&mut daemon, 5, 20));
        assert!(client.screen().alternate_screen());
        assert!(client.screen().contents().contains("full screen"));
        // And the daemon answers where the cursor is.
        daemon.process(b"\x1b[6n");
        assert_eq!(
            std::mem::take(&mut daemon.callbacks_mut().replies),
            b"\x1b[1;12R"
        );
        // The keyboard flags a program asks for: answered, and carried over
        // to a window attached later.
        daemon.process(b"\x1b[?u\x1b[>1u\x1b[?u");
        assert_eq!(daemon.callbacks_mut().replies, b"\x1b[?0u\x1b[?1u");
        assert!(super::repaint(&mut daemon, 5, 20).ends_with(b"\x1b[>1u"));
    }

    #[test]
    fn a_narrowed_terminal_rewraps_and_widens_back() {
        let mut t = vt100::Parser::new_with_callbacks(
            std::num::NonZeroU16::new(4).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            100,
            super::Titles::default(),
        );
        t.process(b"0123456789abcdefghij-tail\r\n$ ");
        // Narrower: nothing is cut, the long line wraps at 10.
        super::reflow(&mut t, 4, 10);
        let all = |t: &mut vt100::Parser<super::Titles>| {
            t.screen_mut().set_scrollback(usize::MAX);
            let s = t.screen().contents();
            t.screen_mut().set_scrollback(0);
            s
        };
        let text = all(&mut t);
        assert!(
            text.contains("0123456789abcdefghij-tail")
                || text.replace('\n', "").contains("0123456789abcdefghij-tail"),
            "{text:?}"
        );
        assert_eq!(
            t.screen().cursor_position(),
            (3, 2),
            "{:?}",
            t.screen().contents()
        );
        // Wider again: it's one line, as it was.
        super::reflow(&mut t, 4, 40);
        assert!(
            t.screen()
                .contents()
                .starts_with("0123456789abcdefghij-tail\n$"),
            "{:?}",
            t.screen().contents()
        );
        assert_eq!(t.screen().cursor_position(), (1, 2));
        // Colours survive.
        t.process(b"\x1b[31mred\x1b[m");
        super::reflow(&mut t, 4, 12);
        let red = (0..4)
            .flat_map(|r| (0..12).map(move |c| (r, c)))
            .any(|(r, c)| {
                t.screen()
                    .cell(r, c)
                    .is_some_and(|x| x.contents() == "r" && x.fgcolor() == vt100::Color::Idx(1))
            });
        assert!(red);
    }

    #[test]
    fn codex_hook_overrides_are_valid_toml() {
        let exe = std::path::Path::new("/Applications/It's Here/pitwall");
        for c in super::codex_hooks(exe) {
            let (key, value) = c.split_once('=').unwrap();
            let doc: toml::Table =
                toml::from_str(&format!("v = {value}")).unwrap_or_else(|e| panic!("{c}: {e}"));
            assert!(key.starts_with("hooks."), "{key}");
            let cmd = &doc["v"][0]["hooks"][0]["command"];
            assert_eq!(
                cmd.as_str(),
                Some("'/Applications/It'\\''s Here/pitwall' hook claude")
            );
        }
    }

    #[test]
    fn other_terminals_variables_stay_out() {
        for k in [
            "TMUX",
            "TMUX_PANE",
            "TERM_PROGRAM",
            "ITERM_SESSION_ID",
            "KITTY_WINDOW_ID",
            "VSCODE_GIT_IPC_HANDLE",
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_CHILD_SESSION",
        ] {
            assert!(super::other_terminal_var(k), "{k}");
        }
        for k in [
            "PATH",
            "HOME",
            "TERM",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CONFIG_DIR",
            "LANG",
            "SSH_AUTH_SOCK",
        ] {
            assert!(!super::other_terminal_var(k), "{k}");
        }
    }

    #[test]
    fn program_names_unwrap_interpreters() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::program_name("claude", &[]), "claude");
        assert_eq!(super::program_name("/bin/zsh", &[]), "zsh");
        // Claude Code's native build: the executable is named for its version.
        assert_eq!(
            super::program_name("2.1.325", &v(&["claude", "--resume", "x"])),
            "claude"
        );
        assert_eq!(super::program_name("zsh", &v(&["-zsh"])), "zsh");
        assert_eq!(
            super::program_name(
                "node",
                &v(&["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"])
            ),
            "codex"
        );
        assert_eq!(
            super::program_name("node", &v(&["node", "--no-warnings", "/opt/lib/codex.js"])),
            "codex"
        );
        assert_eq!(
            super::program_name("python3", &v(&["python3", "-m"])),
            "python3"
        );
    }

    #[test]
    fn proc_program_names_a_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let got = super::proc_program(child.id());
        let _ = child.kill();
        assert_eq!(got.as_deref(), Some("sleep"));
    }
}

#[cfg(test)]
mod turn_tests {
    use super::*;

    #[test]
    fn a_terminal_lends_its_agent_a_worktree_and_takes_it_back() {
        let tmp = std::env::temp_dir().join(format!("pitwall-lend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        // The worktree goes under PITWALL_HOME, not your ~/.pitwall.
        std::env::set_var("PITWALL_HOME", tmp.join("home"));
        let g = |dir: &Path, args: &[&str]| git(dir, args).unwrap();
        g(&repo, &["init", "-q", "-b", "main"]);
        g(&repo, &["config", "user.email", "t@t"]);
        g(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("sub/a.txt"), "a\n").unwrap();
        g(&repo, &["add", "."]);
        g(&repo, &["commit", "-qm", "init"]);

        let req = NewTask {
            name: "term".into(),
            agent: "shell".into(),
            cwd: Some(repo.display().to_string()),
            args: vec!["-c".into(), "sleep 30".into()],
            ..Default::default()
        };
        let t = Task::spawn("term-9".into(), &req, &Config::default()).unwrap();
        assert!(t.info().worktree.is_none());

        let wt = t.lend_worktree(&repo.join("sub"), None).unwrap();
        assert!(wt.join("sub/a.txt").is_file());
        let info = t.info();
        assert_eq!(info.branch.as_deref(), Some("term-9"));
        assert_eq!(info.base_branch.as_deref(), Some("main"));
        assert_eq!(info.worktree.as_deref(), Some(wt.to_str().unwrap()));
        assert_eq!(info.repo.map(PathBuf::from), repo.canonicalize().ok());
        // A second agent in the same terminal picks up the same worktree.
        assert_eq!(t.lend_worktree(&repo, None).unwrap(), wt);
        // It's kept for a daemon restart.
        assert_eq!(t.saved().lent.map(|l| l.cwd), Some(wt.clone()));

        // Unmerged work: the worktree goes, the branch stays.
        std::fs::write(wt.join("b.txt"), "b\n").unwrap();
        g(&wt, &["add", "."]);
        g(&wt, &["commit", "-qm", "agent"]);
        t.return_worktree(false).unwrap();
        assert!(!wt.exists());
        assert!(t.info().worktree.is_none() && t.info().branch.is_none());
        g(&repo, &["rev-parse", "--verify", "refs/heads/term-9"]);
        // The next one gets a fresh worktree beside the kept branch.
        let again = t.lend_worktree(&repo, None).unwrap();
        assert_ne!(again, wt);
        assert_eq!(t.info().branch.as_deref(), Some("term-9-2"));

        // A name you give is used as it is, and one that's taken is refused.
        t.return_worktree(false).unwrap();
        assert!(t.lend_worktree(&repo, Some("term-9")).is_err());
        assert!(t.info().worktree.is_none());
        let named = t.lend_worktree(&repo, Some("fix/login")).unwrap();
        assert!(
            named.to_string_lossy().ends_with("-fix-login"),
            "{}",
            named.display()
        );
        assert_eq!(t.info().branch.as_deref(), Some("fix/login"));

        t.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn returning_a_worktree_can_quit_what_runs_in_it() {
        let tmp = std::env::temp_dir().join(format!("pitwall-quit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::env::set_var("PITWALL_HOME", tmp.join("home"));
        let g = |dir: &Path, args: &[&str]| git(dir, args).unwrap();
        g(&repo, &["init", "-q", "-b", "main"]);
        g(&repo, &["config", "user.email", "t@t"]);
        g(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        g(&repo, &["add", "."]);
        g(&repo, &["commit", "-qm", "init"]);

        // Waits to be told where to go, then takes the terminal in a process
        // group of its own there while the shell stays put, as `claude`
        // started through the shim does.
        let go = tmp.join("go");
        let script = format!(
            "while [ ! -f '{0}' ]; do sleep 0.1; done; (cd \"$(cat '{0}')\" && exec perl -MPOSIX -e '\
             $SIG{{TTOU}} = \"IGNORE\"; setpgrp; open(T, \"+<\", \"/dev/tty\") or die; \
             tcsetpgrp(fileno(T), $$); sleep 30'); sleep 30",
            go.display()
        );
        let req = NewTask {
            name: "term".into(),
            agent: "shell".into(),
            cwd: Some(repo.display().to_string()),
            args: vec!["-c".into(), script],
            ..Default::default()
        };
        let t = Task::spawn("term-8".into(), &req, &Config::default()).unwrap();
        let wt = t.lend_worktree(&repo, None).unwrap();
        std::fs::write(&go, wt.display().to_string()).unwrap();
        let wt = wt.canonicalize().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !t.live_cwd().is_some_and(|c| c.starts_with(&wt)) {
            assert!(
                Instant::now() < until,
                "the job never started in the worktree"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(t.return_worktree(false).is_err());
        assert!(wt.exists());
        t.return_worktree(true).unwrap();
        assert!(!wt.exists());

        t.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_restored_terminal_offers_its_agent_back() {
        let tmp = std::env::temp_dir().join(format!("pitwall-offer-{}", std::process::id()));
        let wt = tmp.join("it's a worktree");
        std::fs::create_dir_all(&wt).unwrap();
        let req = NewTask {
            name: "term".into(),
            agent: "shell".into(),
            cwd: Some(tmp.display().to_string()),
            args: vec!["-c".into(), "sleep 30".into()],
            no_worktree: true,
            ..Default::default()
        };
        let cfg = Config::default();
        let t = Task::spawn("term-7".into(), &req, &cfg).unwrap();
        assert!(t.info().resume.is_none());
        let mut saved = t.saved();
        t.kill();
        saved.ran = Some(Ran {
            program: "claude".into(),
            cwd: wt.clone(),
            args: vec!["--resume".into(), "66a6f1e0-f38c".into()],
        });

        let t = Task::resume(saved, &cfg).unwrap();
        let offer = t.info().resume.unwrap();
        assert_eq!(offer.agent, "claude");
        assert_eq!(
            offer.line,
            format!(
                "cd '{}' && claude --resume 66a6f1e0-f38c",
                wt.display().to_string().replace('\'', "'\\''")
            )
        );
        // Still offered if the daemon restarts again before it's taken up.
        assert_eq!(t.saved().ran.map(|r| r.cwd), Some(wt.clone()));
        t.offer.lock().unwrap().take();
        assert!(t.info().resume.is_none() && t.saved().ran.is_none());

        t.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn state() -> TaskState {
        TaskState {
            status: TaskStatus::Idle,
            last_output: Instant::now(),
            last_activity_unix: 0,
            pending_approvals: 0,
            snap_count: 0,
            status_since: Instant::now(),
            had_turn: false,
            had_input: false,
            turn_open: false,
            turn_done: false,
        }
    }

    #[test]
    fn only_a_submitted_turn_ends() {
        let mut st = state();
        // Quiet at its prompt, nothing submitted: not a finished turn.
        st.close_turn();
        assert!(!st.turn_done);
        st.open_turn();
        st.close_turn();
        assert!(st.turn_done);
        // A second quiet spell (a redraw) changes nothing; typing clears it.
        st.close_turn();
        assert!(st.turn_done);
        st.open_turn();
        assert!(!st.turn_done && st.turn_open);
    }

    #[test]
    fn return_submits_in_either_keyboard_protocol() {
        assert!(submits(b"fix the bug\r"));
        assert!(submits(b"\x1b[13u"));
        assert!(submits(b"\x1b[13;1u"));
        assert!(!submits(b"typing"));
        assert!(!submits(b"\x1b[A"));
    }
}

#[cfg(test)]
mod shell_tests {
    #[test]
    fn finds_a_command_the_way_the_shell_does() {
        assert!(super::resolve_command("sh").ends_with("/sh"));
        assert_eq!(super::resolve_command("/bin/echo"), "/bin/echo");
    }
}
