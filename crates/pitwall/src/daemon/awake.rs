//! Keeps the computer from idle-sleeping while an agent is working, so a
//! laptop left alone doesn't stop it halfway. Only idle sleep is held off:
//! the screen still turns off on its own timer, and closing a laptop's lid
//! still sleeps it.
//!
//! The hold is a child process (`caffeinate` on macOS, `systemd-inhibit`
//! on Linux) tied to the daemon's pid, so a daemon that crashes can't leave
//! the computer awake for good.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long nothing has to be working before the hold goes. Agents go
/// quiet for a moment between steps; this keeps that from flapping.
const LINGER: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Awake {
    hold: Option<Child>,
    /// When the last working agent stopped, while the hold still lingers.
    quiet_since: Option<Instant>,
    /// It couldn't be started here; don't try again every second.
    failed: bool,
}

impl Awake {
    /// Called once a second with whether any agent is working.
    pub fn update(&mut self, busy: bool) {
        // Off in Settings: `keep_awake = false` under [app].
        let busy = busy && pitwall_core::config::app_flag("keep_awake").unwrap_or(true);
        if busy {
            self.quiet_since = None;
            if self.hold.is_none() && !self.failed {
                match hold() {
                    Ok(c) => {
                        tracing::info!("keeping the computer awake while agents work");
                        self.hold = Some(c);
                    }
                    Err(e) => {
                        tracing::warn!("can't keep the computer awake: {e:#}");
                        self.failed = true;
                    }
                }
            }
            return;
        }
        if self.hold.is_some() {
            let since = *self.quiet_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= LINGER {
                self.release();
                tracing::info!("agents are done; the computer may sleep");
            }
        }
    }

    #[cfg(test)]
    fn holding(&self) -> bool {
        self.hold.is_some()
    }

    fn release(&mut self) {
        if let Some(mut c) = self.hold.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.quiet_since = None;
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(target_os = "macos")]
fn hold() -> anyhow::Result<Child> {
    // -i holds off idle sleep only, and lets the display sleep.
    let pid = std::process::id().to_string();
    Ok(Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &pid])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

#[cfg(target_os = "linux")]
fn hold() -> anyhow::Result<Child> {
    // `tail --pid` stands in for caffeinate's -w.
    let pid = std::process::id().to_string();
    Ok(Command::new("systemd-inhibit")
        .args([
            "--what=idle:sleep",
            "--who=pitwall",
            "--why=Agents are working",
            "--mode=block",
            "tail",
            &format!("--pid={pid}"),
            "-f",
            "/dev/null",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn hold() -> anyhow::Result<Child> {
    anyhow::bail!("not supported on this system")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn holds_while_busy_and_lets_go_after_a_quiet_spell() {
        let tmp = std::env::temp_dir().join(format!("pitwall-awake-{}", std::process::id()));
        std::env::set_var("PITWALL_HOME", tmp.join("home"));
        let mut a = Awake::default();
        a.update(false);
        assert!(!a.holding());
        a.update(true);
        assert!(a.holding());
        let pid = a.hold.as_ref().unwrap().id();
        // A short pause between steps keeps it.
        a.update(false);
        assert!(a.holding());
        a.quiet_since = Some(Instant::now() - LINGER);
        a.update(false);
        assert!(!a.holding());
        // The caffeinate it started is gone.
        let alive = Command::new("kill").args(["-0", &pid.to_string()]).status();
        assert!(!alive.unwrap().success());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
