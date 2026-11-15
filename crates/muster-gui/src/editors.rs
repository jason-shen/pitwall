//! Which editor opens files: the built-in one, a terminal editor running in a
//! PTY inside the Editor view, or a GUI app launched beside muster.

use std::path::Path;

pub enum Kind {
    Builtin,
    /// A shell command line for a TUI editor, e.g. "nvim" or "hx".
    Terminal(String),
    /// A GUI editor's command, e.g. "code" or "zed".
    Gui(String),
}

/// Choices offered in Settings: (setting value, label).
pub const PRESETS: &[(&str, &str)] = &[
    ("builtin", "Built-in"),
    ("$EDITOR", "$VISUAL / $EDITOR"),
    ("nvim", "Neovim"),
    ("vim", "Vim"),
    ("hx", "Helix"),
    ("micro", "micro"),
    ("nano", "nano"),
    ("code", "VS Code"),
    ("cursor", "Cursor"),
    ("zed", "Zed"),
    ("subl", "Sublime Text"),
];

const TERMINAL: &[&str] = &[
    "nvim", "vim", "vi", "hx", "helix", "micro", "nano", "kak", "emacs", "mg", "joe", "ne",
];

/// The program name of a command line, without its directory.
fn program(cmd: &str) -> String {
    let first = cmd.split_whitespace().next().unwrap_or("");
    Path::new(first)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn resolve(setting: &str) -> Kind {
    let s = setting.trim();
    if s.is_empty() || s == "builtin" {
        return Kind::Builtin;
    }
    let cmd = if s == "$EDITOR" {
        // A Dock launch has no $VISUAL; fall back to the built-in editor
        // rather than guessing.
        match std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")) {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return Kind::Builtin,
        }
    } else {
        s.to_string()
    };
    let prog = program(&cmd);
    // `emacs -nw` runs in the terminal, plain `emacs` opens a window.
    let tui = TERMINAL.contains(&prog.as_str()) && (prog != "emacs" || cmd.contains("-nw"));
    if tui {
        Kind::Terminal(cmd)
    } else {
        Kind::Gui(cmd)
    }
}

/// The editor for one file. Markdown always gets the built-in editor, whose
/// live preview, list keys and brief streaming nvim or VS Code wouldn't have.
pub fn for_file(setting: &str, path: &Path) -> Kind {
    if crate::briefs::is_markdown(path) {
        Kind::Builtin
    } else {
        resolve(setting)
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The shell command that starts `cmd` on `file` at `line`.
pub fn terminal_cmd(cmd: &str, file: &Path, line: usize) -> String {
    let f = file.display().to_string();
    match program(cmd).as_str() {
        // These take file:line rather than +line.
        "hx" | "helix" | "micro" => format!("exec {cmd} {}", quote(&format!("{f}:{line}"))),
        _ => format!("exec {cmd} +{line} {}", quote(&f)),
    }
}

/// Keys that make an already-running editor open another file, where the
/// editor has a command for it.
pub fn reopen_keys(cmd: &str, file: &Path, line: usize) -> Option<Vec<u8>> {
    let f = file.display().to_string().replace(' ', r"\ ");
    match program(cmd).as_str() {
        "nvim" | "vim" | "vi" => Some(format!("\x1b:e +{line} {f}\r").into_bytes()),
        "hx" | "helix" => Some(format!("\x1b:open {f}:{line}\r").into_bytes()),
        "kak" => Some(format!("\x1b:edit {f} {line}\r").into_bytes()),
        _ => None,
    }
}

pub fn open_gui(cmd: &str, file: &Path, line: usize) -> std::io::Result<()> {
    let mut parts = cmd.split_whitespace();
    let prog = parts.next().unwrap_or("open").to_string();
    let mut args: Vec<String> = parts.map(String::from).collect();
    let f = file.display().to_string();
    match program(cmd).as_str() {
        "code" | "cursor" | "windsurf" | "codium" => {
            args.push("-g".into());
            args.push(format!("{f}:{line}"));
        }
        "zed" | "subl" => args.push(format!("{f}:{line}")),
        "idea" | "pycharm" | "goland" | "rustrover" | "webstorm" => {
            args.push("--line".into());
            args.push(line.to_string());
            args.push(f);
        }
        _ => args.push(f),
    }
    std::process::Command::new(prog)
        .args(args)
        .spawn()
        .map(|_| ())
}

/// The daemon task holding a tab's terminal editor. The daemon slugs task
/// names and cuts them at 40 characters, so a project's item
/// (`project:/Users/...`) or a long task name would come back under another
/// id; those get a short hash instead.
pub fn task_id(item: &str) -> String {
    let plain = format!("edit-{item}");
    let slug_safe = item
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if slug_safe && plain.len() <= 40 {
        return plain;
    }
    // FNV-1a: stable across builds, so a running editor is found again
    // after a restart.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in item.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("edit-{h:016x}")
}

pub fn is_editor_task(id: &str) -> bool {
    id.starts_with("edit-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_ids_survive_the_daemons_slugging() {
        assert_eq!(task_id("term-3"), "edit-term-3");
        let p = task_id("project:/Users/me/code/app");
        assert!(p.starts_with("edit-") && p.len() <= 40);
        assert!(p[5..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(p, task_id("project:/Users/me/code/app"));
        assert_ne!(p, task_id("project:/Users/me/code/other"));
        assert!(task_id(&"x".repeat(60)).len() <= 40);
    }

    #[test]
    fn classifies_editors() {
        assert!(matches!(resolve("builtin"), Kind::Builtin));
        assert!(matches!(resolve("nvim"), Kind::Terminal(_)));
        assert!(matches!(resolve("/opt/homebrew/bin/hx"), Kind::Terminal(_)));
        assert!(matches!(resolve("emacs -nw"), Kind::Terminal(_)));
        assert!(matches!(resolve("emacs"), Kind::Gui(_)));
        assert!(matches!(resolve("code --wait"), Kind::Gui(_)));
        assert!(matches!(
            for_file("nvim", Path::new("a/README.md")),
            Kind::Builtin
        ));
        assert!(matches!(
            for_file("nvim", Path::new("a/main.rs")),
            Kind::Terminal(_)
        ));
    }

    #[test]
    fn commands_put_the_cursor_on_the_line() {
        let f = Path::new("/tmp/it's here.rs");
        assert_eq!(
            terminal_cmd("nvim", f, 12),
            r"exec nvim +12 '/tmp/it'\''s here.rs'"
        );
        assert_eq!(
            terminal_cmd("hx", f, 3),
            r"exec hx '/tmp/it'\''s here.rs:3'"
        );
        assert_eq!(
            reopen_keys("nvim", Path::new("/a b.rs"), 4).unwrap(),
            b"\x1b:e +4 /a\\ b.rs\r".to_vec()
        );
        assert!(reopen_keys("nano", f, 1).is_none());
    }
}
