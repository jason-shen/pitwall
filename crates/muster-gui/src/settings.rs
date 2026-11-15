//! GUI preferences, kept in `~/.muster/gui.json`. The daemon's config.toml is
//! shared with the TUI and CLI, so window looks stay out of it.

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
    /// Modifier for the card keys (focus, swap, new, close, maximize).
    pub tile_mod: crate::tiling::TileMod,
    /// The tab strip runs along the bottom of the stage instead of the top.
    pub tabs_bottom: bool,
    pub snippets: Vec<Snippet>,
    /// Post a system notification when an agent needs you or finishes while
    /// muster is in the background.
    pub system_notifications: bool,
    /// Names you gave projects, by root folder.
    pub project_names: std::collections::BTreeMap<String, String>,
    /// Names you gave terminals and agents, by task id.
    pub labels: std::collections::BTreeMap<String, String>,
    /// Projects folded up in the sidebar tree, by root folder.
    pub folded_projects: std::collections::BTreeSet<String>,
    /// AGENTS sorted by latest activity instead of what needs you.
    pub agents_by_recent: bool,
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
        }
    }
}

fn path() -> PathBuf {
    muster_core::paths::home().join("gui.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, p)
    }

    pub fn bump_font(&mut self, by: f32) {
        self.font_size = (self.font_size.round() + by).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
    }
}
