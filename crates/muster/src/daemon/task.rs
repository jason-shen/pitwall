//! An agent task: a PTY running an agent CLI inside an (optionally isolated) worktree,
//! with a server-side vt100 screen so any client can attach and get a full repaint.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use muster_core::config::{AgentConfig, Config};
use muster_core::proto::{NewTask, TaskInfo, TaskStatus};
use muster_core::{now_unix, paths};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::broadcast;

use super::snapshot::Store;

/// Keeps the title a program gives its terminal; vt100's default callbacks
/// drop it.
#[derive(Default)]
pub struct Titles {
    pub title: Option<String>,
}

impl vt100::Callbacks for Titles {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }
}

pub struct TaskState {
    pub status: TaskStatus,
    pub last_output: Instant,
    pub last_activity_unix: i64,
    pub pending_approvals: u32,
    pub snap_count: u32,
    pub status_since: Instant,
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
    pub created_unix: i64,
    pid: Option<u32>,
    pub state: Mutex<TaskState>,
    pub term: Mutex<vt100::Parser<Titles>>,
    pub out_tx: broadcast::Sender<Vec<u8>>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pub snaps: Option<Store>,
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

/// Create `muster/<id>` branch + worktree under ~/.muster/worktrees.
fn create_worktree(
    repo: &Path,
    id: &str,
    base: Option<&str>,
) -> Result<(PathBuf, PathBuf, String, String)> {
    let root = PathBuf::from(
        git(repo, &["rev-parse", "--show-toplevel"]).context("not a git repository")?,
    );
    let repo_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    // Pick a branch/worktree name that doesn't exist yet (old runs may have left one behind).
    let mut n = 1;
    let (branch, path) = loop {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let branch = format!("muster/{id}{suffix}");
        let path = paths::worktrees().join(format!("{}-{id}{suffix}", slug(&repo_name)));
        let exists = git(
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
    let base = base.unwrap_or("HEAD");
    let base_sha = git(&root, &["rev-parse", &format!("{base}^{{commit}}")])
        .with_context(|| format!("unknown base ref `{base}`"))?;
    let p = path.to_string_lossy().to_string();
    git(
        &root,
        &["worktree", "add", "-q", "-b", &branch, &p, &base_sha],
    )
    .with_context(|| format!("create worktree (does branch {branch} already exist?)"))?;
    Ok((root, path, branch, base_sha))
}

pub fn remove_worktree(repo: &Path, path: &Path) -> Result<()> {
    git(
        repo,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )?;
    Ok(())
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
    cmd.to_string()
}

/// `--settings` JSON that wires Claude Code's hooks to `muster hook claude`.
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

impl Task {
    pub fn spawn(id: String, req: &NewTask, cfg: &Config) -> Result<Arc<Task>> {
        let agent_cfg = cfg.agents.get(&req.agent).cloned().ok_or_else(|| {
            anyhow!(
                "unknown agent `{}` (configure it under [agents.{}])",
                req.agent,
                req.agent
            )
        })?;

        let (cwd, repo, branch, base) = match (&req.repo, req.no_worktree) {
            (Some(r), false) => {
                let (root, wt, br, base) =
                    create_worktree(&paths::expand(r), &id, req.base.as_deref())?;
                (wt, Some(root), Some(br), Some(base))
            }
            (Some(r), true) => {
                let p = req
                    .cwd
                    .as_deref()
                    .map(paths::expand)
                    .unwrap_or_else(|| paths::expand(r));
                let root = git(&p, &["rev-parse", "--show-toplevel"])
                    .ok()
                    .map(PathBuf::from);
                (p, root, None, None)
            }
            (None, _) => {
                let p = req
                    .cwd
                    .as_deref()
                    .map(paths::expand)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let root = git(&p, &["rev-parse", "--show-toplevel"])
                    .ok()
                    .map(PathBuf::from);
                (p, root, None, None)
            }
        };
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

        let program = resolve_command(&agent_cfg.command);
        let mut cmd = CommandBuilder::new(&program);
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("muster"));
        if agent_cfg.hooks.as_deref() == Some("claude") {
            cmd.arg("--settings");
            cmd.arg(claude_settings(&exe));
            // So it knows the other agents are there and how to reach them.
            cmd.arg("--append-system-prompt");
            cmd.arg(muster_core::agents::GUIDE);
        }
        for a in agent_cfg.args.iter().chain(req.args.iter()) {
            cmd.arg(a);
        }
        if let (Some(p), "arg") = (&req.prompt, agent_cfg.prompt_mode.as_str()) {
            cmd.arg(p);
        }
        cmd.cwd(&cwd);
        // CommandBuilder starts from the daemon's environment, so the other
        // terminal's variables have to be removed, not just left uncopied.
        for (k, v) in std::env::vars() {
            if other_terminal_var(&k) {
                cmd.env_remove(&k);
            } else {
                cmd.env(k, v);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "muster");
        cmd.env("TERM_PROGRAM_VERSION", muster_core::VERSION);
        cmd.env("MUSTER_TASK_ID", &id);
        cmd.env("MUSTER_SOCKET", paths::socket());
        if let Some(dir) = exe.parent() {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{path}", dir.display()));
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

        // Never snapshot $HOME, / or anything containing muster's own data dir.
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
        let task = Arc::new(Task {
            id: id.clone(),
            name: req.name.clone(),
            agent: req.agent.clone(),
            agent_cfg: agent_cfg.clone(),
            cwd,
            repo,
            branch,
            base,
            created_unix: now_unix(),
            pid,
            state: Mutex::new(TaskState {
                status: TaskStatus::Running,
                last_output: Instant::now(),
                last_activity_unix: now_unix(),
                pending_approvals: 0,
                snap_count: 0,
                status_since: Instant::now(),
            }),
            term: Mutex::new(vt100::Parser::new_with_callbacks(
                nz(rows, 40),
                nz(cols, 120),
                5000,
                Titles::default(),
            )),
            out_tx,
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            snaps,
        });

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
                            {
                                let mut term = t.term.lock().unwrap();
                                term.process(&buf[..n]);
                                // Send under the lock so attach snapshots never miss or duplicate bytes.
                                let _ = t.out_tx.send(buf[..n].to_vec());
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
        term.set_size(nz(rows, 40), nz(cols, 120));
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
        let term = self.term.lock().unwrap();
        let rx = self.out_tx.subscribe();
        let screen = term.screen();
        let (r, c) = screen.size();
        let mut data = b"\x1b[H\x1b[2J".to_vec();
        data.extend_from_slice(screen.state_formatted().as_bytes());
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

    pub fn info(&self) -> TaskInfo {
        let st = self.state.lock().unwrap();
        let attention = st.pending_approvals > 0
            || match &st.status {
                TaskStatus::Running => false,
                TaskStatus::Waiting { .. } | TaskStatus::Exited { .. } => true,
                TaskStatus::Idle => self.agent_cfg.attention_on_idle,
            };
        let status = st.status.clone();
        let last = st.last_activity_unix;
        let pending = st.pending_approvals;
        let snapshots = st.snap_count;
        let exited = matches!(st.status, TaskStatus::Exited { .. });
        drop(st);
        let choices = if matches!(status, TaskStatus::Waiting { .. }) {
            muster_core::prompt::options(&self.tail_text(12).join("\n"))
                .into_iter()
                .map(|(k, l)| (k.to_string(), l))
                .collect()
        } else {
            vec![]
        };
        let (live_cwd, fg) = if exited {
            (None, None)
        } else {
            (self.live_cwd(), self.foreground())
        };
        TaskInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            agent: self.agent.clone(),
            cwd: self.cwd.display().to_string(),
            repo: self.repo.as_ref().map(|p| p.display().to_string()),
            branch: self.branch.clone(),
            base: self.base.clone(),
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
/// on, a shell in muster would think it's inside that terminal: with TMUX
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
    fn other_terminals_variables_stay_out() {
        for k in [
            "TMUX",
            "TMUX_PANE",
            "TERM_PROGRAM",
            "ITERM_SESSION_ID",
            "KITTY_WINDOW_ID",
            "VSCODE_GIT_IPC_HANDLE",
        ] {
            assert!(super::other_terminal_var(k), "{k}");
        }
        for k in [
            "PATH",
            "HOME",
            "TERM",
            "ANTHROPIC_API_KEY",
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
