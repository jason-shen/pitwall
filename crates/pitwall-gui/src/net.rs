//! Daemon connection: starts the daemon if needed and forwards pushes to the UI.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use pitwall_core::client::Client;
use pitwall_core::paths;
use pitwall_core::proto::{Approval, Push, Request, Response, TaskInfo};

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

pub fn pitwall_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sib = dir.join("pitwall");
            if sib.exists() {
                return sib;
            }
        }
    }
    PathBuf::from("pitwall")
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
    let mut c = std::process::Command::new(pitwall_binary());
    c.arg("daemon").stdin(std::process::Stdio::null());
    // Started from the Dock, the app has launchd's bare environment; the
    // daemon and its agents get your shell's.
    for (k, v) in pitwall_core::shellenv::env() {
        c.env(k, v);
    }
    c.env("PATH", pitwall_core::shellenv::path());
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

/// Terminals on screen right now, kept up to date by the app each frame.
/// Output from any other terminal is taken in without redrawing for it.
pub type Shown = std::sync::Arc<std::sync::RwLock<std::collections::HashSet<String>>>;

/// How often terminal output may cause a redraw. A fast agent can print far
/// more often than that; drawing for each chunk had the window (and, in
/// see-through mode, the compositor behind it) redraw at the display's rate.
const OUTPUT_FRAME: Duration = Duration::from_millis(33);
/// Output from terminals that aren't on screen still has to be taken in
/// (notifications come from it, and its screen should be current when you
/// switch to it), but a few frames a second keep up.
const HIDDEN_FRAME: Duration = Duration::from_millis(250);

pub fn start(rt: &tokio::runtime::Runtime, ctx: egui::Context, tx: Sender<Ev>, shown: Shown) {
    rt.spawn(async move {
        let mut spawned = false;
        loop {
            match Client::connect(&paths::socket()).await {
                Ok((client, mut pushes)) => {
                    // Allow a fresh spawn if this daemon later goes away
                    // (e.g. restarted from the sidebar).
                    spawned = false;
                    let _ = client.call(Request::Subscribe).await;
                    // A daemon from an older version drops the connection
                    // over a request it doesn't know.
                    if let Ok(Response::Pong { version }) = client.call(Request::Ping).await {
                        if version == pitwall_core::VERSION {
                            let _ = client.call(Request::NameWorktrees).await;
                        }
                    }
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
                    // One frame per OUTPUT_FRAME slot: the next free slot,
                    // and whether a frame is already asked for in it.
                    let mut next_frame = std::time::Instant::now();
                    let mut asked: Option<std::time::Instant> = None;
                    while let Some(p) = pushes.recv().await {
                        let output_of = match &p {
                            Push::Output { id, .. } => Some(id.clone()),
                            _ => None,
                        };
                        if tx.send(Ev::Push(p)).is_err() {
                            return;
                        }
                        match output_of {
                            None => ctx.request_repaint(),
                            Some(id) if shown.read().is_ok_and(|s| s.contains(&id)) => {
                                let now = std::time::Instant::now();
                                if let Some(at) = asked.filter(|at| now >= *at) {
                                    // That frame has been drawn; its slot is used.
                                    next_frame = at + OUTPUT_FRAME;
                                    asked = None;
                                }
                                if asked.is_none() {
                                    if now >= next_frame {
                                        ctx.request_repaint();
                                        next_frame = now + OUTPUT_FRAME;
                                    } else {
                                        ctx.request_repaint_after(next_frame - now);
                                        asked = Some(next_frame);
                                    }
                                }
                            }
                            Some(_) => ctx.request_repaint_after(HIDDEN_FRAME),
                        }
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
