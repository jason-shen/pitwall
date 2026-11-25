//! `pitwall tiller-pane`: one tmux pane on a Tiller host, shown in this
//! terminal. A pitwall card runs it the way an SSH session card runs `ssh`,
//! so the daemon's terminal, scrollback and tiling all work as usual.

use std::io::Write;

use anyhow::{anyhow, bail, Result};
use pitwall_core::tiller::{self, Incoming, Link, TillerHost};
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::signal::unix::{signal, SignalKind};

/// Restores the terminal however we leave.
struct Raw;

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub async fn pane(host: TillerHost, session: &str, pane: Option<u32>) -> Result<()> {
    eprintln!("Connecting to {} ({})…", host.name, host.ssh_target());
    // Not batch mode: ssh can still ask for a password or a host key here,
    // since this runs in a terminal.
    let (link, mut rx) = Link::connect_ssh(&host, false).await?;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
    let layout = link
        .call("sessions.attach", json!({"name": session, "cols": cols, "rows": rows}))
        .await?;
    let pane = match pane {
        Some(p) => p,
        None => tiller::active_pane(&layout).ok_or_else(|| anyhow!("{session} has no panes"))?,
    };
    if !tiller::has_pane(&layout, pane) {
        bail!("pane {pane} isn't in {session}");
    }

    let _raw = crossterm::terminal::enable_raw_mode().map(|_| Raw);
    // Clear the "Connecting" line; the pane's snapshot paints the rest.
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[H\x1b[2J");
    let _ = out.flush();

    let typing = link.clone();
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdin.read(&mut buf).await {
            if n == 0 {
                break;
            }
            typing.input(pane, &buf[..n]);
        }
    });

    let mut winch = signal(SignalKind::window_change())?;
    let reason = loop {
        tokio::select! {
            m = rx.recv() => match m {
                None => break "tillerd closed the connection".to_string(),
                Some(Incoming::PaneOut { pane: p, data }) if p == pane => {
                    if out.write_all(&data).and_then(|_| out.flush()).is_err() {
                        break "terminal closed".into();
                    }
                }
                Some(Incoming::Event { name, data }) => match name.as_str() {
                    "tmux.layout" if data["session"] == session && !tiller::has_pane(&data, pane) => {
                        break "the pane closed".into();
                    }
                    "tmux.exit" if data["session"] == session => {
                        break format!("the session ended ({})", data["reason"].as_str().unwrap_or("exit"));
                    }
                    _ => {}
                },
                Some(_) => {}
            },
            _ = winch.recv() => {
                if let Ok((c, r)) = crossterm::terminal::size() {
                    let l = link.clone();
                    let s = session.to_string();
                    tokio::spawn(async move {
                        let _ = l.call("sessions.resize", json!({"name": s, "cols": c, "rows": r})).await;
                    });
                }
            }
        }
    };
    drop(_raw);
    let _ = link.call("sessions.detach", json!({"name": session})).await;
    eprintln!("\r\n[{reason}]");
    Ok(())
}
