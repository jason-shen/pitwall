//! Every app command and the keys that run it. Defaults are keel's scheme
//! (app commands on ⌘⇧, card keys on ⌥); `[keys]` in config.toml overrides
//! any of them, and Settings → Shortcuts rebinds them by pressing keys.
//!
//! A chord is written `cmd+shift+p`. `cmd` is ⌘ on macOS and Ctrl elsewhere,
//! so one config works on both; `ctrl` is the real Control key on macOS.

use std::collections::BTreeMap;

use egui::{Event, Key, Modifiers};

pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
}

const fn cmd(id: &'static str, group: &'static str, label: &'static str) -> Command {
    Command { id, label, group }
}

/// In the order they're listed in Settings and written to the file.
pub const COMMANDS: &[Command] = &[
    cmd(
        "command_bar",
        "General",
        "Command bar: open a file, jump to an agent, start a task",
    ),
    cmd("settings", "General", "Settings"),
    cmd("shortcuts", "General", "All shortcuts"),
    cmd("toggle_sidebar", "General", "Hide or show the sidebar"),
    cmd("font_bigger", "General", "Font bigger"),
    cmd("font_smaller", "General", "Font smaller"),
    cmd("font_reset", "General", "Font size reset"),
    cmd("new_agent", "Create", "New agent"),
    cmd("new_terminal", "Create", "New terminal"),
    cmd("new_project", "Create", "New project"),
    cmd("new_brief", "Create", "New brief"),
    cmd("next_attention", "Agents", "Next agent that needs you"),
    cmd("approve", "Agents", "Approve the pending command"),
    cmd("reject", "Agents", "Reject the pending command"),
    cmd("finish_brief", "Agents", "Brief: finish with AI"),
    cmd("hand_off_brief", "Agents", "Brief: hand to agent"),
    cmd("next_tab", "Tabs", "Next tab"),
    cmd("prev_tab", "Tabs", "Previous tab"),
    cmd("rename_tab", "Tabs", "Rename the tab"),
    cmd("close_tab", "Tabs", "Close the terminal"),
    cmd("next_project", "Projects", "Next project"),
    cmd("prev_project", "Projects", "Previous project"),
    cmd("project_1", "Projects", "Project 1"),
    cmd("project_2", "Projects", "Project 2"),
    cmd("project_3", "Projects", "Project 3"),
    cmd("project_4", "Projects", "Project 4"),
    cmd("project_5", "Projects", "Project 5"),
    cmd("project_6", "Projects", "Project 6"),
    cmd("project_7", "Projects", "Project 7"),
    cmd("project_8", "Projects", "Project 8"),
    cmd("project_9", "Projects", "Project 9"),
    cmd("show_changes", "Views", "Changes (again to go back)"),
    cmd("show_files", "Views", "Files (again to go back)"),
    cmd("undo_history", "Views", "Undo history"),
    cmd("markdown_view", "Views", "Markdown: edit or preview"),
    cmd(
        "broadcast",
        "Views",
        "Broadcast typing to every terminal card",
    ),
    cmd("clear_terminal", "Views", "Clear the terminal"),
    cmd("split", "Cards", "Split: a second card"),
    cmd("close_card", "Cards", "Close the card or the open file"),
    cmd("card_left", "Cards", "Focus the card to the left"),
    cmd("card_down", "Cards", "Focus the card below"),
    cmd("card_up", "Cards", "Focus the card above"),
    cmd("card_right", "Cards", "Focus the card to the right"),
    cmd("swap_left", "Cards", "Swap with the card to the left"),
    cmd("swap_down", "Cards", "Swap with the card below"),
    cmd("swap_up", "Cards", "Swap with the card above"),
    cmd("swap_right", "Cards", "Swap with the card to the right"),
    cmd(
        "card_terminal",
        "Cards",
        "New terminal in a card of its own",
    ),
    cmd("card_close", "Cards", "Close the card"),
    cmd("card_maximize", "Cards", "Maximize or restore the card"),
];

/// The keys a command has out of the box. `cards` is the modifier the old
/// "card keys" setting picked, so an upgraded config keeps its card keys.
fn defaults(id: &str, cards: Option<&str>) -> Vec<&'static str> {
    let mac = cfg!(target_os = "macos");
    let card = |k: &'static str,
                shifted: &'static str,
                ctrl_alt: &'static str,
                ctrl_alt_shifted: &'static str,
                alt_shift: bool|
     -> Vec<&'static str> {
        match cards {
            None => vec![],
            Some("ctrl+alt") => vec![if alt_shift {
                ctrl_alt_shifted
            } else {
                ctrl_alt
            }],
            Some(_) => vec![if alt_shift { shifted } else { k }],
        }
    };
    match id {
        // ⌘P too on macOS (VS Code's quick open); Ctrl+P elsewhere is the
        // shell's previous command.
        "command_bar" if mac => vec!["cmd+shift+p", "cmd+p"],
        "command_bar" => vec!["cmd+shift+p"],
        "settings" => vec!["cmd+,"],
        "shortcuts" => vec!["cmd+shift+/"],
        "toggle_sidebar" => vec!["cmd+shift+e"],
        // ⌘= arrives as ⌘⇧= (⌘+) on most layouts.
        "font_bigger" => vec!["cmd+=", "cmd+shift+="],
        "font_smaller" => vec!["cmd+-"],
        "font_reset" => vec!["cmd+0"],
        "new_agent" => vec!["cmd+shift+n"],
        "new_terminal" => vec!["cmd+shift+t"],
        "new_project" => vec!["cmd+alt+n"],
        "new_brief" => vec!["cmd+alt+b"],
        "next_attention" => vec!["cmd+shift+a"],
        "approve" => vec!["cmd+shift+y"],
        "reject" => vec!["cmd+shift+r"],
        "finish_brief" => vec!["cmd+alt+f"],
        "hand_off_brief" => vec!["cmd+alt+enter"],
        "next_tab" => vec!["cmd+shift+]", "ctrl+tab"],
        "prev_tab" => vec!["cmd+shift+[", "ctrl+shift+tab"],
        "rename_tab" => vec!["cmd+alt+r"],
        "close_tab" => vec!["cmd+alt+w"],
        "next_project" => vec!["cmd+shift+k"],
        "prev_project" => vec!["cmd+shift+j"],
        "project_1" => vec!["cmd+1"],
        "project_2" => vec!["cmd+2"],
        "project_3" => vec!["cmd+3"],
        "project_4" => vec!["cmd+4"],
        "project_5" => vec!["cmd+5"],
        "project_6" => vec!["cmd+6"],
        "project_7" => vec!["cmd+7"],
        "project_8" => vec!["cmd+8"],
        "project_9" => vec!["cmd+9"],
        "show_changes" => vec!["cmd+shift+g"],
        "show_files" => vec!["cmd+shift+o"],
        "undo_history" => vec!["cmd+shift+u"],
        "markdown_view" => vec!["cmd+alt+p"],
        "broadcast" => vec!["cmd+shift+b"],
        // ⌘K like Terminal.app and iTerm2; Ctrl+K belongs to the shell.
        "clear_terminal" if mac => vec!["cmd+k"],
        "clear_terminal" => vec![],
        "split" => vec!["cmd+shift+d"],
        "close_card" => vec!["cmd+shift+w"],
        "card_left" => card("alt+h", "", "ctrl+alt+h", "", false),
        "card_down" => card("alt+j", "", "ctrl+alt+j", "", false),
        "card_up" => card("alt+k", "", "ctrl+alt+k", "", false),
        "card_right" => card("alt+l", "", "ctrl+alt+l", "", false),
        "swap_left" => card("", "alt+shift+h", "", "ctrl+alt+shift+h", true),
        "swap_down" => card("", "alt+shift+j", "", "ctrl+alt+shift+j", true),
        "swap_up" => card("", "alt+shift+k", "", "ctrl+alt+shift+k", true),
        "swap_right" => card("", "alt+shift+l", "", "ctrl+alt+shift+l", true),
        "card_terminal" => card("alt+enter", "", "ctrl+alt+enter", "", false),
        "card_close" => card("alt+w", "", "ctrl+alt+w", "", false),
        "card_maximize" => card("alt+z", "", "ctrl+alt+z", "", false),
        _ => vec![],
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub cmd: bool,
    /// The Control key itself; only distinct from `cmd` on macOS.
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Chord {
    pub fn parse(s: &str) -> Option<Chord> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return None;
        }
        // The key is after the last `+`, unless the key is `+` itself.
        let (mods, key) = match s.strip_suffix("++") {
            Some(m) => (m, "plus"),
            None => match s.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", s.as_str()),
            },
        };
        let mut c = Chord {
            cmd: false,
            ctrl: false,
            alt: false,
            shift: false,
            key: key_from(key)?,
        };
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            match m {
                "cmd" | "command" | "super" | "meta" => c.cmd = true,
                "ctrl" | "control" => c.ctrl = true,
                "alt" | "opt" | "option" => c.alt = true,
                "shift" => c.shift = true,
                _ => return None,
            }
        }
        // Off macOS there's one Control key, and it's what `cmd` means.
        if !cfg!(target_os = "macos") && c.ctrl {
            c.ctrl = false;
            c.cmd = true;
        }
        Some(c)
    }

    /// As written in config.toml.
    pub fn to_config(self) -> String {
        let mut s = String::new();
        for (on, m) in [
            (self.cmd, "cmd"),
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.shift, "shift"),
        ] {
            if on {
                s.push_str(m);
                s.push('+');
            }
        }
        s.push_str(&key_name(self.key));
        s
    }

    /// As shown: ⌘⇧P on macOS, Ctrl+Shift+P elsewhere.
    pub fn label(self) -> String {
        let key = key_label(self.key);
        if cfg!(target_os = "macos") {
            let mut s = String::new();
            for (on, g) in [
                (self.ctrl, "⌃"),
                (self.alt, "⌥"),
                (self.shift, "⇧"),
                (self.cmd, "⌘"),
            ] {
                if on {
                    s.push_str(g);
                }
            }
            s + &key
        } else {
            let mut parts = vec![];
            for (on, m) in [(self.cmd, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift")] {
                if on {
                    parts.push(m.to_string());
                }
            }
            parts.push(key);
            parts.join("+")
        }
    }

    pub fn from_event(key: Key, physical: Option<Key>, m: Modifiers) -> Chord {
        // With ⌥ held, macOS turns letters into other characters; the key
        // under the finger is what was meant.
        let key = match physical {
            Some(p) if m.alt => p,
            _ => key,
        };
        let mac = cfg!(target_os = "macos");
        Chord {
            cmd: if mac { m.mac_cmd } else { m.ctrl },
            ctrl: mac && m.ctrl,
            alt: m.alt,
            shift: m.shift,
            key,
        }
    }

    fn matches(self, key: Key, physical: Option<Key>, m: Modifiers) -> bool {
        let mac = cfg!(target_os = "macos");
        let mods = if mac {
            m.mac_cmd == self.cmd && m.ctrl == self.ctrl
        } else {
            m.ctrl == self.cmd
        } && m.alt == self.alt
            && m.shift == self.shift;
        mods && (key == self.key || physical == Some(self.key))
    }
}

fn key_from(s: &str) -> Option<Key> {
    Some(match s {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "plus" => Key::Plus,
        "," => Key::Comma,
        "." => Key::Period,
        "/" => Key::Slash,
        ";" => Key::Semicolon,
        "'" => Key::Quote,
        "[" => Key::OpenBracket,
        "]" => Key::CloseBracket,
        "\\" => Key::Backslash,
        "-" => Key::Minus,
        "=" => Key::Equals,
        "`" => Key::Backtick,
        other => Key::from_name(&other.to_ascii_uppercase())
            .or_else(|| Key::from_name(other))
            .filter(|_| other.len() <= 3 || other.starts_with('f'))?,
    })
}

fn key_name(k: Key) -> String {
    match k {
        Key::Enter => "enter".into(),
        Key::Tab => "tab".into(),
        Key::Space => "space".into(),
        Key::Escape => "esc".into(),
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::ArrowUp => "up".into(),
        Key::ArrowDown => "down".into(),
        Key::ArrowLeft => "left".into(),
        Key::ArrowRight => "right".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::Plus => "plus".into(),
        Key::Comma => ",".into(),
        Key::Period => ".".into(),
        Key::Slash => "/".into(),
        Key::Semicolon => ";".into(),
        Key::Quote => "'".into(),
        Key::OpenBracket => "[".into(),
        Key::CloseBracket => "]".into(),
        Key::Backslash => "\\".into(),
        Key::Minus => "-".into(),
        Key::Equals => "=".into(),
        Key::Backtick => "`".into(),
        other => other.name().to_ascii_lowercase(),
    }
}

fn key_label(k: Key) -> String {
    let mac = cfg!(target_os = "macos");
    match k {
        Key::Enter if mac => "↵".into(),
        Key::Tab if mac => "⇥".into(),
        Key::ArrowUp => "↑".into(),
        Key::ArrowDown => "↓".into(),
        Key::ArrowLeft => "←".into(),
        Key::ArrowRight => "→".into(),
        Key::Backspace if mac => "⌫".into(),
        Key::Plus => "+".into(),
        _ => {
            let n = key_name(k);
            if n.chars().count() == 1 {
                n.to_uppercase()
            } else {
                let mut c = n.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }
        }
    }
}

/// Every command's chords.
#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    map: BTreeMap<&'static str, Vec<Chord>>,
}

impl Keymap {
    /// The defaults, with `[keys]` from the config over them. Entries that
    /// don't parse are reported and left at their default.
    pub fn new(table: Option<&toml::Table>, cards: Option<&str>) -> (Keymap, Vec<String>) {
        let mut map = BTreeMap::new();
        let mut problems = vec![];
        for c in COMMANDS {
            let mut chords: Vec<Chord> = defaults(c.id, cards)
                .into_iter()
                .filter_map(Chord::parse)
                .collect();
            if let Some(v) = table.and_then(|t| t.get(c.id)) {
                let written: Vec<String> = match v {
                    toml::Value::String(s) => vec![s.clone()],
                    toml::Value::Array(a) => a
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect(),
                    _ => {
                        problems.push(format!("[keys] {}: expected a string or a list", c.id));
                        vec![]
                    }
                };
                let parsed: Vec<Option<Chord>> = written
                    .iter()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| Chord::parse(s))
                    .collect();
                if parsed.iter().all(Option::is_some) {
                    chords = parsed.into_iter().flatten().collect();
                } else {
                    problems.push(format!("[keys] {}: couldn't read {:?}", c.id, written));
                }
            }
            map.insert(c.id, chords);
        }
        if let Some(t) = table {
            for k in t.keys() {
                if !COMMANDS.iter().any(|c| c.id == k) {
                    problems.push(format!("[keys] {k}: there's no such command"));
                }
            }
        }
        (Keymap { map }, problems)
    }

    pub fn chords(&self, id: &str) -> &[Chord] {
        self.map.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set(&mut self, id: &'static str, chords: Vec<Chord>) {
        self.map.insert(id, chords);
    }

    fn default_chords(id: &str) -> Vec<Chord> {
        defaults(id, Some("ctrl+alt"))
            .into_iter()
            .filter_map(Chord::parse)
            .collect()
    }

    pub fn reset(&mut self, id: &'static str) {
        self.map.insert(id, Self::default_chords(id));
    }

    pub fn is_default(&self, id: &str) -> bool {
        self.chords(id) == Self::default_chords(id).as_slice()
    }

    /// The first chord's label, or "" when the command has none.
    pub fn label(&self, id: &str) -> String {
        self.chords(id)
            .first()
            .map(|c| c.label())
            .unwrap_or_default()
    }

    /// Consume a press of one of `id`'s chords, if there is one this frame.
    pub fn take(&self, ctx: &egui::Context, id: &str) -> bool {
        let chords = self.chords(id);
        if chords.is_empty() {
            return false;
        }
        ctx.input_mut(|i| {
            let hit = i.events.iter().position(|e| match e {
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } => chords
                    .iter()
                    .any(|c| c.matches(*key, *physical_key, *modifiers)),
                _ => false,
            });
            let Some(p) = hit else {
                return false;
            };
            i.events.remove(p);
            // ⌥ chords on macOS also type a character; it isn't text.
            if matches!(i.events.get(p), Some(Event::Text(_))) {
                i.events.remove(p);
            }
            true
        })
    }

    /// Commands that share a chord.
    pub fn clashes(&self) -> Vec<(Chord, Vec<&'static str>)> {
        let mut by: Vec<(Chord, Vec<&'static str>)> = vec![];
        for c in COMMANDS {
            for ch in self.chords(c.id) {
                match by.iter_mut().find(|(k, _)| k == ch) {
                    Some((_, ids)) => ids.push(c.id),
                    None => by.push((*ch, vec![c.id])),
                }
            }
        }
        by.retain(|(_, ids)| ids.len() > 1);
        by
    }

    /// The `[keys]` table, every command in order.
    pub fn to_toml(&self) -> toml_edit::Table {
        let mut t = toml_edit::Table::new();
        for c in COMMANDS {
            let chords: Vec<String> = self.chords(c.id).iter().map(|c| c.to_config()).collect();
            let v: toml_edit::Value = match chords.as_slice() {
                [one] => one.as_str().into(),
                many => many
                    .iter()
                    .map(String::as_str)
                    .collect::<toml_edit::Array>()
                    .into(),
            };
            t.insert(c.id, toml_edit::Item::Value(v));
        }
        t
    }
}

thread_local! {
    /// This frame's keymap, so a tooltip deep in the UI can name the keys
    /// without borrowing the app.
    static CURRENT: std::cell::RefCell<Option<Keymap>> = const { std::cell::RefCell::new(None) };
}

pub fn set_current(k: &Keymap) {
    CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref() != Some(k) {
            *c = Some(k.clone());
        }
    });
}

/// A command's keys for a hint, like "⌘⇧N", or "" when it has none.
pub fn hint(id: &str) -> String {
    CURRENT.with(|c| c.borrow().as_ref().map(|k| k.label(id)).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_read_and_write_back() {
        for s in [
            "cmd+shift+p",
            "alt+shift+h",
            "cmd+=",
            "cmd+,",
            "cmd+alt+enter",
            "cmd+shift+[",
            "f2",
            "cmd+shift+/",
        ] {
            let c = Chord::parse(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(c.to_config(), s, "{s}");
        }
        assert_eq!(Chord::parse("cmd++").map(|c| c.key), Some(Key::Plus));
        assert!(Chord::parse("cmd+shift+nope").is_none());
        assert!(Chord::parse("hyper+p").is_none());
    }

    #[test]
    fn config_overrides_defaults_and_reports_mistakes() {
        let t: toml::Table = toml::from_str(
            "command_bar = \"cmd+k\"\nnew_agent = [\"cmd+shift+n\", \"f3\"]\nsplit = \"\"\nbogus = \"cmd+x\"\nreject = \"cmd+shift+wat\"",
        )
        .unwrap();
        let (k, problems) = Keymap::new(Some(&t), Some("alt"));
        assert_eq!(k.chords("command_bar"), &[Chord::parse("cmd+k").unwrap()]);
        assert_eq!(k.chords("new_agent").len(), 2);
        assert!(k.chords("split").is_empty());
        // The broken one keeps its default.
        assert_eq!(
            k.label("reject"),
            Chord::parse("cmd+shift+r").unwrap().label()
        );
        assert_eq!(problems.len(), 2);
        // Card keys follow the old setting until they're written out.
        let (off, _) = Keymap::new(None, None);
        assert!(off.chords("card_left").is_empty());
    }

    #[test]
    fn written_table_reads_back_the_same() {
        let (k, _) = Keymap::new(None, Some("alt"));
        let text = toml_edit::DocumentMut::from(k.to_toml()).to_string();
        let back: toml::Table = toml::from_str(&text).unwrap();
        let (k2, problems) = Keymap::new(Some(&back), Some("alt"));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(k, k2);
        assert!(k.clashes().is_empty(), "{:?}", k.clashes());
    }
}
