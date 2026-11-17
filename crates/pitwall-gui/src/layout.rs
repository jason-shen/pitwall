//! What's on screen, saved to `~/.pitwall/layout.json` and put back on launch:
//! the project, the cards and which has focus, and each task's open files.
//! The daemon keeps the terminals themselves alive between launches.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::Target;

#[derive(Serialize, Deserialize, Default, PartialEq, Clone, Debug)]
#[serde(default)]
pub struct Layout {
    pub project: Option<String>,
    pub cards: Vec<Target>,
    pub active: usize,
    pub zoom: bool,
    pub sidebar: bool,
    pub items: BTreeMap<String, ItemLayout>,
    /// Terminals opened for an agent, by terminal id.
    pub parents: BTreeMap<String, String>,
    /// Tabs dropped onto other tabs: (pane, the tab it's in).
    pub joined: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Default, PartialEq, Clone, Debug)]
#[serde(default)]
pub struct ItemLayout {
    pub expanded: bool,
    pub opened: Vec<PathBuf>,
}

fn path() -> PathBuf {
    pitwall_core::paths::home().join("layout.json")
}

impl Layout {
    pub fn load() -> Option<Layout> {
        serde_json::from_slice(&std::fs::read(path()).ok()?).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, p)
    }
}
