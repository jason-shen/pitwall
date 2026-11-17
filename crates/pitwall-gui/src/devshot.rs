//! Debug builds only: `PITWALL_SHOT=/tmp/shot` steps through the screens listed in
//! `PITWALL_SHOT_STEPS` (terminal, changes, files, file:<path>, bar[:query], settings, newagent, approval, toast, join:<pane>:<tab>, separate:<pane>, connect:<hosts>, fleet:<hosts>:<command>, addremote:<addr>[:<name>], saveremote, group:<name>, move:<remote>:<group>, remove:<remote>, projects, remotes, keystab, keypw:<pw>, addkey:<name>:<service>:<secret>, reveal:<pw>, startreview, auto, drop:<path>, split, select, size:WxH), saves a BMP of the
//! window after each, then quits. Lets UI changes be checked without screen
//! recording permission.

use std::time::{Duration, Instant};

use crate::app::{App, Child};

pub struct DevShot {
    prefix: String,
    steps: Vec<String>,
    next: usize,
    at: Instant,
    /// A key to feed in with the next frame's input, with ⌘ (and ⇧ if true).
    pub key: Option<(egui::Key, bool)>,
    /// Text to type in with the next frame's input, then Enter.
    pub typed: Option<String>,
    /// A file to drop on the window with the next frame's input.
    pub dropped: Option<std::path::PathBuf>,
    /// Set by the screenshot callback once the file is written.
    waiting: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl DevShot {
    pub fn from_env() -> Option<DevShot> {
        let prefix = std::env::var("PITWALL_SHOT").ok()?;
        let steps = std::env::var("PITWALL_SHOT_STEPS")
            .unwrap_or_else(|_| "terminal,changes,files,file:README.md,bar,settings,split".into())
            .split(',')
            .map(str::to_string)
            .collect();
        Some(DevShot {
            prefix,
            steps,
            next: 0,
            at: Instant::now() + Duration::from_secs(4),
            waiting: None,
            key: None,
            typed: None,
            dropped: None,
        })
    }

    pub fn tick(&mut self, ctx: &egui::Context, app: &mut App) {
        ctx.request_repaint_after(Duration::from_millis(100));
        if let Some(done) = &self.waiting {
            if !done.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            self.waiting = None;
            self.at = Instant::now() + Duration::from_millis(1500);
        }
        if Instant::now() < self.at || app.tabs.is_empty() {
            return;
        }
        let Some(step) = self.steps.get(self.next).cloned() else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        if self.at.elapsed() < Duration::from_millis(1500) && !step.starts_with('@') {
            // Apply the step, then give it time to load before capturing.
            match step.split_once(':') {
                Some(("agent", spec)) => {
                    // agent:<kind>:<name>, in the launch folder, no worktree.
                    if let Some((kind, name)) = spec.split_once(':') {
                        let dir = app.ctx_dir();
                        app.create_task(kind.into(), name.into(), None, false, dir);
                    }
                }
                Some(("type", text)) => self.typed = Some(text.to_string()),
                Some(("pick", n)) => {
                    if let (Some(f), Ok(n)) = (app.new_form.as_mut(), n.parse()) {
                        f.template = n;
                    }
                }
                Some(("brief", p)) => app.open_brief(std::path::Path::new(p)),
                Some(("finish", p)) => app.finish_brief(std::path::Path::new(p)),
                Some(("handoff", p)) => app.hand_off_brief(std::path::Path::new(p)),
                Some(("edtop", n)) => {
                    let cur = app.cur;
                    if let (Some(it), Ok(n)) = (app.tabs.get_mut(cur), n.parse::<usize>()) {
                        let f = it.file;
                        if let Some(e) = it.files.get_mut(f) {
                            app.md_mode
                                .insert(e.path.clone(), crate::app::MdMode::Preview);
                            e.scroll_y = n;
                        }
                    }
                }
                Some(("capture", id)) => {
                    app.capturing = crate::keys::COMMANDS
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| c.id)
                }
                Some(("tile", id)) => app.tile_beside(id, app.active),
                // size:WxH resizes the window, to see terminals reflow.
                Some(("size", wh)) => {
                    if let Some((w, h)) = wh.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                                w, h,
                            )));
                        }
                    }
                }
                Some(("chord", k)) => self.key = egui::Key::from_name(k).map(|k| (k, true)),
                Some(("cmd", k)) => self.key = egui::Key::from_name(k).map(|k| (k, false)),
                Some(("file", f)) => app.open_file(f, Some(1)),
                Some(("rename", name)) => {
                    if let Some(id) = app.target().map(|t| t.item.clone()) {
                        app.renaming = Some((id, name.to_string()));
                    }
                }
                Some(("newagent", task)) => app.open_new_agent(task),
                // join:<pane>:<tab>, by task name: drop one tab onto another.
                Some(("join", names)) => {
                    let id = |n: &str| app.tasks.values().find(|t| t.name == n).map(|t| t.id.clone());
                    if let Some((p, h)) = names.split_once(':') {
                        if let (Some(p), Some(h)) = (id(p), id(h)) {
                            app.join_tab(&p, &h, None);
                        }
                    }
                }
                // review:<task id or name>: as if its brief had just been finished.
                Some(("review", name)) => {
                    let pick = app.tasks.get(name).or_else(|| app.tasks.values().find(|t| t.name == name));
                    if let Some(id) = pick.map(|t| t.id.clone()) {
                        app.open_review(&id);
                    }
                }
                Some(("separate", name)) => {
                    if let Some(p) = app.tasks.values().find(|t| t.name == name).map(|t| t.id.clone()) {
                        app.separate(&p);
                    }
                }
                Some(("drop", path)) => self.dropped = Some(path.into()),
                Some(("group", name)) => {
                    app.new_group();
                    if let Some((old, _)) = app.renaming_group.clone() {
                        app.rename_group(&old, name);
                    }
                }
                Some(("move", spec)) => {
                    if let Some((host, group)) = spec.rsplit_once(':') {
                        app.refresh_hosts(true);
                        app.move_remote(host, Some(group).filter(|g| !g.is_empty()));
                    }
                }
                Some(("remove", host)) => {
                    app.refresh_hosts(true);
                    app.ask_remove_remote(host);
                }
                Some(("keypw", pw)) => {
                    app.api_ui.setup_pw = pw.to_string();
                    app.api_ui.setup_confirm = pw.to_string();
                    app.keys_setup();
                }
                Some(("addkey", spec)) => {
                    let mut it = spec.splitn(3, ':');
                    let (name, service, secret) =
                        (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
                    app.keys_open_editor(None);
                    if let Some(e) = app.api_ui.editor.as_mut() {
                        e.name = name.into();
                        e.service = service.into();
                        e.secret = secret.into();
                    }
                    app.keys_save_editor();
                }
                Some(("reveal", pw)) => {
                    if let Some(id) = app.api_keys.keys.first().map(|k| k.id.clone()) {
                        app.keys_request(crate::apikeys::Action::Reveal(id));
                        if let Some(p) = app.api_ui.prompt.as_mut() {
                            p.password = pw.to_string();
                        }
                        app.keys_submit_prompt();
                    }
                }
                Some(("addremote", spec)) => {
                    let (addr, name) = spec.split_once(':').unwrap_or((spec, ""));
                    app.open_remote_form(None, None);
                    if let Some(f) = app.remote_form.as_mut() {
                        f.addr = addr.to_string();
                        f.name = name.to_string();
                        f.user = "ubuntu".into();
                        f.tags = "lab".into();
                    }
                }
                Some(("connect", sel)) => {
                    app.refresh_hosts(true);
                    app.connect_all(sel);
                }
                Some(("fleet", spec)) => {
                    app.refresh_hosts(true);
                    let (sel, cmd) = spec.split_once(':').unwrap_or((spec, "uptime"));
                    app.open_fleet(sel);
                    if let Some(f) = app.fleet.as_mut() {
                        f.command = cmd.to_string();
                    }
                    app.run_fleet(false);
                }
                Some(("newproject", dir)) => {
                    app.open_new_project();
                    if let Some(f) = app.new_form.as_mut() {
                        f.dir = dir.to_string();
                        f.name = "Voice Agent".into();
                        f.name_edited = true;
                    }
                }
                Some(("bar", q)) => {
                    app.bar = Some(crate::app::CmdBar {
                        query: q.to_string(),
                        ..Default::default()
                    })
                }
                _ => match step.as_str() {
                    "changes" => app.toggle_child(Child::Changes),
                    "files" => app.toggle_child(Child::Files),
                    "saveremote" => app.save_remote_form(false),
                    "projects" => app.settings.sidebar_tab = crate::settings::SidebarTab::Projects,
                    "remotes" => app.settings.sidebar_tab = crate::settings::SidebarTab::Remotes,
                    "keystab" => app.settings.sidebar_tab = crate::settings::SidebarTab::Keys,
                    "terminal" => {
                        app.bar = None;
                        app.new_form = None;
                        app.settings_ui = None;
                        if let Some(id) = app.target().map(|t| t.item.clone()) {
                            app.open_item(&id);
                        }
                    }
                    "bar" => app.bar = Some(Default::default()),
                    "settings" => {
                        app.bar = None;
                        app.settings_ui = Some(Default::default());
                    }
                    "newcard" => app.new_terminal_card(),
                    // Lines 5 to 8 of the open file, to see a selection drawn.
                    "select" => {
                        let cur = app.cur;
                        if let Some(it) = app.tabs.get_mut(cur) {
                            let f = it.file;
                            if let Some(e) = it.files.get_mut(f) {
                                e.anchor = Some((4.min(e.lines.len() - 1), 2));
                                e.cy = 7.min(e.lines.len() - 1);
                                e.cx = 6.min(e.lines[e.cy].chars().count());
                            }
                        }
                    }
                    "newbrief" => app.open_new_brief(),
                    // A brief-sized task, to see the form stay on screen.
                    "newagent" => app.open_new_agent(&"A step of a long brief.\n".repeat(80)),
                    // A long command waiting on approval, with a toast beside it.
                    "approval" => {
                        use pitwall_core::proto::{Approval, ApprovalSource, Tier};
                        let task_id = app.target().map(|t| t.item.clone()).unwrap_or_default();
                        let command = r##"# Rebuild the release and push it to the staging box
set -euo pipefail
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --target "$target" -p pitwall-gui
done
lipo -create target/*/release/pitwall-gui -output dist/pitwall-gui
if [ -n "${DEPLOY_HOST:-}" ]; then
  rsync -az --delete dist/ "deploy@$DEPLOY_HOST:/opt/pitwall/$VERSION/"
  ssh "deploy@$DEPLOY_HOST" 'sudo systemctl restart pitwall && echo restarted'
fi
cat > dist/NOTES.md <<EOF
Pitwall $VERSION built on $(date +%F)
EOF
grep -c "warning" build.log | xargs -I{} echo "{} warnings""##
                        .to_string();
                        app.approvals.insert(
                            9001,
                            Approval {
                                id: 9001,
                                source: ApprovalSource::Task { task_id, tool: "Bash".into() },
                                command,
                                tier: Tier::Mutating,
                                reason: "writes outside the worktree".into(),
                                created_unix: 0,
                            },
                        );
                        app.notify("agent: finished its turn", false);
                    }
                    "toast" => app.notify(crate::i18n::trf!("Copied {n} lines", n = 12), false),
                    // The project's Review button (twice, to see the reviewer reused).
                    "startreview" | "startreview2" => {
                        let root = app.project.clone().unwrap_or_default();
                        app.start_review(&root);
                    }
                    "auto" => {
                        if let Some(id) = app.target().map(|t| t.item.clone()) {
                            app.set_auto(&id, true);
                        }
                    }
                    "keys" => app.show_keys = true,
                    "submit" => app.submit_form(),
                    "swap" => app.swap_cards(0, 1),
                    "companion" => {
                        if let Some(id) = app.target().map(|t| t.item.clone()) {
                            app.new_companion(&id);
                        }
                    }
                    "broadcast" => app.broadcast = true,
                    "split" => {
                        app.settings_ui = None;
                        app.toggle_split();
                    }
                    _ => {}
                },
            }
            self.steps[self.next] = format!("@{step}");
            self.at = Instant::now() + Duration::from_millis(2500);
            return;
        }
        let name = step.trim_start_matches('@').replace([':', '/', '.'], "_");
        self.next += 1;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.waiting = Some(done.clone());
        let path = format!("{}-{name}.bmp", self.prefix);
        ctx.request_screenshot(move |img| {
            if let Err(e) = save_bmp(&path, &img) {
                eprintln!("devshot: {path}: {e}");
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }
}

fn save_bmp(path: &str, img: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = img.size;
    let row = w * 3;
    let pad = (4 - row % 4) % 4;
    let data_len = (row + pad) * h;
    let mut out = Vec::with_capacity(54 + data_len);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        for x in 0..w {
            let c = img.pixels[y * w + x];
            out.extend_from_slice(&[c.b(), c.g(), c.r()]);
        }
        out.extend(std::iter::repeat_n(0, pad));
    }
    std::fs::write(path, out)
}

/// What a Finder drop hands egui: just a path, and no pointer position.
#[derive(Debug)]
pub struct Dropped(pub std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}
