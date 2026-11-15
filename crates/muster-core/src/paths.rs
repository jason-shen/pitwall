//! Filesystem layout. Settings live in `~/.config/muster/config.toml`; state
//! (the socket, worktrees, snapshots, logs) under `~/.muster`. With
//! `MUSTER_HOME` set, both go there.

use std::path::PathBuf;

pub fn home() -> PathBuf {
    if let Some(p) = std::env::var_os("MUSTER_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".muster")
}

pub fn socket() -> PathBuf {
    std::env::var_os("MUSTER_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("muster.sock"))
}

/// The one settings file: the daemon's, the CLI's and both apps'.
/// `$XDG_CONFIG_HOME` is honoured; `MUSTER_CONFIG` names a file outright.
pub fn config() -> PathBuf {
    if let Some(p) = std::env::var_os("MUSTER_CONFIG") {
        return PathBuf::from(p);
    }
    if std::env::var_os("MUSTER_HOME").is_some() {
        return home().join("config.toml");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("muster").join("config.toml")
}

/// Before settings moved to ~/.config, the config lived here.
pub fn legacy_config() -> PathBuf {
    home().join("config.toml")
}

/// Move a config from the old place to the new one, once. Comments and
/// all; the old file is left behind renamed so it isn't read by mistake.
pub fn migrate_config() {
    let (new, old) = (config(), legacy_config());
    if new == old || new.exists() || !old.exists() {
        return;
    }
    if let Some(dir) = new.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::copy(&old, &new).is_ok() {
        let _ = std::fs::rename(&old, old.with_extension("toml.moved"));
    }
}
pub fn worktrees() -> PathBuf {
    home().join("worktrees")
}
pub fn snapshots() -> PathBuf {
    home().join("snapshots")
}
pub fn audit_log() -> PathBuf {
    home().join("audit.jsonl")
}
pub fn runbooks() -> PathBuf {
    home().join("runbooks")
}
pub fn daemon_log() -> PathBuf {
    home().join("daemon.log")
}

/// Expand a leading `~/`.
pub fn expand(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}
