//! Unix-socket server: one connection per client, requests handled concurrently,
//! pushes (screen output, events) multiplexed onto the same stream.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use pitwall_core::proto::{ClientFrame, Push, Request, Response, ServerFrame};
use pitwall_core::{hosts, paths, policy, wire};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::{fleet, hook, ops, Daemon};

pub async fn serve(d: Arc<Daemon>, sock: &Path) -> Result<()> {
    let listener = UnixListener::bind(sock)?;
    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let d = d.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(d, stream).await {
                tracing::debug!("connection ended: {e:#}");
            }
        });
    }
}

struct Conn {
    id: u64,
    tx: mpsc::UnboundedSender<ServerFrame>,
    attached: Mutex<HashMap<String, JoinHandle<()>>>,
    subscription: Mutex<Option<JoinHandle<()>>>,
}

impl Conn {
    fn push(&self, p: Push) {
        let _ = self.tx.send(ServerFrame::Push(p));
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        for (_, h) in self.attached.lock().unwrap().drain() {
            h.abort();
        }
        if let Some(h) = self.subscription.lock().unwrap().take() {
            h.abort();
        }
    }
}

async fn handle_conn(d: Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerFrame>();
    let writer = tokio::spawn(async move {
        while let Some(f) = rx.recv().await {
            if wire::write_frame(&mut wr, &f).await.is_err() {
                break;
            }
        }
    });
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let conn = Arc::new(Conn {
        id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        tx,
        attached: Mutex::new(HashMap::new()),
        subscription: Mutex::new(None),
    });

    while let Some(frame) = wire::read_frame::<_, ClientFrame>(&mut rd).await? {
        let (d, conn) = (d.clone(), conn.clone());
        // Keystrokes must stay ordered, so handle Input inline; everything else concurrently.
        if matches!(frame.body, Request::Input { .. } | Request::Resize { .. }) {
            let body = handle(&d, &conn, frame.body)
                .await
                .unwrap_or_else(|e| Response::Error(format!("{e:#}")));
            let _ = conn.tx.send(ServerFrame::Reply {
                req: frame.req,
                body,
            });
            continue;
        }
        tokio::spawn(async move {
            let body = handle(&d, &conn, frame.body)
                .await
                .unwrap_or_else(|e| Response::Error(format!("{e:#}")));
            let _ = conn.tx.send(ServerFrame::Reply {
                req: frame.req,
                body,
            });
        });
    }
    drop(conn);
    writer.abort();
    Ok(())
}

async fn handle(d: &Arc<Daemon>, conn: &Arc<Conn>, req: Request) -> Result<Response> {
    Ok(match req {
        Request::Ping => Response::Pong {
            version: pitwall_core::VERSION.into(),
        },
        Request::Shutdown => {
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = nix_kill_self();
            });
            Response::Ok
        }
        Request::Subscribe => {
            let mut rx = d.events.subscribe();
            let c = conn.clone();
            let h = tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(p) => c.push(p),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            if let Some(old) = conn.subscription.lock().unwrap().replace(h) {
                old.abort();
            }
            Response::Ok
        }

        Request::ListTasks => Response::Tasks(d.list()),
        Request::NewTask(req) => Response::Task(d.new_task(req).await?),
        Request::Kill { id } => {
            let t = d.task(&id)?;
            t.kill();
            Response::Ok
        }
        Request::Remove { id, prune_worktree } => {
            match d.remove_task(&id, prune_worktree).await? {
                kept if kept.is_empty() => Response::Ok,
                kept => Response::Text(kept),
            }
        }

        Request::LendWorktree { id, dir } => {
            let t = d.task(&id)?;
            if t.agent != "shell" || t.host.is_some() {
                bail!("{id} isn't a terminal");
            }
            let dir = paths::expand(&dir);
            let mut name = None;
            if t.lent_for(&dir).is_none() {
                let root = pitwall_core::git::root(&dir).unwrap_or_else(|| dir.clone());
                match d.ask_branch(&id, &root).await? {
                    Some(None) => bail!("skipped"),
                    Some(n) => name = n,
                    None => {}
                }
            }
            let t2 = t.clone();
            let path = tokio::task::spawn_blocking(move || t2.lend_worktree(&dir, name.as_deref()))
                .await??;
            d.saved_changed(&t);
            Response::Text(path.display().to_string())
        }
        Request::NameWorktrees => {
            d.add_namer(conn.id, conn.tx.clone());
            Response::Ok
        }
        Request::WorktreeName { id, name } => {
            d.name_branch(&id, name)?;
            Response::Ok
        }
        Request::ReturnWorktree { id, quit } => {
            let t = d.task(&id)?;
            let t2 = t.clone();
            let kept = tokio::task::spawn_blocking(move || t2.return_worktree(quit)).await??;
            d.saved_changed(&t);
            if kept.is_empty() {
                Response::Ok
            } else {
                Response::Text(kept)
            }
        }

        Request::Attach {
            id,
            rows,
            cols,
            repaints,
        } => {
            let t = d.task(&id)?;
            if rows > 0 && cols > 0 {
                t.resize(rows, cols)?;
            }
            let (r, c, data, mut rx) = t.attach();
            conn.push(Push::Screen {
                id: t.id.clone(),
                rows: r,
                cols: c,
                data,
            });
            let c2 = conn.clone();
            let t2 = t.clone();
            let mut reflowed = t.reflowed.subscribe();
            reflowed.mark_unchanged();
            let h = tokio::spawn(async move {
                let resend = |rx: &mut broadcast::Receiver<Vec<u8>>| {
                    let (r, c, data, new_rx) = t2.attach();
                    *rx = new_rx;
                    c2.push(Push::Screen {
                        id: t2.id.clone(),
                        rows: r,
                        cols: c,
                        data,
                    });
                };
                loop {
                    tokio::select! {
                        got = rx.recv() => match got {
                            Ok(data) => c2.push(Push::Output {
                                id: t2.id.clone(),
                                data,
                            }),
                            Err(broadcast::error::RecvError::Lagged(_)) => resend(&mut rx),
                            Err(_) => break,
                        },
                        // Reflowed for a new size: once the resizing stops
                        // (a window being dragged sends many), the new screen.
                        changed = reflowed.changed(), if repaints => {
                            if changed.is_err() {
                                break;
                            }
                            loop {
                                tokio::select! {
                                    _ = tokio::time::sleep(Duration::from_millis(150)) => break,
                                    c = reflowed.changed() => if c.is_err() { break },
                                }
                            }
                            resend(&mut rx);
                        }
                    }
                }
            });
            if let Some(old) = conn.attached.lock().unwrap().insert(t.id.clone(), h) {
                old.abort();
            }
            Response::Task(t.info())
        }
        Request::Detach { id } => {
            if let Some(h) = conn.attached.lock().unwrap().remove(&id) {
                h.abort();
            }
            Response::Ok
        }
        Request::Input { id, data } => {
            let t = d.task(&id)?;
            {
                let mut st = t.state.lock().unwrap();
                st.had_input = true;
                if super::task::submits(&data) {
                    st.open_turn();
                }
            }
            // A program that isn't reading its input would block this write;
            // that shouldn't hold up a runtime thread.
            tokio::task::spawn_blocking(move || t.write(&data)).await??;
            Response::Ok
        }
        Request::Prompt { id, text, from } => {
            let t = d.task(&id)?;
            if from.as_deref() == Some(t.id.as_str()) {
                anyhow::bail!("{} is you", t.id);
            }
            // The Return would pick an option on a prompt it's waiting on.
            if matches!(
                t.info().status,
                pitwall_core::proto::TaskStatus::Waiting { .. }
            ) {
                anyhow::bail!("{} is waiting on a prompt; answer that first", t.id);
            }
            let line = pitwall_core::agents::message_line(from.as_deref(), &text);
            let _turn = t.typing.lock().await;
            t.state.lock().unwrap().open_turn();
            t.write(line.as_bytes())?;
            // A Return in the same write reads as part of a paste to TUIs
            // like Claude Code, which then don't submit it.
            tokio::time::sleep(Duration::from_millis(150)).await;
            // A permission prompt that came up in the meantime would take the
            // Return as "yes". Leave the text typed and say so instead.
            let info = t.info();
            if matches!(info.status, pitwall_core::proto::TaskStatus::Waiting { .. })
                || info.pending_approvals > 0
            {
                anyhow::bail!(
                    "{} started waiting on a prompt; the message is typed but not sent",
                    t.id
                );
            }
            t.write(b"\r")?;
            d.audit.log(serde_json::json!({
                "source": "agent", "event": "message", "from": from, "to": t.id, "text": text,
            }));
            d.emit(Push::Message {
                from,
                to: t.id.clone(),
                text,
            });
            Response::Ok
        }
        Request::Brief { id, step, done } => {
            // Two ticks at once would each read the old file and one would
            // be lost; they take turns.
            static BRIEFS: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _turn = BRIEFS.lock().unwrap_or_else(|e| e.into_inner());
            let t = d.task(&id)?;
            let Some(path) = t.brief.clone() else {
                anyhow::bail!("{} wasn't handed a brief", t.id);
            };
            // The daemon writes it, not the agent: the brief is outside the
            // agent's worktree, where a sandboxed agent can't write.
            let text = std::fs::read_to_string(&path)?;
            let text = match step {
                Some(n) => {
                    let new =
                        pitwall_core::brief::set(&text, n, done).map_err(anyhow::Error::msg)?;
                    let tmp = format!("{path}.tmp");
                    std::fs::write(&tmp, &new)?;
                    std::fs::rename(&tmp, &path)?;
                    new
                }
                None => text,
            };
            Response::Text(pitwall_core::brief::listing(&text))
        }
        Request::Read { id, lines } => {
            Response::Text(d.task(&id)?.tail_text(lines.clamp(1, 500)).join("\n"))
        }
        Request::Resize { id, rows, cols } => {
            d.task(&id)?.resize(rows, cols)?;
            Response::Ok
        }

        Request::Snapshots { id } => {
            let t = d.task(&id)?;
            let list = tokio::task::spawn_blocking(move || match &t.snaps {
                Some(s) => s.list(),
                None => Ok(vec![]),
            })
            .await??;
            Response::Snapshots(list)
        }
        Request::TakeSnapshot { id, label } => {
            let t = d.task(&id)?;
            if t.snaps.is_none() {
                return Err(anyhow!("snapshots are not available for this task"));
            }
            let label = if label.trim().is_empty() {
                "manual checkpoint".to_string()
            } else {
                label
            };
            Response::Snapshot(d.snapshot(&t, label).await)
        }
        Request::Revert { id, snapshot } => {
            let t = d.task(&id)?;
            let t2 = t.clone();
            let info = tokio::task::spawn_blocking(move || {
                t2.snaps
                    .as_ref()
                    .ok_or_else(|| anyhow!("snapshots are not available for this task"))?
                    .restore(&snapshot)
            })
            .await??;
            t.state.lock().unwrap().snap_count = info.seq;
            d.audit.log(serde_json::json!({"source": "snapshot", "event": "revert", "task": t.id, "to": info.label}));
            d.emit(Push::SnapshotTaken {
                task: t.id.clone(),
                snapshot: info.clone(),
            });
            d.emit_task(&t);
            Response::Snapshot(Some(info))
        }
        Request::Diff { id, from, to } => {
            let t = d.task(&id)?;
            let text = tokio::task::spawn_blocking(move || {
                t.snaps
                    .as_ref()
                    .ok_or_else(|| anyhow!("snapshots are not available for this task"))?
                    .diff(from.as_deref(), to.as_deref())
            })
            .await??;
            Response::Diff(text)
        }

        Request::Approvals => Response::Approvals(d.approvals()),
        Request::Decide { id, allow } => {
            d.decide(id, allow)?;
            Response::Ok
        }
        Request::SetAuto { id, on } => {
            d.set_auto(&id, on)?;
            Response::Ok
        }
        Request::DismissResume { id } => {
            d.dismiss_resume(&id)?;
            Response::Ok
        }

        Request::Hook(h) => Response::Hook(hook::handle(d, h).await),
        Request::Classify { command } => {
            let c = policy::classify(&command, &d.cfg.policy);
            Response::Classified {
                tier: c.tier,
                decision: c.decision,
                reason: c.reason,
            }
        }

        Request::Hosts => Response::Hosts(fleet::host_infos(&hosts::current(&d.cfg.hosts))),
        Request::FleetRun {
            selector,
            command,
            confirmed,
        } => {
            let all = hosts::current(&d.cfg.hosts);
            let targets: Vec<_> = hosts::select(&all, &selector)
                .into_iter()
                .cloned()
                .collect();
            if targets.is_empty() {
                return Err(anyhow!("selector `{selector}` matches no hosts"));
            }
            let c = policy::classify(&command, &d.cfg.policy);
            let names: Vec<String> = targets.iter().map(|h| h.name.clone()).collect();
            let auto = c.decision == pitwall_core::proto::Decision::Allow;
            if !auto && !confirmed {
                return Ok(Response::NeedsConfirm {
                    tier: c.tier,
                    decision: c.decision,
                    reason: c.reason,
                    hosts: names,
                });
            }
            let results = fleet::run(&d.cfg, &targets, &command).await;
            d.audit.log(serde_json::json!({
                "source": "fleet", "hosts": names, "command": command, "tier": c.tier.label(),
                "decision": if auto { "auto" } else { "human-confirmed" },
                "exits": results.iter().map(|r| (r.host.clone(), r.exit)).collect::<Vec<_>>(),
            }));
            Response::Fleet(results)
        }
        Request::OpsAsk { question, selector } => {
            let session = ops::start(d.clone(), question, selector)?;
            Response::OpsStarted { session }
        }
    })
}

fn nix_kill_self() -> std::io::Result<()> {
    // SIGTERM ourselves so the normal shutdown path in `run` executes.
    let r = unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
