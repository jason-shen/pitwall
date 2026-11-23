//! `pitwall wrap claude …`: what typing `claude` in a pitwall terminal runs,
//! through the shims in ~/.pitwall/shims. Inside a git repo the agent starts
//! in the terminal's own worktree instead of your checkout, so its work can
//! be reviewed and merged like an agent pitwall started. Anywhere else, or
//! when that can't be done, the real program runs as if typed directly.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use pitwall_core::client::Client;
use pitwall_core::paths;
use pitwall_core::proto::{Request, Response};

/// Subcommands that don't do any work in the repo: no worktree for these.
const NO_WORK: &[&str] = &[
    "--version", "-v", "-V", "--help", "-h", "help", "version", "mcp", "config", "update",
    "upgrade", "doctor", "login", "logout", "auth", "install", "setup-token", "completion",
];

pub async fn run(program: &str, args: &[String]) -> Result<()> {
    let Some(real) = find_real(program) else {
        eprintln!("{program}: command not found");
        std::process::exit(127);
    };
    let mut cmd = Command::new(&real);
    cmd.arg0(program).args(args);
    let dir = match session_dir(program, args) {
        Some(d) => Some(d),
        None => worktree_dir(args).await,
    };
    if let Some(dir) = dir {
        cmd.current_dir(&dir).env("PWD", &dir);
    }
    // exec only returns on failure.
    let e = cmd.exec();
    eprintln!("{program}: {e}");
    std::process::exit(126);
}

/// `claude --resume <id>` has to run in the folder the session started in,
/// or Claude says it's from a different directory. That folder wins over
/// lending a worktree: the session's work was already going there.
fn session_dir(program: &str, args: &[String]) -> Option<PathBuf> {
    if program != "claude" {
        return None;
    }
    let s = pitwall_core::claude::find(pitwall_core::claude::resume_id(args)?)?;
    if !s.cwd.is_dir() {
        return None;
    }
    if std::env::current_dir().ok().as_deref() != Some(s.cwd.as_path()) {
        eprintln!(
            "\x1b[2mpitwall: resuming in the session's folder, {}\x1b[0m",
            tilde(&s.cwd)
        );
    }
    Some(s.cwd)
}

/// The folder to start in, or None to stay where we are.
async fn worktree_dir(args: &[String]) -> Option<PathBuf> {
    let id = std::env::var("PITWALL_TASK_ID").ok()?;
    if std::env::var("PITWALL_NO_WORKTREE").is_ok_and(|v| !v.is_empty() && v != "0") {
        return None;
    }
    if args.first().is_some_and(|a| NO_WORK.contains(&a.as_str())) || !worktrees_on() {
        return None;
    }
    let here = std::env::current_dir().ok()?;
    // Already in a worktree pitwall made: that's where the work goes.
    if here.starts_with(paths::worktrees()) {
        return None;
    }
    let root = pitwall_core::git::root(&here)?;
    let ask = async {
        let (c, _) = Client::connect(&paths::socket()).await?;
        c.call(Request::LendWorktree {
            id,
            dir: here.display().to_string(),
        })
        .await
    };
    // A new worktree waits for you to name its branch in the Pitwall
    // window, which the daemon gives up on after ten minutes.
    let ask = tokio::time::timeout(Duration::from_secs(660), ask);
    tokio::pin!(ask);
    let answer = match tokio::time::timeout(Duration::from_millis(400), &mut ask).await {
        Ok(a) => a,
        Err(_) => {
            // With no window open this is only git being slow, so it
            // doesn't claim you're being asked.
            eprintln!("\x1b[2mpitwall: making its worktree…\x1b[0m");
            ask.await
        }
    };
    let why = match answer {
        Ok(Ok(Response::Text(p))) => return Some(enter(PathBuf::from(p), &here, &root)),
        Ok(Ok(Response::Error(e))) => e,
        Ok(Ok(_)) => "unexpected answer".into(),
        Ok(Err(e)) => format!("{e:#}"),
        Err(_) => "the daemon didn't answer".into(),
    };
    eprintln!("pitwall: no worktree ({why}); running here");
    None
}

fn enter(path: PathBuf, here: &Path, root: &Path) -> PathBuf {
    // The same subfolder you were in, if the branch has it.
    let sub = here.strip_prefix(root).ok().map(|s| path.join(s)).filter(|p| p.is_dir());
    let dir = sub.unwrap_or(path);
    eprintln!(
        "\x1b[2mpitwall: in its own worktree, {} (PITWALL_NO_WORKTREE=1 to skip)\x1b[0m",
        tilde(&dir)
    );
    dir
}

/// `worktree` under [app] in config.toml, the app's "own branch and
/// worktree" setting; on unless it's set to false.
fn worktrees_on() -> bool {
    pitwall_core::config::app_flag("worktree").unwrap_or(true)
}

/// The program as the shell would find it with the shims taken off PATH.
fn find_real(program: &str) -> Option<PathBuf> {
    let shims = paths::shims();
    let shims = shims.canonicalize().unwrap_or(shims);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.canonicalize().map_or(true, |c| c != shims))
        .map(|d| d.join(program))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn tilde(p: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(h) if p.starts_with(&h) => format!("~/{}", p.strip_prefix(&h).unwrap().display()),
        _ => p.display().to_string(),
    }
}
