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
        let hidden = &self.settings.hidden_remotes;
        self.hosts = pitwall_core::hosts::current(&self.cfg.hosts)
            .into_iter()
            .filter(|h| !(from_ssh_config(h) && hidden.contains(&h.name)))
            .collect();
    }

    /// The group `host` is listed under; None is Ungrouped.
    pub fn group_of(&self, host: &str) -> Option<&str> {
        self.settings
            .remote_group
            .get(host)
            .map(String::as_str)
            .filter(|g| self.settings.remote_groups.iter().any(|x| x == g))
    }

    /// The remotes in a group (None: Ungrouped), in config order.
    pub fn group_members(&self, group: Option<&str>) -> Vec<HostConfig> {
        self.hosts
            .iter()
            .filter(|h| self.group_of(&h.name) == group)
            .cloned()
            .collect()
    }

    /// A selector for every remote in a group.
    pub fn group_selector(&self, group: Option<&str>) -> String {
        self.group_members(group)
            .iter()
            .map(|h| h.name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// A new, empty group, with its name up for editing.
    pub fn new_group(&mut self) {
        let mut name = crate::i18n::tr("New group").to_string();
        let mut n = 2;
        while self.settings.remote_groups.contains(&name) {
            name = format!("{} {n}", crate::i18n::tr("New group"));
            n += 1;
        }
        self.settings.remote_groups.push(name.clone());
        self.renaming_group = Some((name.clone(), name));
    }

    pub fn rename_group(&mut self, old: &str, new: &str) {
        self.renaming_group = None;
        let new = new.trim();
        if new.is_empty() || new == old {
            return;
        }
        if self.settings.remote_groups.iter().any(|g| g == new) {
            return self.notify(trf!("There's already a group named {name}", name = new), true);
        }
        let s = &mut self.settings;
        for g in s.remote_groups.iter_mut().filter(|g| *g == old) {
            *g = new.to_string();
        }
        for g in s.remote_group.values_mut().filter(|g| *g == old) {
            *g = new.to_string();
        }
        if s.folded_remote_groups.remove(old) {
            s.folded_remote_groups.insert(new.to_string());
        }
    }

    /// Its remotes go back to Ungrouped.
    pub fn delete_group(&mut self, name: &str) {
        let s = &mut self.settings;
        s.remote_groups.retain(|g| g != name);
        s.remote_group.retain(|_, g| g != name);
        s.folded_remote_groups.remove(name);
    }

    pub fn move_remote(&mut self, host: &str, group: Option<&str>) {
        match group {
            Some(g) => {
                self.settings
                    .remote_group
                    .insert(host.to_string(), g.to_string());
                // Unfold it so the remote is seen arriving.
                self.settings.folded_remote_groups.remove(g);
            }
            None => {
                self.settings.remote_group.remove(host);
                self.settings.folded_remote_groups.remove("");
            }
        }
    }

    pub fn ask_remove_remote(&mut self, name: &str) {
        let Some(h) = self.hosts.iter().find(|h| h.name == name) else {
            return;
        };
        let shown = self.remote_name(name);
        let mut text = if from_ssh_config(h) {
            trf!(
                "Remove {name} from pitwall? It stays in ~/.ssh/config; only pitwall stops listing it.",
                name = shown
            )
        } else {
            trf!("Remove {name}? Its entry comes out of config.toml.", name = shown)
        };
        let open = self.sessions_of(name).len();
        if open > 0 {
            text.push(' ');
            text.push_str(&trf!("Its {n} open sessions close.", n = open));
        }
        self.confirm = Some(crate::app::Confirm::RemoveRemote {
            name: name.to_string(),
            text,
        });
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

/// The add / edit remote dialog.
#[derive(Default)]
pub struct RemoteForm {
    /// The config name of the remote being edited; None adds one.
    pub editing: Option<String>,
    pub name: String,
    pub addr: String,
    pub user: String,
    pub port: String,
    pub key: String,
    pub tags: String,
    /// "" is Ungrouped.
    pub group: String,
    pub error: Option<String>,
}

impl RemoteForm {
    fn from_host(h: &HostConfig) -> RemoteForm {
        RemoteForm {
            editing: Some(h.name.clone()),
            name: h.name.clone(),
            addr: h.addr.clone().unwrap_or_default(),
            user: h.user.clone().unwrap_or_default(),
            port: h.port.map(|p| p.to_string()).unwrap_or_default(),
            key: h.key.clone().unwrap_or_default(),
            tags: h.tags.join(", "),
            group: String::new(),
            error: None,
        }
    }

    /// The remote as typed, or what's wrong with it.
    fn host(&self, taken: &[HostConfig]) -> Result<HostConfig, String> {
        let addr = self.addr.trim();
        if addr.is_empty() {
            return Err(crate::i18n::tr("An address is needed: an IP or a hostname").into());
        }
        if addr.contains(char::is_whitespace) {
            return Err(crate::i18n::tr("The address can't contain spaces").into());
        }
        // Without a name it goes by its address.
        let name = match self.name.trim() {
            "" => addr.to_string(),
            n => n.to_string(),
        };
        let clash = taken.iter().any(|h| {
            h.name == name
                && Some(&h.name) != self.editing.as_ref()
                && !h.tags.iter().any(|t| t == "ssh-config")
        });
        if clash {
            return Err(trf!("There's already a remote named {name}", name = name));
        }
        let port = match self.port.trim() {
            "" => None,
            p => Some(
                p.parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or_else(|| trf!("{port} isn't a port number", port = p))?,
            ),
        };
        let opt = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        Ok(HostConfig {
            addr: (addr != name).then(|| addr.to_string()),
            name,
            port,
            user: opt(&self.user),
            key: opt(&self.key),
            tags: self
                .tags
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
            hostname: None,
        })
    }
}

impl App {
    /// `group` is where a new remote goes; an edited one shows its own.
    pub fn open_remote_form(&mut self, editing: Option<&str>, group: Option<&str>) {
        let mut f = editing
            .and_then(|n| self.hosts.iter().find(|h| h.name == n))
            .map(RemoteForm::from_host)
            .unwrap_or_default();
        f.group = match editing {
            Some(n) => self.group_of(n),
            None => group,
        }
        .unwrap_or_default()
        .to_string();
        self.remote_form = Some(f);
    }

    pub fn save_remote_form(&mut self, connect: bool) {
        let Some(f) = self.remote_form.as_mut() else {
            return;
        };
        let h = match f.host(&self.hosts) {
            Ok(h) => h,
            Err(e) => {
                f.error = Some(e);
                return;
            }
        };
        let old = f.editing.clone();
        let group = Some(f.group.clone()).filter(|g| !g.is_empty());
        if let Err(e) = edit_config_hosts(|list| {
            let at = old
                .as_ref()
                .and_then(|o| list.iter().position(|t| entry_name(t) == Some(o.as_str())));
            match at {
                Some(i) => fill_entry(&mut list[i], &h),
                None => {
                    let mut t = toml_edit::InlineTable::new();
                    fill_entry(&mut t, &h);
                    list.push(t);
                }
            }
        }) {
            f.error = Some(e);
            return;
        }
        self.remote_form = None;
        // A name you gave it follows it to its new config name.
        if let Some(o) = old.filter(|o| *o != h.name) {
            if let Some(n) = self.settings.remote_names.remove(&o) {
                self.settings.remote_names.insert(h.name.clone(), n);
            }
            self.settings.remote_group.remove(&o);
        }
        self.move_remote(&h.name, group.as_deref());
        self.refresh_hosts(true);
        if connect {
            self.connect(&h.name);
        }
    }

    pub fn remove_remote(&mut self, name: &str) {
        let ssh_config = self
            .hosts
            .iter()
            .find(|h| h.name == name)
            .is_some_and(from_ssh_config);
        if ssh_config {
            self.settings.hidden_remotes.insert(name.to_string());
        } else if let Err(e) =
            edit_config_hosts(|list| list.retain(|t| entry_name(t) != Some(name)))
        {
            return self.notify(e, true);
        }
        for id in self.sessions_of(name) {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                self.close_tab(i, true);
            }
        }
        self.remote_form = None;
        self.settings.remote_names.remove(name);
        self.settings.remote_group.remove(name);
        self.refresh_hosts(true);
    }
}

fn entry_name(t: &toml_edit::InlineTable) -> Option<&str> {
    t.get("name").and_then(|v| v.as_str())
}

/// Write `h` into a hosts entry, leaving keys the form doesn't know about.
fn fill_entry(t: &mut toml_edit::InlineTable, h: &HostConfig) {
    let mut set = |k: &str, v: Option<toml_edit::Value>| match v {
        Some(v) => {
            t.insert(k, v);
        }
        None => {
            t.remove(k);
        }
    };
    set("name", Some(h.name.clone().into()));
    set("addr", h.addr.clone().map(Into::into));
    set("port", h.port.map(|p| i64::from(p).into()));
    set("user", h.user.clone().map(Into::into));
    set("key", h.key.clone().map(Into::into));
    set(
        "tags",
        (!h.tags.is_empty()).then(|| toml_edit::Array::from_iter(h.tags.iter()).into()),
    );
    t.fmt();
}

/// Rewrite config.toml's top-level `hosts`, one entry per line, leaving the
/// rest of the file (comments, the daemon's sections, `[app]`) as it was.
/// Hosts stranded under `[ops]` by the old default config move up with it.
fn edit_config_hosts(f: impl FnOnce(&mut Vec<toml_edit::InlineTable>)) -> Result<(), String> {
    use toml_edit::{Item, Value};
    let path = pitwall_core::paths::config();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| {
            trf!("config.toml doesn't parse, so it was left alone: {e}", e = e)
        })?;
    let mut list: Vec<toml_edit::InlineTable> = vec![];
    match doc.get("hosts") {
        Some(Item::Value(Value::Array(a))) => {
            list.extend(a.iter().filter_map(|v| v.as_inline_table().cloned()))
        }
        Some(Item::ArrayOfTables(a)) => {
            list.extend(a.iter().map(|t| t.clone().into_inline_table()))
        }
        _ => {}
    }
    let stray = doc
        .get_mut("ops")
        .and_then(|o| o.as_table_mut())
        .filter(|o| o.get("hosts").and_then(|h| h.as_array()).is_some_and(|a| !a.is_empty()))
        .and_then(|o| o.remove("hosts"));
    if let Some(Item::Value(Value::Array(a))) = stray {
        list.extend(a.iter().filter_map(|v| v.as_inline_table().cloned()));
    }
    f(&mut list);
    let mut arr = toml_edit::Array::new();
    for t in list {
        arr.push_formatted(Value::InlineTable(t).decorated("\n  ", ""));
    }
    if !arr.is_empty() {
        arr.set_trailing("\n");
        arr.set_trailing_comma(true);
    }
    match doc.get_mut("hosts") {
        // In place, so the comments above it stay put.
        Some(item) => *item = Item::Value(Value::Array(arr)),
        None => {
            // A new top-level key is written above every table, and the
            // file's opening comment is stored as the first table's prefix;
            // move that comment up so it still opens the file.
            // Only its first paragraph: comments after a blank line belong to
            // the section below.
            let lead: String = text
                .lines()
                .take_while(|l| l.trim_start().starts_with('#'))
                .map(|l| format!("{l}\n"))
                .collect();
            let moved = !lead.is_empty() && unprefix(doc.as_table_mut(), &lead);
            let ours = "# Remotes, added in the app or by hand.\n";
            doc.insert("hosts", Item::Value(Value::Array(arr)));
            if let Some(mut k) = doc.key_mut("hosts") {
                k.leaf_decor_mut().set_prefix(if moved {
                    format!("{lead}\n{ours}")
                } else {
                    ours.to_string()
                });
            }
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Take `lead` off the front of the first table whose header starts with it,
/// looking inside implicit tables (`[agents.claude]` sits in `agents`).
fn unprefix(t: &mut toml_edit::Table, lead: &str) -> bool {
    for (_, item) in t.iter_mut() {
        let Some(t) = item.as_table_mut() else {
            continue;
        };
        let rest = t
            .decor()
            .prefix()
            .and_then(|p| p.as_str())
            .and_then(|p| p.strip_prefix(lead))
            .map(str::to_string);
        if let Some(rest) = rest {
            // One blank line between the list and this table.
            let rest = if rest.starts_with('\n') { rest } else { format!("\n{rest}") };
            t.decor_mut().set_prefix(rest);
            return true;
        }
        if t.is_implicit() && unprefix(t, lead) {
            return true;
        }
    }
    false
}

pub fn from_ssh_config(h: &HostConfig) -> bool {
    h.tags.iter().any(|t| t == "ssh-config")
}
