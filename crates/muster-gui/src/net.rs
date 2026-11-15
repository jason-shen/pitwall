//! Daemon connection: starts the daemon if needed and forwards pushes to the UI.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use muster_core::client::Client;
use muster_core::paths;
use muster_core::proto::{Approval, Push, Request, Response, TaskInfo};

use crate::app::{GitData, Reply};

pub enum Ev {
    Connected {
        client: Client,
        tasks: Vec<TaskInfo>,
        approvals: Vec<Approval>,
    },
    Push(Push),
    Disconnected,
    Reply(Reply),
    Git(GitData),
}

pub fn muster_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sib = dir.join("muster");
            if sib.exists() {
                return sib;
            }
        }
    }
    PathBuf::from("muster")
}

/// Apps launched from the Dock/Finder get a bare environment. Borrow the user's
/// login-shell environment so agents find `claude`, `codex`, API keys, etc.
fn login_env() -> Vec<(String, String)> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let Ok(out) = std::process::Command::new(&shell)
        .args(["-l", "-c", "env"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .filter(|(k, _)| !matches!(*k, "PWD" | "OLDPWD" | "SHLVL" | "_"))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn spawn_daemon() {
    use std::os::unix::process::CommandExt;
    let _ = std::fs::create_dir_all(paths::home());
    let Ok(log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::daemon_log())
    else {
        return;
    };
    let mut c = std::process::Command::new(muster_binary());
    c.arg("daemon").stdin(std::process::Stdio::null());
    for (k, v) in login_env() {
        c.env(k, v);
    }
    if let Ok(l2) = log.try_clone() {
        c.stdout(l2);
    }
    c.stderr(log);
    unsafe {
        c.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    let _ = c.spawn();
}

extern "C" {
    fn setsid() -> i32;
}

pub fn start(rt: &tokio::runtime::Runtime, ctx: egui::Context, tx: Sender<Ev>) {
    rt.spawn(async move {
        let mut spawned = false;
        loop {
            match Client::connect(&paths::socket()).await {
                Ok((client, mut pushes)) => {
                    // Allow a fresh spawn if this daemon later goes away
                    // (e.g. restarted from the sidebar).
                    spawned = false;
                    let _ = client.call(Request::Subscribe).await;
                    let tasks = match client.call(Request::ListTasks).await {
                        Ok(Response::Tasks(t)) => t,
                        _ => vec![],
                    };
                    let approvals = match client.call(Request::Approvals).await {
                        Ok(Response::Approvals(a)) => a,
                        _ => vec![],
                    };
                    if tx
                        .send(Ev::Connected {
                            client,
                            tasks,
                            approvals,
                        })
                        .is_err()
                    {
                        return;
                    }
                    ctx.request_repaint();
                    while let Some(p) = pushes.recv().await {
                        if tx.send(Ev::Push(p)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                    let _ = tx.send(Ev::Disconnected);
                    ctx.request_repaint();
                }
                Err(_) => {
                    if !spawned {
                        let _ = tokio::task::spawn_blocking(spawn_daemon).await;
                        spawned = true;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    });
}
