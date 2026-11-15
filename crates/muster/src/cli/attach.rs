//! `muster attach`: bridge the local terminal to a task's PTY, tmux-style.

use std::io::{Read, Write};

use anyhow::{bail, Result};
use muster_core::client::Client;
use muster_core::proto::{Push, Request, Response, TaskStatus};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, UnboundedReceiver};

const DETACH: u8 = 0x1d; // Ctrl-]

struct RawGuard;

impl RawGuard {
    fn enter() -> Result<RawGuard> {
        crossterm::terminal::enable_raw_mode()?;
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?1049h\x1b[H\x1b[2J")?;
        out.flush()?;
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        // Reset modes a TUI may have left on, then leave the alternate screen.
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1049l");
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub async fn run(client: Client, mut pushes: UnboundedReceiver<Push>, id: String) -> Result<()> {
    client.call(Request::Subscribe).await?;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
    let Response::Task(info) = client.call(Request::Attach { id, rows, cols }).await? else {
        bail!("unexpected reply")
    };
    let id = info.id;

    // Blocking stdin reader on its own thread (cancel-safe, unlike select! on async stdin).
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if in_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut winch = signal(SignalKind::window_change())?;
    let guard = RawGuard::enter()?;
    let mut out = std::io::stdout();
    let mut exit_msg = None;

    loop {
        tokio::select! {
            p = pushes.recv() => match p {
                Some(Push::Screen { id: pid, data, .. }) | Some(Push::Output { id: pid, data }) if pid == id => {
                    out.write_all(&data)?;
                    out.flush()?;
                }
                Some(Push::TaskUpdated(t)) if t.id == id => {
                    if let TaskStatus::Exited { .. } = t.status {
                        exit_msg = Some(format!("[{} {}]", id, t.status.label()));
                        break;
                    }
                }
                Some(Push::TaskRemoved { id: pid }) if pid == id => {
                    exit_msg = Some(format!("[{id} removed]"));
                    break;
                }
                Some(_) => {}
                None => {
                    exit_msg = Some("[daemon connection lost]".into());
                    break;
                }
            },
            data = in_rx.recv() => {
                let Some(data) = data else { break };
                if let Some(pos) = data.iter().position(|b| *b == DETACH) {
                    if pos > 0 {
                        client.send(Request::Input { id: id.clone(), data: data[..pos].to_vec() });
                    }
                    break;
                }
                client.send(Request::Input { id: id.clone(), data });
            },
            _ = winch.recv() => {
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    client.send(Request::Resize { id: id.clone(), rows, cols });
                }
            }
        }
    }

    drop(guard);
    let _ = client.call(Request::Detach { id: id.clone() }).await;
    println!(
        "{}",
        exit_msg.unwrap_or_else(|| format!(
            "[detached from {id} — reattach with `muster attach {id}`]"
        ))
    );
    // The stdin thread is blocked in read(); exit instead of waiting for it.
    std::process::exit(0);
}
