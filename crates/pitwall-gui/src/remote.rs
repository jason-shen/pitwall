//! SSH hosts: the list in the sidebar, interactive sessions opened as
//! terminal cards, and commands run across hosts at once.

use std::time::{Duration, Instant};

use pitwall_core::config::HostConfig;
use pitwall_core::proto::{HostResult, NewTask, Request, Response, Tier};

use crate::app::{App, Reply, Target};
use crate::i18n::trf;

/// The project SSH sessions are grouped under. Not a path, so it never
/// collides with a real project.
pub const SSH_PROJECT: &str = "ssh:";

/// Sessions of a "Connect all" still to arrive, and whether the first one
/// (which clears the stage) has come in.
#[derive(Default)]
pub struct Batch {
    pub left: usize,
    pub started: bool,
}

#[derive(Default)]
pub struct FleetUi {
    pub selector: String,
    pub command: String,
    pub running: bool,
    /// The policy wants a human to look before this runs.
    pub confirm: Option<(Tier, String, Vec<String>)>,
    pub results: Vec<HostResult>,
    pub error: Option<String>,
}

pub enum FleetReply {
    Done(Vec<HostResult>),
    NeedsConfirm(Tier, String, Vec<String>),
    Failed(String),
}

impl App {
    /// Hosts come from config.toml and ~/.ssh/config, both of which can
    /// change under us; rereading two small files every few seconds is cheap.
    pub fn refresh_hosts(&mut self, force: bool) {
        if !force && self.hosts_read.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.hosts_read = Instant::now();
        self.hosts = pitwall_core::hosts::current(&self.cfg.hosts);
    }

    /// Live sessions to `host`, oldest first.
    pub fn sessions_of(&self, host: &str) -> Vec<String> {
        let mut v: Vec<_> = self
            .tasks
            .values()
            .filter(|t| t.host.as_deref() == Some(host))
            .collect();
        v.sort_by_key(|t| t.created_unix);
        v.into_iter().map(|t| t.id.clone()).collect()
    }

    pub fn connect(&mut self, name: &str) {
        let Some(h) = self.hosts.iter().find(|h| h.name == name).cloned() else {
            return self.notify(trf!("no remote named {name}", name = name), true);
        };
        self.start_session(&h);
    }

    /// Every host the selector matches, tiled side by side as keel's
    /// Connect all does.
    pub fn connect_all(&mut self, selector: &str) {
        let hosts: Vec<HostConfig> = pitwall_core::hosts::select(&self.hosts, selector)
            .into_iter()
            .cloned()
            .collect();
        if hosts.is_empty() {
            return self.notify(
                trf!("{selector} matches no remotes", selector = selector),
                true,
            );
        }
        self.ssh_batch = Batch {
            left: hosts.len(),
            started: false,
        };
        for h in &hosts {
            self.start_session(h);
        }
    }

    fn start_session(&mut self, h: &HostConfig) {
        // A daemon from before SSH sessions ignores `host` and would start
        // $SHELL with ssh's arguments.
        if self.daemon_outdated {
            return self.notify(
                crate::i18n::tr("Restart the daemon (sidebar banner) to open remote sessions"),
                true,
            );
        }
        let (rows, cols) = self.pane_grid();
        let home = dirs_home();
        let req = NewTask {
            name: h.name.clone(),
            agent: "shell".into(),
            no_worktree: true,
            cwd: Some(home),
            repo: None,
            base: None,
            args: pitwall_core::hosts::ssh_args(h),
            prompt: None,
            rows,
            cols,
            brief: None,
            host: Some(h.name.clone()),
        };
        self.call(Request::NewTask(req), move |r| match r {
            Ok(Response::Task(t)) => Some(Reply::Created(t, true)),
            Err(e) => Some(Reply::CreateFailed(e)),
            _ => None,
        });
    }

    /// Where a new session's card goes: the first of a Connect all clears
    /// the stage, the rest tile beside it.
    pub fn place_session(&mut self, id: &str) {
        let host = self.tasks.get(id).and_then(|t| t.host.clone());
        if let Some(i) = self
            .ssh_reconnects
            .iter()
            .position(|(h, _)| Some(h) == host.as_ref())
        {
            let (_, old) = self.ssh_reconnects.remove(i);
            if let Some(c) = self.cards.iter().position(|c| c.item == old) {
                self.item_mut(id);
                self.cards[c].item = id.to_string();
                self.active = c;
                self.on_active_changed();
            } else {
                self.open_item(id);
            }
            if let Some(t) = self.tabs.iter().position(|t| t.id == old) {
                self.close_tab(t, true);
            }
            return;
        }
        if self.ssh_batch.left == 0 {
            return self.open_item(id);
        }
        self.ssh_batch.left -= 1;
        if !std::mem::replace(&mut self.ssh_batch.started, true) {
            self.project = Some(SSH_PROJECT.into());
            self.cards.clear();
            self.active = 0;
            self.zoom = false;
            self.show(Target {
                item: id.to_string(),
                child: crate::app::Child::Terminal,
            });
        } else {
            self.open_beside(id);
        }
    }

    /// A new session in place of one that ended.
    pub fn reconnect(&mut self, old: &str) {
        let Some(host) = self.tasks.get(old).and_then(|t| t.host.clone()) else {
            return;
        };
        let Some(h) = self.hosts.iter().find(|h| h.name == host).cloned() else {
            return self.notify(trf!("{host} is no longer configured", host = self.remote_name(&host)), true);
        };
        self.ssh_reconnects.push((host, old.to_string()));
        self.start_session(&h);
    }

    pub fn open_fleet(&mut self, selector: &str) {
        let f = self.fleet.get_or_insert_with(FleetUi::default);
        f.selector = selector.to_string();
        f.confirm = None;
        f.error = None;
    }

    pub fn run_fleet(&mut self, confirmed: bool) {
        let Some(f) = self.fleet.as_mut() else {
            return;
        };
        let command = f.command.trim().to_string();
        if command.is_empty() || f.running {
            return;
        }
        f.running = true;
        f.confirm = None;
        f.error = None;
        let req = Request::FleetRun {
            selector: f.selector.trim().to_string(),
            command,
            confirmed,
        };
        self.call(req, |r| {
            Some(Reply::Fleet(match r {
                Ok(Response::Fleet(v)) => FleetReply::Done(v),
                Ok(Response::NeedsConfirm {
                    tier, reason, hosts, ..
                }) => FleetReply::NeedsConfirm(tier, reason, hosts),
                Ok(Response::Error(e)) | Err(e) => FleetReply::Failed(e),
                Ok(_) => FleetReply::Failed("unexpected reply from the daemon".into()),
            }))
        });
    }

    pub fn on_fleet(&mut self, r: FleetReply) {
        let Some(f) = self.fleet.as_mut() else {
            return;
        };
        f.running = false;
        match r {
            FleetReply::Done(v) => f.results = v,
            FleetReply::NeedsConfirm(tier, reason, hosts) => f.confirm = Some((tier, reason, hosts)),
            FleetReply::Failed(e) => f.error = Some(e),
        }
    }
}

fn dirs_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}
