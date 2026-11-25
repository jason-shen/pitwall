//! The Tiller tab: machines running tillerd, with their tmux sessions opened
//! as terminal cards. A card runs `pitwall tiller-pane`, the way an SSH
//! session card runs `ssh`, so the daemon treats it as any other terminal.

use std::collections::HashMap;
use std::path::PathBuf;

use pitwall_core::proto::{NewTask, Request, Response};
use pitwall_core::tiller::{self, Link, Session, TillerHost};
use serde_json::json;

use crate::app::{App, Reply};
use crate::net::Ev;
use crate::i18n::{tr, trf};

/// The pseudo-project Tiller cards belong to, as SSH sessions have "ssh:".
pub const TILLER_PROJECT: &str = "tiller:";

#[derive(Default)]
pub struct TillerUi {
    pub hosts: HashMap<String, HostView>,
    /// Pairing JSON being pasted, while the add form is open.
    pub form: Option<String>,
    /// A new session's name being typed, per host.
    pub new_session: HashMap<String, String>,
}

#[derive(Default, Clone)]
pub struct HostView {
    pub loading: bool,
    pub error: Option<String>,
    pub sessions: Vec<Session>,
    pub loaded: bool,
}

/// Where a host's cards run. They need a folder of their own: it's how a
/// card is recognised as a Tiller card after a restart.
fn tiller_root() -> PathBuf {
    pitwall_core::paths::home().join("tiller")
}

fn card_dir(host: &str) -> PathBuf {
    let d = tiller_root().join(pitwall_core::tiller::slug(host));
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn is_tiller_dir(dir: &str) -> bool {
    std::path::Path::new(dir).starts_with(tiller_root())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl App {
    fn tiller_host(&self, name: &str) -> Option<TillerHost> {
        self.settings.tiller_hosts.iter().find(|h| h.name == name).cloned()
    }

    /// Fetches a host's sessions in the background.
    pub fn tiller_refresh(&mut self, name: &str) {
        let Some(h) = self.tiller_host(name) else { return };
        let v = self.tiller.hosts.entry(name.to_string()).or_default();
        if v.loading {
            return;
        }
        v.loading = true;
        let (tx, ctx, host) = (self.tx.clone(), self.ctx.clone(), name.to_string());
        self.rt.spawn(async move {
            // Batch mode: there's no terminal here for ssh to ask in.
            let r = async {
                let (link, _rx) = Link::connect_ssh(&h, true).await?;
                tiller::sessions(&link).await
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Ev::Reply(Reply::TillerSessions { host, result: r }));
            ctx.request_repaint();
        });
    }

    pub fn tiller_sessions_loaded(&mut self, host: String, result: Result<Vec<Session>, String>) {
        let v = self.tiller.hosts.entry(host).or_default();
        v.loading = false;
        v.loaded = true;
        match result {
            Ok(s) => {
                v.sessions = s;
                v.error = None;
            }
            Err(e) => v.error = Some(e),
        }
    }

    /// A card showing a session's active pane.
    pub fn tiller_open(&mut self, host: &str, session: &str) {
        let Some(h) = self.tiller_host(host) else { return };
        let mut cmd = vec![
            sh_quote(&crate::net::pitwall_binary().display().to_string()),
            "tiller-pane".into(),
            "--name".into(),
            sh_quote(&h.name),
            "--session".into(),
            sh_quote(session),
        ];
        if h.local {
            cmd.push("--local".into());
        } else {
            cmd.extend(["--ssh".into(), sh_quote(&h.ssh_target()), "--port".into(), h.port().to_string()]);
        }
        if let Some(helper) = h.helper.as_deref().filter(|s| !s.trim().is_empty()) {
            cmd.extend(["--helper".into(), sh_quote(helper)]);
        }
        let (rows, cols) = self.pane_grid();
        let req = NewTask {
            name: format!("{session} · {}", h.name),
            agent: "shell".into(),
            no_worktree: true,
            cwd: Some(card_dir(&h.name).display().to_string()),
            args: vec!["-c".into(), format!("exec {}", cmd.join(" "))],
            rows,
            cols,
            ..Default::default()
        };
        self.call(Request::NewTask(req), move |r| match r {
            Ok(Response::Task(t)) => Some(Reply::TillerCard(t)),
            Err(e) => Some(Reply::CreateFailed(e)),
            _ => None,
        });
    }

    /// Starts a tmux session on the host and opens it.
    pub fn tiller_new_session(&mut self, host: &str) {
        let Some(h) = self.tiller_host(host) else { return };
        let name = self.tiller.new_session.remove(host).unwrap_or_default().trim().to_string();
        if name.is_empty() {
            return;
        }
        let (tx, ctx, hname) = (self.tx.clone(), self.ctx.clone(), host.to_string());
        self.rt.spawn(async move {
            let r = async {
                let (link, _rx) = Link::connect_ssh(&h, true).await?;
                link.call("sessions.new", json!({"name": name})).await?;
                anyhow::Ok(name)
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Ev::Reply(Reply::TillerSessionStarted { host: hname, result: r }));
            ctx.request_repaint();
        });
    }

    pub fn tiller_session_started(&mut self, host: String, result: Result<String, String>) {
        match result {
            Ok(session) => {
                self.tiller_open(&host, &session);
                self.tiller.hosts.entry(host.clone()).or_default().loading = false;
                self.tiller_refresh(&host);
            }
            Err(e) => self.notify(e, true),
        }
    }

    /// Adds the host from the pasted pairing code, or updates it if it's
    /// paired already.
    pub fn tiller_add_from_form(&mut self) {
        let text = self.tiller.form.clone().unwrap_or_default();
        match tiller::parse_pairing(&text) {
            Ok(h) => {
                self.tiller.form = None;
                self.tiller_save_host(h);
            }
            Err(e) => self.notify(format!("{e:#}"), true),
        }
    }

    /// tillerd on this machine, reached without SSH.
    pub fn tiller_add_local(&mut self) {
        let name = hostname();
        self.tiller_save_host(TillerHost { name, local: true, ..Default::default() });
    }

    fn tiller_save_host(&mut self, h: TillerHost) {
        let hosts = &mut self.settings.tiller_hosts;
        let same = |x: &TillerHost| {
            (!h.host_id.is_empty() && x.host_id == h.host_id) || (h.local && x.local) || x.name == h.name
        };
        let name = h.name.clone();
        match hosts.iter_mut().find(|x| same(x)) {
            Some(x) => {
                *x = h;
                self.notify(trf!("Updated {name}", name = name), false);
            }
            None => {
                hosts.push(h);
                self.notify(trf!("Added {name}", name = name), false);
            }
        }
        self.tiller_refresh(&name);
    }

    pub fn tiller_remove(&mut self, name: &str) {
        self.settings.tiller_hosts.retain(|h| h.name != name);
        self.tiller.hosts.remove(name);
        self.notify(tr("Removed. Its open cards keep running until you close them."), false);
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "This machine".into())
}
