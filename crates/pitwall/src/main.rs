//! pitwall — an agent multiplexer for the terminal.
//!
//! One binary: `pitwall daemon` runs the supervisor; every other subcommand is a
//! client that talks to it over `~/.pitwall/pitwall.sock` (auto-starting it).

mod cli;
mod daemon;
mod tui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "pitwall",
    version,
    about = "Agent-first terminal workspace: run coding agents side by side, watch them, edit, diff and undo alongside them. Run with no arguments for the TUI."
)]
struct Cli {
    /// No subcommand: open the TUI.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground (normally auto-started).
    Daemon,
    /// Stop the daemon and every task it runs.
    Stop,
    /// Write the default config to ~/.pitwall/config.toml.
    Init {
        #[arg(long)]
        force: bool,
    },

    /// Start an agent task. Inside a git repo it gets its own worktree + branch.
    New {
        /// Task name (becomes the id and the branch `pitwall/<name>`).
        name: String,
        /// Agent from [agents] in the config.
        #[arg(short, long, default_value = "claude")]
        agent: String,
        /// Repo to branch from (default: the current directory if it is a git repo).
        #[arg(short, long)]
        repo: Option<String>,
        /// Base ref for the new branch.
        #[arg(short, long)]
        base: Option<String>,
        /// Run in place instead of creating a worktree.
        #[arg(long)]
        here: bool,
        /// Initial prompt.
        #[arg(short, long)]
        prompt: Option<String>,
        /// Attach right away.
        #[arg(long)]
        attach: bool,
        /// Extra arguments for the agent command (after `--`).
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// List tasks.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Attach your terminal to a task (detach with Ctrl-]).
    Attach { id: String },
    /// Type text into a task (Enter is sent unless --no-enter).
    Send {
        id: String,
        text: Vec<String>,
        #[arg(long)]
        no_enter: bool,
    },
    /// Agents and terminals, for agents finding each other: id, status, folder, what it's doing.
    Agents {
        #[arg(long)]
        json: bool,
    },
    /// Message an agent: typed into its prompt and submitted. From inside an
    /// agent, it's told who sent it and how to reply.
    Tell { id: String, text: Vec<String> },
    /// The brief this agent was handed: its steps, numbered.
    /// `pitwall brief tick 2` marks step 2 done; `untick` undoes it.
    Brief {
        #[arg(value_parser = ["tick", "untick"])]
        action: Option<String>,
        step: Option<usize>,
        /// The task (default: the agent this runs in).
        #[arg(long)]
        id: Option<String>,
    },
    /// Print the last lines of a task's screen.
    Read {
        id: String,
        #[arg(short = 'n', long, default_value_t = 40)]
        lines: usize,
    },
    /// Wait until an agent finishes its turn (idle, waiting on you, or exited).
    Wait {
        id: String,
        /// Give up after this many seconds.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// How agents work together in pitwall (what Claude Code agents are told).
    Guide,

    /// Kill a task's process (keeps worktree and snapshots).
    Kill { id: String },
    /// Remove a task. --prune also deletes its worktree and snapshots, and its branch if it's merged.
    Rm {
        id: String,
        #[arg(long)]
        prune: bool,
    },

    /// Everything waiting on you: tasks needing attention and pending approvals.
    Inbox,
    /// Approve a pending command.
    Approve { id: u64 },
    /// Deny a pending command.
    Deny { id: u64 },
    /// Auto mode for a task: `on` approves what pitwall would ask about
    /// (deny rules still hold), `off` asks again.
    Auto { id: String, state: String },

    /// Take a snapshot of a task's working tree now.
    Snap { id: String, label: Vec<String> },
    /// List a task's snapshots (newest first).
    Snaps { id: String },
    /// Diff a task: everything since start, or between snapshots (`#3`, `<sha>`, `#5^`).
    Diff {
        id: String,
        from: Option<String>,
        to: Option<String>,
    },
    /// Restore a task's files to a snapshot (a safety snapshot is taken first).
    Revert { id: String, snapshot: String },

    /// List configured hosts.
    Hosts,
    /// Run a command across hosts: `pitwall run tag:prod -- uptime`.
    Run {
        selector: String,
        /// Skip the confirmation prompt for mutating commands.
        #[arg(short = 'y', long)]
        yes: bool,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Ask an LLM to investigate across your fleet: `pitwall ask "why is prod-3 returning 502s?"`.
    Ask {
        question: Vec<String>,
        /// Restrict to a host selector.
        #[arg(long)]
        hosts: Option<String>,
    },
    /// Show how the policy classifies a command.
    Classify { command: Vec<String> },

    /// Hook entry point used by agents (e.g. `pitwall hook claude`). Reads JSON on stdin.
    #[command(hide = true)]
    Hook { kind: String },
}

fn main() {
    pitwall_core::rename::migrate();
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let res = rt.block_on(async move {
        let Some(cmd) = cli.cmd else {
            return tui::run().await;
        };
        match cmd {
            Cmd::Daemon => {
                tracing_subscriber::fmt()
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "info".into()),
                    )
                    .with_target(false)
                    .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
                    .init();
                daemon::run().await
            }
            Cmd::Hook { kind } => cli::hook(&kind).await,
            Cmd::Init { force } => cli::init(force),
            Cmd::Classify { command } => cli::classify(&command.join(" ")),
            Cmd::Stop => cli::stop().await,
            Cmd::Guide => {
                println!("{}", pitwall_core::agents::GUIDE);
                Ok(())
            }
            other => cli::dispatch(other).await,
        }
    });
    if let Err(e) = res {
        eprintln!("pitwall: {e:#}");
        std::process::exit(1);
    }
}
