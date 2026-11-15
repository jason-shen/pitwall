//! The app's settings: `[app]` and `[keys]` in the shared config.toml. The
//! names you give projects and tabs, and what's folded, are state rather
//! than settings, so they're kept in `~/.muster/state.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TermFont {
    pub name: String,
    pub path: PathBuf,
    /// Face index inside a .ttc collection.
    #[serde(default)]
    pub index: u32,
}

/// A saved command to run in a terminal.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Snippet {
    pub name: String,
    pub body: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// None is the system monospace font.
    pub font: Option<TermFont>,
    pub font_size: f32,
    pub font_weight: u16,
    pub bold_weight: u16,
    pub line_height: f32,
    pub scheme: String,
    pub cursor: CursorStyle,
    pub cursor_blink: bool,
    pub bold_is_bright: bool,
    /// Selecting text in a terminal copies it.
    pub copy_on_select: bool,
    /// macOS: Option+key sends Esc+key (Meta), as Claude Code's Option
    /// shortcuts and readline want. Characters a keyboard layout makes with
    /// Option, like `@` or `{` on German and French layouts, still type.
    pub option_meta: bool,
    /// How much of the scheme background covers the desktop in a see-through window.
    pub opacity: f32,
    /// Only read at startup; winit can't make an existing window transparent.
    pub see_through: bool,
    /// macOS only.
    pub blur: f32,
    /// "builtin", "$EDITOR", or a command such as "nvim" or "code".
    pub editor: String,
    /// Agent the command bar starts for a typed task.
    pub default_agent: String,
    /// New agents get their own branch and worktree.
    pub worktree: bool,
    /// The old card-keys modifier, read once to seed the card keys in
    /// `[keys]`; the keys themselves are what count now.
    #[serde(skip_serializing)]
    pub tile_mod: crate::tiling::TileMod,
    /// The tab strip runs along the bottom of the stage instead of the top.
    pub tabs_bottom: bool,
    pub snippets: Vec<Snippet>,
    /// Post a system notification when an agent needs you or finishes while
    /// muster is in the background.
    pub system_notifications: bool,
    /// Every command's keys, written as `[keys]`.
    #[serde(skip)]
    pub keys: crate::keys::Keymap,
    /// Names you gave projects, by root folder.
    #[serde(skip)]
    pub project_names: std::collections::BTreeMap<String, String>,
    /// Names you gave terminals and agents, by task id.
    #[serde(skip)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// Projects folded up in the sidebar tree, by root folder.
    #[serde(skip)]
    pub folded_projects: std::collections::BTreeSet<String>,
    /// AGENTS sorted by latest activity instead of what needs you.
    #[serde(skip)]
    pub agents_by_recent: bool,
}

/// What's kept in state.json. Also how the old gui.json is read for it.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct State {
    project_names: std::collections::BTreeMap<String, String>,
    labels: std::collections::BTreeMap<String, String>,
    folded_projects: std::collections::BTreeSet<String>,
    agents_by_recent: bool,
}

pub const FONT_SIZE_MIN: f32 = 8.0;
pub const FONT_SIZE_MAX: f32 = 32.0;
pub const FONT_SIZE_DEFAULT: f32 = 13.5;
pub const BLUR_MAX: f32 = 64.0;

impl Default for Settings {
    fn default() -> Self {
        Self {
            font: None,
            font_size: FONT_SIZE_DEFAULT,
            font_weight: 400,
            bold_weight: 700,
            line_height: 1.0,
            scheme: "Muster Dark".into(),
            cursor: CursorStyle::Block,
            cursor_blink: false,
            bold_is_bright: true,
            copy_on_select: false,
            option_meta: true,
            opacity: 0.85,
            see_through: false,
            blur: 20.0,
            editor: "builtin".into(),
            default_agent: "claude".into(),
            worktree: true,
            tile_mod: Default::default(),
            tabs_bottom: false,
            snippets: vec![],
            system_notifications: true,
            labels: Default::default(),
            folded_projects: Default::default(),
            agents_by_recent: false,
            project_names: Default::default(),
            // Ctrl+Alt: Alt+h/j/k/l, Alt+Enter and friends belong to the
            // programs in the terminal.
            keys: crate::keys::Keymap::new(None, Some("ctrl+alt")).0,
        }
    }
}

fn state_path() -> PathBuf {
    muster_core::paths::home().join("state.json")
}

/// Where the app's settings lived before config.toml.
fn legacy_path() -> PathBuf {
    muster_core::paths::home().join("gui.json")
}

fn invalid(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

/// Put [app] (with its sub-tables) and then [keys] after everything else,
/// whatever order they were added in, so the daemon's sections read first.
fn place_last(doc: &mut toml_edit::DocumentMut) {
    fn max_pos(t: &toml_edit::Table) -> isize {
        let mut m = t.position().unwrap_or(0);
        for (_, item) in t.iter() {
            match item {
                toml_edit::Item::Table(sub) => m = m.max(max_pos(sub)),
                toml_edit::Item::ArrayOfTables(a) => {
                    for sub in a.iter() {
                        m = m.max(max_pos(sub));
                    }
                }
                _ => {}
            }
        }
        m
    }
    fn place(t: &mut toml_edit::Table, next: &mut isize) {
        t.set_position(Some(*next));
        *next += 1;
        for (_, item) in t.iter_mut() {
            match item {
                toml_edit::Item::Table(sub) => place(sub, next),
                toml_edit::Item::ArrayOfTables(a) => {
                    for sub in a.iter_mut() {
                        place(sub, next);
                    }
                }
                _ => {}
            }
        }
    }
    let mut next = doc
        .iter()
        .filter(|(k, _)| *k != "app" && *k != "keys")
        .filter_map(|(_, v)| v.as_table())
        .map(max_pos)
        .max()
        .unwrap_or(0)
        + 1;
    for name in ["app", "keys"] {
        if let Some(t) = doc.get_mut(name).and_then(|i| i.as_table_mut()) {
            place(t, &mut next);
        }
    }
}

impl Settings {
    /// Read the settings, and anything in the file that couldn't be used.
    pub fn load() -> (Self, Vec<String>) {
        muster_core::paths::migrate_config();
        let mut problems = vec![];
        let text = std::fs::read_to_string(muster_core::paths::config()).unwrap_or_default();
        let table: toml::Table = toml::from_str(&text).unwrap_or_else(|e| {
            problems.push(format!("config.toml: {e}"));
            toml::Table::new()
        });
        let legacy = std::fs::read(legacy_path()).ok();
        let mut s: Settings = match table.get("app") {
            Some(v) => v.clone().try_into().unwrap_or_else(|e| {
                problems.push(format!("config.toml [app]: {e}"));
                Settings::default()
            }),
            None => legacy
                .as_deref()
                .and_then(|b| serde_json::from_slice(b).ok())
                .unwrap_or_default(),
        };
        let state: State = std::fs::read(state_path())
            .ok()
            .or(legacy)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        s.project_names = state.project_names;
        s.labels = state.labels;
        s.folded_projects = state.folded_projects;
        s.agents_by_recent = state.agents_by_recent;
        // Until [keys] is written, the card keys follow the old setting.
        let cards = match (table.contains_key("keys"), s.tile_mod) {
            (true, _) | (false, crate::tiling::TileMod::Alt | crate::tiling::TileMod::CtrlAlt) => {
                Some("ctrl+alt")
            }
            (false, crate::tiling::TileMod::Off) => None,
        };
        let (keys, p) =
            crate::keys::Keymap::new(table.get("keys").and_then(|v| v.as_table()), cards);
        s.keys = keys;
        problems.extend(p);
        // Settings still in gui.json move into config.toml now.
        if !table.contains_key("app") && legacy_path().exists() && s.save().is_ok() {
            let _ = std::fs::rename(legacy_path(), legacy_path().with_extension("json.moved"));
        }
        (s, problems)
    }

    /// Write `[app]` and `[keys]` into config.toml, in place: the daemon's
    /// sections and your comments elsewhere in the file stay as they are.
    /// A file that doesn't parse is left alone rather than overwritten.
    pub fn save(&self) -> std::io::Result<()> {
        let path = muster_core::paths::config();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => muster_core::config::DEFAULT_TOML.to_string(),
        };
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(invalid)?;
        let ours: toml_edit::DocumentMut = toml::to_string(self)
            .map_err(invalid)?
            .parse()
            .map_err(invalid)?;
        if !doc.contains_table("app") {
            let mut t = toml_edit::Table::new();
            t.decor_mut().set_prefix(
                "\n# The app's settings. Settings in the app (cmd+,) writes these; edit them\n# here or there.\n",
            );
            doc.insert("app", toml_edit::Item::Table(t));
        }
        let app = doc["app"]
            .as_table_mut()
            .ok_or_else(|| invalid("[app] isn't a table"))?;
        let gone: Vec<String> = app
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| !ours.contains_key(k))
            .collect();
        for k in gone {
            app.remove(&k);
        }
        for (k, v) in ours.iter() {
            app.insert(k, v.clone());
        }
        if !doc.contains_table("keys") {
            let mut t = toml_edit::Table::new();
            t.decor_mut().set_prefix(
                "\n# Shortcuts. \"cmd\" is Cmd on macOS and Ctrl elsewhere; a list gives a\n# command more than one; \"\" leaves it without. Settings > Shortcuts rebinds.\n",
            );
            doc.insert("keys", toml_edit::Item::Table(t));
        }
        let keys = doc["keys"]
            .as_table_mut()
            .ok_or_else(|| invalid("[keys] isn't a table"))?;
        for (k, v) in self.keys.to_toml().iter() {
            keys.insert(k, v.clone());
        }
        place_last(&mut doc);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, doc.to_string())?;
        std::fs::rename(tmp, &path)?;

        let state = State {
            project_names: self.project_names.clone(),
            labels: self.labels.clone(),
            folded_projects: self.folded_projects.clone(),
            agents_by_recent: self.agents_by_recent,
        };
        let sp = state_path();
        if let Some(dir) = sp.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = sp.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&state).unwrap_or_default())?;
        std::fs::rename(tmp, sp)
    }

    pub fn bump_font(&mut self, by: f32) {
        self.font_size = (self.font_size.round() + by).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
    }
}
