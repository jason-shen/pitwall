//! The environment your shell gives you, for everything pitwall starts.
//!
//! An app opened from the Dock or Finder gets launchd's bare environment
//! (PATH is /usr/bin:/bin:/usr/sbin:/sbin). Installers like Claude Code's
//! put ~/.local/bin on PATH in .zshrc, which only an interactive shell reads,
//! so a login shell alone (`-l -c env`) doesn't see it either. The shell is
//! asked once, as an interactive login shell, and its environment is used
//! for the daemon, the agents and anything the app runs itself.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const MARK: &str = "__PITWALL_ENV__";

/// Your shell's environment, asked for the first time it's needed. Empty if
/// the shell couldn't be asked; callers fall back to their own environment.
pub fn env() -> &'static HashMap<String, String> {
    static ENV: OnceLock<HashMap<String, String>> = OnceLock::new();
    ENV.get_or_init(ask)
}

/// Your shell's PATH, then this process's, without repeats.
pub fn path() -> String {
    let own = std::env::var("PATH").unwrap_or_default();
    let mut seen = HashSet::new();
    env()
        .get("PATH")
        .map(String::as_str)
        .unwrap_or("")
        .split(':')
        .chain(own.split(':'))
        .filter(|d| !d.is_empty() && seen.insert(*d))
        .collect::<Vec<_>>()
        .join(":")
}

/// Where `prog` is on that PATH: the copy you'd get typing it, not an older
/// one earlier on the daemon's own PATH.
pub fn resolve(prog: &str) -> PathBuf {
    if prog.contains('/') {
        return PathBuf::from(prog);
    }
    path()
        .split(':')
        .map(|d| Path::new(d).join(prog))
        .find(|p| is_executable(p))
        .unwrap_or_else(|| PathBuf::from(prog))
}

/// `Command::new(prog)` as your shell would run it.
pub fn command(prog: &str) -> Command {
    let mut c = Command::new(resolve(prog));
    c.env("PATH", path());
    c
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn ask() -> HashMap<String, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    // The mark separates the environment from whatever an interactive shell
    // prints first (greetings, prompts, escape codes).
    let Ok(mut child) = Command::new(&shell)
        .args(["-lic", &format!("printf '\\n{MARK}\\n'; /usr/bin/env")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return HashMap::new();
    };
    let Some(mut out) = child.stdout.take() else {
        return HashMap::new();
    };
    // Read while waiting, or a big environment fills the pipe and the
    // shell never exits.
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    // A shell config that waits on something mustn't hold everything up.
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return HashMap::new();
            }
        }
    }
    let text = reader.join().unwrap_or_default();
    parse(&text)
}

fn parse(text: &str) -> HashMap<String, String> {
    let Some((_, body)) = text.rsplit_once(&format!("\n{MARK}\n")) else {
        return HashMap::new();
    };
    body.lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        // Where that shell was and how deep, not anything to pass on.
        .filter(|(k, _)| !matches!(*k, "PWD" | "OLDPWD" | "SHLVL" | "_"))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_what_the_shell_prints_first() {
        let text = format!("\x1b]0;title\x07Welcome!\n{MARK}\nPATH=/a:/b\nPWD=/x\nHOME=/h\n");
        let env = parse(&text);
        assert_eq!(env.get("PATH").map(String::as_str), Some("/a:/b"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/h"));
        assert!(!env.contains_key("PWD"));
        assert!(parse("no mark here\nPATH=/a\n").is_empty());
    }

    #[test]
    fn finds_programs_on_the_path() {
        assert!(resolve("sh").is_absolute());
        assert_eq!(resolve("/x/y"), PathBuf::from("/x/y"));
        assert_eq!(
            resolve("no-such-command-pitwall"),
            PathBuf::from("no-such-command-pitwall")
        );
    }
}
