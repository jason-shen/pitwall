//! Debug builds only: `MUSTER_SHOT=/tmp/shot` steps through the screens listed in
//! `MUSTER_SHOT_STEPS` (terminal, changes, files, file:<path>, bar[:query], settings, split), saves a BMP of the
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
    /// Set by the screenshot callback once the file is written.
    waiting: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl DevShot {
    pub fn from_env() -> Option<DevShot> {
        let prefix = std::env::var("MUSTER_SHOT").ok()?;
        let steps = std::env::var("MUSTER_SHOT_STEPS")
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
                Some(("chord", k)) => self.key = egui::Key::from_name(k).map(|k| (k, true)),
                Some(("cmd", k)) => self.key = egui::Key::from_name(k).map(|k| (k, false)),
                Some(("file", f)) => app.open_file(f, Some(1)),
                Some(("rename", name)) => {
                    if let Some(id) = app.target().map(|t| t.item.clone()) {
                        app.renaming = Some((id, name.to_string()));
                    }
                }
                Some(("newagent", task)) => app.open_new_agent(task),
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
                    "newbrief" => app.open_new_brief(),
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
