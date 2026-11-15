//! `muster` with no arguments: a herdr-style TUI inside your own terminal.
//! Agents run in the daemon; this is just a client, so quitting never kills them.

mod app;
mod term;
mod ui;

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use muster_core::config::Config;
use muster_core::proto::{Push, Request, Response};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

pub enum Ev {
    Input(Event),
    Push(Push),
    Reply(app::Reply),
    Git(app::GitData),
    Tick,
    Disconnected,
}

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let mut t = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    t.clear()?;
    Ok(t)
}

fn restore() {
    let mut out = std::io::stdout();
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
}

/// Run `$VISUAL`/`$EDITOR` on a file, jumping to `line` for editors that support it.
fn run_external(path: &Path, line: usize) -> Result<()> {
    let ed = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let mut parts = ed.split_whitespace();
    let prog = parts.next().unwrap_or("vi").to_string();
    let mut args: Vec<String> = parts.map(String::from).collect();
    let base = Path::new(&prog)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let file = path.display().to_string();
    match base.as_str() {
        "vi" | "vim" | "nvim" | "nano" | "emacs" | "micro" | "kak" | "mg" => {
            args.push(format!("+{line}"));
            args.push(file);
        }
        "hx" | "helix" => args.push(format!("{file}:{line}")),
        "code" | "cursor" | "zed" | "subl" => {
            if !args.iter().any(|a| a == "-w" || a == "--wait") && base != "zed" {
                args.push("--wait".into());
            }
            if base == "code" || base == "cursor" {
                args.push("-g".into());
            }
            args.push(format!("{file}:{line}"));
        }
        _ => args.push(file),
    }
    let status = std::process::Command::new(&prog).args(&args).status()?;
    if !status.success() {
        bail!("{prog} exited with {status}");
    }
    Ok(())
}

pub async fn run() -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        bail!("muster's TUI needs a terminal; use subcommands (see `muster --help`) in scripts");
    }
    let (client, mut pushes) = crate::cli::connect().await?;
    client.call(Request::Subscribe).await?;
    let tasks = match client.call(Request::ListTasks).await? {
        Response::Tasks(t) => t,
        _ => vec![],
    };
    let approvals = match client.call(Request::Approvals).await? {
        Response::Approvals(a) => a,
        _ => vec![],
    };
    let cfg = Config::load().unwrap_or_default();

    let (tx, mut rx) = mpsc::unbounded_channel::<Ev>();

    // Input on a plain thread so it can be paused while $EDITOR owns the terminal.
    let paused = Arc::new(AtomicBool::new(false));
    {
        let tx = tx.clone();
        let paused = paused.clone();
        std::thread::spawn(move || loop {
            if paused.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            match crossterm::event::poll(Duration::from_millis(40)) {
                Ok(true) => {
                    if paused.load(Ordering::SeqCst) {
                        continue;
                    }
                    match crossterm::event::read() {
                        Ok(e) => {
                            if tx.send(Ev::Input(e)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                Ok(false) => {}
                Err(_) => break,
            }
        });
    }
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(p) = pushes.recv().await {
                if tx.send(Ev::Push(p)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Ev::Disconnected);
        });
    }
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            loop {
                iv.tick().await;
                if tx.send(Ev::Tick).is_err() {
                    return;
                }
            }
        });
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default_hook(info);
    }));

    let mut terminal = setup()?;
    let mut app = app::App::new(client, tx.clone(), cfg, tasks, approvals);
    app.request_git();

    let res: Result<()> = async {
        loop {
            if app.dirty {
                terminal.draw(|f| ui::draw(f, &mut app))?;
                app.dirty = false;
            }
            let Some(ev) = rx.recv().await else { break };
            app.handle(ev);
            // Drain bursts (agent output) before redrawing.
            let mut n = 0;
            while let Ok(ev) = rx.try_recv() {
                app.handle(ev);
                n += 1;
                if n > 2000 {
                    break;
                }
            }
            if std::mem::take(&mut app.bell) {
                let mut out = std::io::stdout();
                let _ = out.write_all(b"\x07");
                let _ = out.flush();
            }
            if let Some((path, line)) = app.external.take() {
                paused.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                restore();
                let r = run_external(&path, line);
                terminal = setup()?;
                paused.store(false, Ordering::SeqCst);
                app.after_external(&path);
                if let Err(e) = r {
                    app.notify(format!("{e:#}"), true);
                }
                app.dirty = true;
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;
    restore();
    let running = app
        .tasks
        .values()
        .filter(|t| !matches!(t.status, muster_core::proto::TaskStatus::Exited { .. }))
        .count();
    if running > 0 {
        println!("{running} agent(s) still running in the background — run `muster` to come back, `muster stop` to end them.");
    }
    res
}
