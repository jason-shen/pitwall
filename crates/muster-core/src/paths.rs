//! Filesystem layout. Everything lives under `~/.muster` unless `MUSTER_HOME` is set.

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

pub fn config() -> PathBuf {
    home().join("config.toml")
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
