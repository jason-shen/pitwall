//! Pitwall used to be called muster. The first pitwall binary to start moves
//! muster's state over: `~/.muster`, `~/.config/muster` and each project's
//! `.muster` folder. A muster daemon still running is stopped first so it
//! saves its tasks and lets go of the folder; the pitwall daemon then
//! restores them like after any restart.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::paths;

const OLD: &str = ".muster";
const NEW: &str = ".pitwall";

pub fn migrate() {
    // A home or config named outright is a test or a second setup, not the
    // one muster left behind.
    if std::env::var_os("PITWALL_HOME").is_some() {
        return;
    }
    let Some(h) = dirs::home_dir() else {
        return;
    };
    let (old, new) = (h.join(OLD), paths::home());
    if old.is_dir() && !new.exists() {
        stop_daemon(&old.join("muster.sock"));
        if std::fs::rename(&old, &new).is_ok() {
            let _ = std::fs::remove_file(new.join("muster.sock"));
            for f in ["tasks.json", "layout.json", "state.json"] {
                repoint(&new.join(f), &old, &new);
            }
            repair_worktrees(&paths::worktrees());
        }
    }
    if std::env::var_os("PITWALL_CONFIG").is_none() {
        let cfg = paths::config();
        if let Some(dir) = cfg.parent() {
            let old_dir = dir.with_file_name("muster");
            if old_dir.is_dir() && !dir.exists() && std::fs::rename(&old_dir, dir).is_ok() {
                rename_in_config(&cfg);
            }
        }
    }
}

/// A project's briefs folder, the first time pitwall lists or writes there.
pub fn adopt_project(root: &Path) {
    let (old, new) = (root.join(OLD), root.join(NEW));
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(old, new);
    }
}

/// `muster stop`, spoken to the old socket. The protocol hasn't changed, so
/// the old daemon understands it.
fn stop_daemon(sock: &Path) {
    let alive = || std::os::unix::net::UnixStream::connect(sock).is_ok();
    if !alive() {
        return;
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(async {
        if let Ok((c, _)) = crate::client::Client::connect(sock).await {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                c.call(crate::proto::Request::Shutdown),
            )
            .await;
        }
        // It exits a moment after answering; renaming the folder under a
        // daemon still writing to it would lose what it writes.
        for _ in 0..50 {
            if !alive() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

/// Saved paths into the old home, or into a project's `.muster`, now point
/// at their new places. A project's folder is moved here when a saved path
/// needs it, so the two never disagree.
fn repoint(file: &Path, old: &Path, new: &Path) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let (old, new) = (old.display().to_string(), new.display().to_string());
    let mut changed = false;
    walk(&mut v, &mut |s: &mut String| {
        if let Some(rest) = s.strip_prefix(&old) {
            *s = format!("{new}{rest}");
            changed = true;
        }
        let mark = format!("/{OLD}/");
        if let Some(i) = s.find(&mark) {
            adopt_project(Path::new(&s[..i]));
            *s = s.replacen(&mark, &format!("/{NEW}/"), 1);
            changed = true;
        }
    });
    if changed {
        if let Ok(out) = serde_json::to_string_pretty(&v) {
            let _ = std::fs::write(file, out);
        }
    }
}

fn walk(v: &mut serde_json::Value, f: &mut impl FnMut(&mut String)) {
    match v {
        serde_json::Value::String(s) => f(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, f)),
        serde_json::Value::Object(o) => {
            // Keys too: layout.json keys open files by path.
            let entries: Vec<(String, serde_json::Value)> = std::mem::take(o).into_iter().collect();
            for (mut k, mut x) in entries {
                f(&mut k);
                walk(&mut x, f);
                o.insert(k, x);
            }
        }
        _ => {}
    }
}

/// A moved worktree has lost its repo's pointer back to it until git is
/// told where it went.
fn repair_worktrees(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let wts: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    for wt in wts {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["worktree", "repair"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// The default config's comments name the app, and its colour scheme was
/// "Muster Dark".
fn rename_in_config(cfg: &Path) {
    let Ok(text) = std::fs::read_to_string(cfg) else {
        return;
    };
    let out: Vec<String> = text
        .lines()
        .map(|l| {
            let l = l.replace("Muster Dark", "Pitwall Dark");
            if l.trim_start().starts_with('#') {
                l.replace("muster", "pitwall").replace("Muster", "Pitwall")
            } else {
                l
            }
        })
        .collect();
    let mut out = out.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    if out != text {
        let _ = std::fs::write(cfg, out);
    }
}
