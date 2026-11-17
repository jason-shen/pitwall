//! CLI client commands.

mod attach;

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use pitwall_core::client::Client;
use pitwall_core::config::{Config, DEFAULT_TOML};
use pitwall_core::proto::{
    ApprovalSource, Decision, HookRequest, HostResult, NewTask, OpsEvent, Push, Request, Response,
    TaskInfo, TaskStatus, Tier,
};
use pitwall_core::{now_unix, paths, policy};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::Cmd;

/// Connect to the daemon, starting it if needed.
pub async fn connect() -> Result<(Client, UnboundedReceiver<Push>)> {
    let sock = paths::socket();
    if let Ok(c) = Client::connect(&sock).await {
        return Ok(c);
    }
    spawn_daemon()?;
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(c) = Client::connect(&sock).await {
            return Ok(c);
        }
    }
    bail!(
        "could not start the pitwall daemon; see {}",
        paths::daemon_log().display()
    )
}

fn spawn_daemon() -> Result<()> {
    std::fs::create_dir_all(paths::home())?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::daemon_log())?;
    let mut c = std::process::Command::new(std::env::current_exe()?);
    c.arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().context("spawn daemon")?;
    Ok(())
}

pub async fn stop() -> Result<()> {
    match Client::connect(&paths::socket()).await {
        Ok((c, _)) => {
            c.call(Request::Shutdown).await?;
            println!("daemon stopped");
        }
        Err(_) => println!("daemon is not running"),
    }
    Ok(())
}

pub fn init(force: bool) -> Result<()> {
    let p = paths::config();
    if p.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", p.display());
    }
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, DEFAULT_TOML)?;
    println!("wrote {}", p.display());
    Ok(())
}

pub fn classify(cmd: &str) -> Result<()> {
    let cfg = Config::load()?;
    let c = policy::classify(cmd, &cfg.policy);
    println!(
        "{} → {}  ({})",
        tier_badge(c.tier),
        decision_word(c.decision),
        c.reason
    );
    Ok(())
}

/// Agent hook bridge. Never fails loudly: if the daemon is unreachable, exit 0 with no opinion.
pub async fn hook(kind: &str) -> Result<()> {
    if kind != "claude" {
        bail!("unknown hook kind `{kind}`");
    }
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let event = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|e| e.as_str())
                .map(String::from)
        })
        .unwrap_or_default();

    let Ok(Ok((client, _))) =
        tokio::time::timeout(Duration::from_secs(2), Client::connect(&paths::socket())).await
    else {
        return Ok(());
    };
    let req = Request::Hook(HookRequest {
        task_id: std::env::var("PITWALL_TASK_ID").ok(),
        payload: input,
    });
    let Ok(Ok(Response::Hook(reply))) =
        tokio::time::timeout(Duration::from_secs(590), client.call(req)).await
    else {
        return Ok(());
    };
    if event == "PreToolUse" {
        if let Some(d) = reply.decision {
            let out = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": decision_word(d),
                    "permissionDecisionReason": reply.reason,
                }
            });
            println!("{out}");
        }
    }
    Ok(())
}

fn decision_word(d: Decision) -> &'static str {
    match d {
        Decision::Allow => "allow",
        Decision::Ask => "ask",
        Decision::Deny => "deny",
    }
}

fn tier_badge(t: Tier) -> &'static str {
    match t {
        Tier::ReadOnly => "\x1b[32mread-only\x1b[0m",
        Tier::Mutating => "\x1b[33mmutating\x1b[0m",
        Tier::Destructive => "\x1b[31mdestructive\x1b[0m",
    }
}

fn status_badge(s: &TaskStatus) -> String {
    match s {
        TaskStatus::Running => "\x1b[32m● running\x1b[0m".into(),
        TaskStatus::Waiting { .. } => "\x1b[33m◆ waiting\x1b[0m".into(),
        TaskStatus::Idle => "\x1b[36m○ idle\x1b[0m".into(),
        TaskStatus::Exited { .. } => format!("\x1b[90m✕ {}\x1b[0m", s.label()),
    }
}

fn ago(unix: i64) -> String {
    let s = (now_unix() - unix).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w {
        s.chars().take(w).collect()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

/// `pitwall agents`: agents first, then terminals, with what each is doing.
fn agents(tasks: &[TaskInfo], json: bool) -> Result<()> {
    use pitwall_core::agents::agent_kind;
    let me = std::env::var("PITWALL_TASK_ID").unwrap_or_default();
    let mut v: Vec<&TaskInfo> = tasks
        .iter()
        .filter(|t| !t.id.starts_with("edit-"))
        .collect();
    v.sort_by_key(|t| (agent_kind(t).is_none(), t.created_unix));
    let doing = |t: &TaskInfo| match &t.status {
        TaskStatus::Waiting { reason } => reason.clone(),
        _ => t
            .title
            .clone()
            .unwrap_or_else(|| t.preview.trim().to_string()),
    };
    let folder = |t: &TaskInfo| {
        let d = t.live_cwd.clone().unwrap_or_else(|| t.cwd.clone());
        match std::env::var("HOME") {
            Ok(h) if d.starts_with(&h) => format!("~{}", &d[h.len()..]),
            _ => d,
        }
    };
    if json {
        let out: Vec<_> = v
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id, "agent": agent_kind(t), "status": t.status.label(),
                    "folder": folder(t), "branch": t.branch, "doing": doing(t), "you": t.id == me,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if v.is_empty() {
        println!("no agents or terminals running");
        return Ok(());
    }
    println!(
        "{:<22} {:<9} {:<10} {:<34} DOING",
        "ID", "STATUS", "AGENT", "FOLDER"
    );
    for t in v {
        let id = if t.id == me {
            format!("{} (you)", t.id)
        } else {
            t.id.clone()
        };
        let doing: String = doing(t).chars().take(60).collect();
        println!(
            "{:<22} {:<9} {:<10} {:<34} {}",
            id,
            t.status.label(),
            agent_kind(t).unwrap_or("terminal"),
            folder(t),
            doing
        );
    }
    Ok(())
}

/// Block until a task stops working. An agent just messaged may not have
/// started yet, so a quiet one only counts as done once it has been seen
/// working or a few seconds have passed.
async fn wait_turn(client: &Client, id: &str, timeout: u64) -> Result<()> {
    let start = std::time::Instant::now();
    let mut saw_running = false;
    loop {
        let Response::Tasks(tasks) = client.call(Request::ListTasks).await? else {
            bail!("unexpected reply")
        };
        let t = match tasks.iter().find(|t| t.id == id) {
            Some(t) => t,
            None => {
                let m: Vec<_> = tasks.iter().filter(|t| t.id.starts_with(id)).collect();
                match m.as_slice() {
                    [t] => *t,
                    [] => bail!("no task {id}"),
                    _ => bail!("{id} matches more than one task"),
                }
            }
        };
        let running = matches!(t.status, TaskStatus::Running);
        saw_running |= running;
        if !running && (saw_running || start.elapsed() >= Duration::from_secs(5)) {
            match &t.status {
                TaskStatus::Waiting { reason } => println!("{} waiting: {reason}", t.id),
                s => println!("{} {}", t.id, s.label()),
            }
            return Ok(());
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            bail!("{} is still working after {timeout}s", t.id);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn print_tasks(tasks: &[TaskInfo]) {
    if tasks.is_empty() {
        println!("no tasks — start one with `pitwall new <name>`");
        return;
    }
    println!(
        "\x1b[1m{}  {}  {}  {}  {}  LAST\x1b[0m",
        pad("ID", 18),
        pad("STATUS", 12),
        pad("AGENT", 8),
        pad("BRANCH", 22),
        pad("SNAPS", 5)
    );
    for t in tasks {
        // status badge has escape codes; pad the visible label
        let label = match &t.status {
            TaskStatus::Exited { .. } => t.status.label(),
            other => other.label(),
        };
        let badge = status_badge(&t.status);
        let visible = label.chars().count() + 2;
        let spaces = " ".repeat(12usize.saturating_sub(visible));
        let flag = if t.pending_approvals > 0 {
            format!(" \x1b[33m[{} approval]\x1b[0m", t.pending_approvals)
        } else {
            String::new()
        };
        println!(
            "{}  {badge}{spaces}  {}  {}  {}  {:>4} ago  {}{flag}",
            pad(&t.id, 18),
            pad(&t.agent, 8),
            pad(t.branch.as_deref().unwrap_or("-"), 22),
            pad(&t.snapshots.to_string(), 5),
            ago(t.last_activity_unix),
            pad(&t.preview, 60),
        );
    }
}

fn print_results(results: &[HostResult]) {
    for r in results {
        let status = match (&r.error, r.exit) {
            (Some(e), _) => format!("\x1b[31merror: {e}\x1b[0m"),
            (None, Some(0)) => "\x1b[32mexit 0\x1b[0m".into(),
            (None, Some(c)) => format!("\x1b[31mexit {c}\x1b[0m"),
            (None, None) => "no exit status".into(),
        };
        println!("\x1b[1m── {} \x1b[0m({status}, {} ms)", r.host, r.ms);
        if !r.stdout.is_empty() {
            print!("{}", r.stdout);
            if !r.stdout.ends_with('\n') {
                println!();
            }
        }
        if !r.stderr.is_empty() {
            eprintln!("\x1b[90m{}\x1b[0m", r.stderr.trim_end());
        }
    }
}

async fn prompt_line(msg: &str) -> Result<String> {
    print!("{msg}");
    std::io::stdout().flush()?;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    Ok(lines
        .next_line()
        .await?
        .unwrap_or_default()
        .trim()
        .to_string())
}

fn git_root(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let (client, pushes) = connect().await?;
    match cmd {
        Cmd::New {
            name,
            agent,
            repo,
            base,
            here,
            prompt,
            attach,
            args,
        } => {
            let cwd = std::env::current_dir()?;
            let repo = repo.or_else(|| git_root(&cwd));
            let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
            let req = NewTask {
                name,
                agent,
                repo: repo.clone(),
                cwd: Some(cwd.display().to_string()),
                base,
                no_worktree: here || repo.is_none(),
                args,
                prompt,
                rows,
                cols,
                brief: None,
            };
            let Response::Task(t) = client.call(Request::NewTask(req)).await? else {
                bail!("unexpected reply")
            };
            println!("started \x1b[1m{}\x1b[0m ({}) in {}", t.id, t.agent, t.cwd);
            if let Some(b) = &t.branch {
                println!("  branch {b}");
            }
            if attach {
                return attach::run(client, pushes, t.id).await;
            }
            println!("  attach: pitwall attach {}", t.id);
        }
        Cmd::Ls { json } => {
            let Response::Tasks(tasks) = client.call(Request::ListTasks).await? else {
                bail!("unexpected reply")
            };
            if json {
                let v: Vec<_> = tasks
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "id": t.id, "name": t.name, "agent": t.agent, "status": t.status.label(), "attention": t.attention,
                            "cwd": t.cwd, "live_cwd": t.live_cwd, "fg": t.fg, "branch": t.branch, "snapshots": t.snapshots, "pending_approvals": t.pending_approvals,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                print_tasks(&tasks);
            }
        }
        Cmd::Attach { id } => return attach::run(client, pushes, id).await,
        Cmd::Agents { json } => {
            let Response::Tasks(tasks) = client.call(Request::ListTasks).await? else {
                bail!("unexpected reply")
            };
            agents(&tasks, json)?;
        }
        Cmd::Tell { id, text } => {
            let text = text.join(" ");
            if text.trim().is_empty() {
                bail!("nothing to say: pitwall tell {id} \"<message>\"");
            }
            let from = std::env::var("PITWALL_TASK_ID")
                .ok()
                .filter(|s| !s.is_empty());
            client
                .call(Request::Prompt {
                    id: id.clone(),
                    text,
                    from,
                })
                .await?;
            println!("sent to {id}");
        }
        Cmd::Brief { action, step, id } => {
            let Some(id) = id.or_else(|| {
                std::env::var("PITWALL_TASK_ID")
                    .ok()
                    .filter(|s| !s.is_empty())
            }) else {
                bail!("run this inside an agent, or name one with --id");
            };
            let step = match (action.as_deref(), step) {
                (None, _) => None,
                (Some(_), Some(n)) => Some(n),
                (Some(a), None) => bail!("which step? pitwall brief {a} <number>"),
            };
            let req = Request::Brief {
                id,
                step,
                done: action.as_deref() == Some("tick"),
            };
            let Response::Text(t) = client.call(req).await? else {
                bail!("unexpected reply")
            };
            println!("{t}");
        }
        Cmd::Read { id, lines } => {
            let Response::Text(t) = client.call(Request::Read { id, lines }).await? else {
                bail!("unexpected reply")
            };
            println!("{t}");
        }
        Cmd::Wait { id, timeout } => wait_turn(&client, &id, timeout).await?,
        Cmd::Send { id, text, no_enter } => {
            let mut data = text.join(" ").into_bytes();
            if !no_enter {
                data.push(b'\r');
            }
            client.call(Request::Input { id, data }).await?;
        }
        Cmd::Kill { id } => {
            client.call(Request::Kill { id: id.clone() }).await?;
            println!("killed {id}");
        }
        Cmd::Rm { id, prune } => {
            client
                .call(Request::Remove {
                    id: id.clone(),
                    prune_worktree: prune,
                })
                .await?;
            println!(
                "removed {id}{}",
                if prune {
                    ", its worktree and snapshots (and its branch, if merged)"
                } else {
                    ""
                }
            );
        }
        Cmd::Inbox => {
            let Response::Tasks(tasks) = client.call(Request::ListTasks).await? else {
                bail!("unexpected reply")
            };
            let Response::Approvals(apps) = client.call(Request::Approvals).await? else {
                bail!("unexpected reply")
            };
            let needy: Vec<_> = tasks.into_iter().filter(|t| t.attention).collect();
            if needy.is_empty() && apps.is_empty() {
                println!("inbox zero ✨");
            }
            if !apps.is_empty() {
                println!("\x1b[1mApprovals\x1b[0m");
                for a in &apps {
                    let from = match &a.source {
                        ApprovalSource::Task { task_id, tool } => format!("{task_id} ({tool})"),
                        ApprovalSource::Ops { session, hosts } => {
                            format!("ops #{session} on {}", hosts.join(","))
                        }
                    };
                    println!(
                        "  #{:<4} {} {}  \x1b[90m{} · {} ago\x1b[0m",
                        a.id,
                        tier_badge(a.tier),
                        a.command,
                        from,
                        ago(a.created_unix)
                    );
                }
                println!("  → pitwall approve <id> | pitwall deny <id>");
            }
            if !needy.is_empty() {
                println!("\x1b[1mTasks\x1b[0m");
                for t in &needy {
                    let why = match &t.status {
                        TaskStatus::Waiting { reason } => reason.clone(),
                        TaskStatus::Idle => "finished its turn".into(),
                        TaskStatus::Exited { .. } => t.status.label(),
                        TaskStatus::Running => "has pending approvals".into(),
                    };
                    println!("  {}  {}  {}", pad(&t.id, 18), status_badge(&t.status), why);
                }
                println!("  → pitwall attach <id>");
            }
        }
        Cmd::Approve { id } => {
            client.call(Request::Decide { id, allow: true }).await?;
            println!("approved #{id}");
        }
        Cmd::Deny { id } => {
            client.call(Request::Decide { id, allow: false }).await?;
            println!("denied #{id}");
        }
        Cmd::Auto { id, state } => {
            let on = match state.as_str() {
                "on" => true,
                "off" => false,
                _ => bail!("auto mode is `on` or `off`"),
            };
            client.call(Request::SetAuto { id: id.clone(), on }).await?;
            println!("auto mode {state} for {id}");
        }
        Cmd::Snap { id, label } => match client
            .call(Request::TakeSnapshot {
                id,
                label: label.join(" "),
            })
            .await?
        {
            Response::Snapshot(Some(s)) => {
                println!("snapshot #{} {} — {}", s.seq, s.short(), s.label)
            }
            _ => println!("nothing changed since the last snapshot"),
        },
        Cmd::Snaps { id } => {
            let Response::Snapshots(list) = client.call(Request::Snapshots { id }).await? else {
                bail!("unexpected reply")
            };
            if list.is_empty() {
                println!("no snapshots");
            }
            for s in list {
                println!(
                    "#{:<4} {}  {:>5} ago  \x1b[32m+{}\x1b[0m \x1b[31m-{}\x1b[0m {:>3} files  {}",
                    s.seq,
                    s.short(),
                    ago(s.unix),
                    s.insertions,
                    s.deletions,
                    s.files_changed,
                    s.label
                );
            }
        }
        Cmd::Diff { id, from, to } => {
            let Response::Diff(text) = client.call(Request::Diff { id, from, to }).await? else {
                bail!("unexpected reply")
            };
            print!("{text}");
        }
        Cmd::Revert { id, snapshot } => {
            let Response::Snapshot(Some(s)) = client.call(Request::Revert { id, snapshot }).await?
            else {
                bail!("revert failed")
            };
            println!(
                "{} (undo with `pitwall revert <task> #{}`)",
                s.label,
                s.seq.saturating_sub(1)
            );
        }
        Cmd::Hosts => {
            let Response::Hosts(hosts) = client.call(Request::Hosts).await? else {
                bail!("unexpected reply")
            };
            if hosts.is_empty() {
                println!(
                    "no hosts configured — add [[hosts]] to {}",
                    paths::config().display()
                );
            }
            for h in hosts {
                println!(
                    "{}  {}  {}  [{}]",
                    pad(&h.name, 16),
                    pad(&h.addr, 24),
                    pad(h.user.as_deref().unwrap_or("-"), 10),
                    h.tags.join(", ")
                );
            }
        }
        Cmd::Run {
            selector,
            yes,
            command,
        } => {
            let command = command.join(" ");
            let mut resp = client
                .call(Request::FleetRun {
                    selector: selector.clone(),
                    command: command.clone(),
                    confirmed: false,
                })
                .await?;
            if let Response::NeedsConfirm {
                tier,
                decision,
                reason,
                hosts,
            } = &resp
            {
                println!(
                    "{} on {} host(s): {}  \x1b[90m({reason})\x1b[0m",
                    tier_badge(*tier),
                    hosts.len(),
                    hosts.join(", ")
                );
                let ok = if *tier == Tier::Destructive || *decision == Decision::Deny {
                    prompt_line(&format!(
                        "policy says {}. Type the number of hosts ({}) to run anyway: ",
                        decision_word(*decision),
                        hosts.len()
                    ))
                    .await?
                        == hosts.len().to_string()
                } else {
                    yes || matches!(
                        prompt_line("run it? [y/N] ").await?.as_str(),
                        "y" | "Y" | "yes"
                    )
                };
                if !ok {
                    println!("cancelled");
                    return Ok(());
                }
                resp = client
                    .call(Request::FleetRun {
                        selector,
                        command,
                        confirmed: true,
                    })
                    .await?;
            }
            let Response::Fleet(results) = resp else {
                bail!("unexpected reply")
            };
            print_results(&results);
        }
        Cmd::Ask { question, hosts } => {
            return ask(client, pushes, question.join(" "), hosts).await
        }
        Cmd::Daemon
        | Cmd::Stop
        | Cmd::Init { .. }
        | Cmd::Classify { .. }
        | Cmd::Hook { .. }
        | Cmd::Guide => {
            unreachable!()
        }
    }
    Ok(())
}

async fn ask(
    client: Client,
    mut pushes: UnboundedReceiver<Push>,
    question: String,
    hosts: Option<String>,
) -> Result<()> {
    if question.trim().is_empty() {
        bail!("ask what? e.g. pitwall ask \"why is prod-3 returning 502s?\"");
    }
    client.call(Request::Subscribe).await?;
    let Response::OpsStarted { session } = client
        .call(Request::OpsAsk {
            question,
            selector: hosts,
        })
        .await?
    else {
        bail!("unexpected reply")
    };
    println!("\x1b[90minvestigation #{session} started — approvals for mutating commands will be asked here\x1b[0m");
    while let Some(p) = pushes.recv().await {
        match p {
            Push::Ops(e) if e.session() == session => match e {
                OpsEvent::Started { .. } => {}
                OpsEvent::Thinking { text, .. } => println!("\n\x1b[36m{}\x1b[0m", text.trim()),
                OpsEvent::Command {
                    hosts,
                    command,
                    tier,
                    why,
                    ..
                } => {
                    println!(
                        "\n\x1b[1m$ {command}\x1b[0m  \x1b[90mon {} · {}\x1b[0m {}",
                        hosts.join(","),
                        why,
                        tier_badge(tier)
                    )
                }
                OpsEvent::Results { results, .. } => {
                    for r in results {
                        let lines: Vec<&str> = r.stdout.lines().collect();
                        let shown = lines.len().min(8);
                        let status = r.error.clone().unwrap_or_else(|| {
                            format!(
                                "exit {}",
                                r.exit.map(|c| c.to_string()).unwrap_or("?".into())
                            )
                        });
                        println!("  \x1b[90m{} ({status}):\x1b[0m", r.host);
                        for l in &lines[..shown] {
                            println!("    {l}");
                        }
                        if lines.len() > shown {
                            println!("    \x1b[90m… {} more lines\x1b[0m", lines.len() - shown);
                        }
                    }
                }
                OpsEvent::Blocked {
                    command, reason, ..
                } => println!("  \x1b[31m✕ {command}: {reason}\x1b[0m"),
                OpsEvent::Finished {
                    answer, runbook, ..
                } => {
                    println!("\n{answer}");
                    if let Some(p) = runbook {
                        println!("\n\x1b[90mrunbook saved to {p}\x1b[0m");
                    }
                    return Ok(());
                }
                OpsEvent::Failed { error, .. } => return Err(anyhow!(error)),
            },
            Push::ApprovalAdded(a) => {
                if let ApprovalSource::Ops { session: s, hosts } = &a.source {
                    if *s == session {
                        let ans = prompt_line(&format!(
                            "\x1b[33m? approve {} `{}` on {}? [y/N] \x1b[0m",
                            a.tier.label(),
                            a.command,
                            hosts.join(",")
                        ))
                        .await?;
                        client
                            .call(Request::Decide {
                                id: a.id,
                                allow: matches!(ans.as_str(), "y" | "Y" | "yes"),
                            })
                            .await?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
