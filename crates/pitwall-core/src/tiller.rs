//! Client side of the Tiller protocol, so pitwall can work with a machine
//! running tillerd the way the Tiller phone app does: its tmux sessions as
//! terminals, its agents and their approvals.
//!
//! The wire format is Tiller's docs/PROTOCOL.md: frames of `len u32 BE`
//! (kind byte + payload), `kind`, payload. JSON frames carry requests,
//! responses and events; PANE_OUT/PANE_IN carry raw terminal bytes. Over SSH
//! the link is the stdin/stdout of `tillerd stdio` on the host.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

pub const KIND_JSON: u8 = 0x01;
pub const KIND_PANE_OUT: u8 = 0x02;
pub const KIND_PANE_IN: u8 = 0x03;
pub const KIND_PING: u8 = 0x05;
pub const KIND_PONG: u8 = 0x06;
const MAX_FRAME: usize = 16 << 20;

/// What the phone app runs over SSH: tillerd from ~/.local/bin, where its
/// console installs it, or from PATH.
pub const DEFAULT_HELPER: &str = r#"if [ -x "$HOME/.local/bin/tillerd" ]; then exec "$HOME/.local/bin/tillerd" stdio; else exec tillerd stdio; fi"#;

/// A Tiller host as pitwall keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct TillerHost {
    pub name: String,
    /// tillerd's host id, so pairing the same host again updates it.
    pub host_id: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    /// Overrides [DEFAULT_HELPER].
    pub helper: Option<String>,
    /// tillerd on this machine: run the helper directly, no SSH.
    pub local: bool,
}

impl TillerHost {
    /// `user@host`, or just the host when no user is set.
    pub fn ssh_target(&self) -> String {
        if self.local {
            return "this machine".into();
        }
        if self.ssh_user.is_empty() {
            self.ssh_host.clone()
        } else {
            format!("{}@{}", self.ssh_user, self.ssh_host)
        }
    }

    pub fn port(&self) -> u16 {
        if self.ssh_port == 0 {
            22
        } else {
            self.ssh_port
        }
    }

    pub fn helper(&self) -> &str {
        self.helper.as_deref().filter(|h| !h.trim().is_empty()).unwrap_or(DEFAULT_HELPER)
    }

    /// The `ssh` arguments that open a link. `batch` fails instead of
    /// prompting, for connections with no terminal to prompt in.
    pub fn ssh_args(&self, batch: bool) -> Vec<String> {
        let mut a = vec![
            "-T".into(),
            "-o".into(),
            "ServerAliveInterval=15".into(),
            "-p".into(),
            self.port().to_string(),
        ];
        if batch {
            a.extend(["-o".into(), "BatchMode=yes".into()]);
        }
        a.push(self.ssh_target());
        a.push(self.helper().to_string());
        a
    }
}

/// Reads the pairing code tillerd's console shows ("Copy pairing JSON").
/// pitwall connects over SSH, so the code needs an SSH address.
pub fn parse_pairing(text: &str) -> Result<TillerHost> {
    let v: Value = serde_json::from_str(text.trim()).context("not JSON")?;
    if v["v"] != 1 || !v["host_id"].is_string() {
        bail!("not a Tiller pairing code");
    }
    let ssh = &v["ssh"];
    let host = ssh["host"].as_str().unwrap_or_default();
    if host.is_empty() {
        bail!("this pairing code has no SSH address; in the tillerd console, set one under Pair a phone (pitwall connects over SSH)");
    }
    Ok(TillerHost {
        name: v["name"].as_str().unwrap_or(host).to_string(),
        host_id: v["host_id"].as_str().unwrap_or_default().to_string(),
        ssh_host: host.to_string(),
        ssh_port: ssh["port"].as_u64().unwrap_or(22) as u16,
        ssh_user: ssh["user"].as_str().unwrap_or_default().to_string(),
        helper: None,
        local: false,
    })
}

/// Something the host sent that isn't a reply.
#[derive(Debug)]
pub enum Incoming {
    Event { name: String, data: Value },
    PaneOut { pane: u32, data: Vec<u8> },
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

/// One connection to tillerd. Cheap to clone; the connection (and the ssh
/// process behind it) closes when the last clone and the receiver are gone.
#[derive(Clone)]
pub struct Link {
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next: Arc<AtomicU64>,
    /// Why the link ended, once it has.
    closed: Arc<Mutex<Option<String>>>,
    _child: Option<Arc<tokio::sync::Mutex<tokio::process::Child>>>,
}

impl Link {
    /// Runs tillerd over `ssh` (or directly, for this machine) and says hello.
    pub async fn connect_ssh(h: &TillerHost, batch: bool) -> Result<(Link, mpsc::UnboundedReceiver<Incoming>)> {
        let mut cmd = if h.local {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(h.helper());
            c
        } else {
            let mut c = tokio::process::Command::new("ssh");
            c.args(h.ssh_args(batch));
            c
        };
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("running ssh")?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        // ssh and the remote shell explain failures on stderr ("Permission
        // denied", "tillerd: not found"); keep the tail for the error.
        let err_tail = Arc::new(Mutex::new(String::new()));
        let et = err_tail.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut t = et.lock().unwrap();
                t.push_str(&String::from_utf8_lossy(&buf[..n]));
                let keep = t.len().saturating_sub(2000);
                t.drain(..keep);
            }
        });
        let (mut link, rx) = Link::over(stdout, stdin);
        link._child = Some(Arc::new(tokio::sync::Mutex::new(child)));
        match link.hello().await {
            Ok(_) => Ok((link, rx)),
            Err(e) => {
                // Let stderr drain before reporting.
                tokio::time::sleep(Duration::from_millis(200)).await;
                let tail = err_tail.lock().unwrap().trim().to_string();
                if tail.is_empty() {
                    Err(e)
                } else {
                    Err(anyhow!("{}", last_lines(&tail, 3)))
                }
            }
        }
    }

    /// A link over any byte stream.
    pub fn over<R, W>(r: R, w: W) -> (Link, mpsc::UnboundedReceiver<Incoming>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let closed: Arc<Mutex<Option<String>>> = Arc::default();

        let mut w = w;
        tokio::spawn(async move {
            while let Some(f) = out_rx.recv().await {
                if w.write_all(&f).await.is_err() || w.flush().await.is_err() {
                    break;
                }
            }
        });

        let (p, c, o) = (pending.clone(), closed.clone(), out.clone());
        tokio::spawn(async move {
            let reason = read_loop(r, &p, &in_tx, &o).await;
            *c.lock().unwrap() = Some(reason.clone());
            for (_, tx) in p.lock().unwrap().drain() {
                let _ = tx.send(Err(anyhow!("{reason}")));
            }
        });

        let link = Link { out, pending, next: Arc::new(AtomicU64::new(1)), closed, _child: None };
        (link, in_rx)
    }

    pub async fn hello(&self) -> Result<Value> {
        self.call(
            "hello",
            json!({"client": "pitwall", "version": crate::VERSION, "last_seq": 0, "foreground": true}),
        )
        .await
    }

    /// One request; fails after 30 s.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if let Some(why) = self.closed.lock().unwrap().clone() {
            bail!("{why}");
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let body = json!({"t": "req", "id": id, "m": method, "p": params});
        self.out.send(frame(KIND_JSON, body.to_string().as_bytes())).map_err(|_| anyhow!("connection closed"))?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => bail!("connection closed"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("{method}: no answer from tillerd")
            }
        }
    }

    /// Types bytes into a pane.
    pub fn input(&self, pane: u32, data: &[u8]) {
        let mut p = pane.to_be_bytes().to_vec();
        p.extend_from_slice(data);
        let _ = self.out.send(frame(KIND_PANE_IN, &p));
    }
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(5 + payload.len());
    f.extend_from_slice(&((payload.len() + 1) as u32).to_be_bytes());
    f.push(kind);
    f.extend_from_slice(payload);
    f
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut r: R,
    pending: &Pending,
    events: &mpsc::UnboundedSender<Incoming>,
    out: &mpsc::UnboundedSender<Vec<u8>>,
) -> String {
    loop {
        let mut len = [0u8; 4];
        if r.read_exact(&mut len).await.is_err() {
            return "tillerd closed the connection".into();
        }
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 || len > MAX_FRAME {
            return format!("bad frame length {len}");
        }
        let mut buf = vec![0u8; len];
        if r.read_exact(&mut buf).await.is_err() {
            return "tillerd closed the connection".into();
        }
        let (kind, body) = (buf[0], &buf[1..]);
        match kind {
            KIND_JSON => {
                let Ok(v) = serde_json::from_slice::<Value>(body) else { continue };
                match v["t"].as_str() {
                    Some("res") => {
                        let id = v["id"].as_u64().unwrap_or(0);
                        if let Some(tx) = pending.lock().unwrap().remove(&id) {
                            let r = if v["ok"] == true {
                                Ok(v["r"].clone())
                            } else {
                                Err(anyhow!("{}", v["err"]["msg"].as_str().unwrap_or("request failed")))
                            };
                            let _ = tx.send(r);
                        }
                    }
                    Some("evt") => {
                        let name = v["e"].as_str().unwrap_or_default().to_string();
                        let _ = events.send(Incoming::Event { name, data: v["d"].clone() });
                    }
                    _ => {}
                }
            }
            KIND_PANE_OUT if body.len() >= 4 => {
                let pane = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                let _ = events.send(Incoming::PaneOut { pane, data: body[4..].to_vec() });
            }
            KIND_PING => {
                let _ = out.send(frame(KIND_PONG, body));
            }
            _ => {}
        }
    }
}

/// A host name as a folder name.
pub fn slug(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.trim_matches('-');
    if s.is_empty() { "host".into() } else { s.into() }
}

/// A tmux session on the host, from `sessions.list`.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Session {
    pub name: String,
    pub windows: u32,
    pub attached: u32,
    pub activity: i64,
    pub path: String,
}

pub async fn sessions(link: &Link) -> Result<Vec<Session>> {
    let r = link.call("sessions.list", json!({})).await?;
    Ok(serde_json::from_value(r["sessions"].clone()).unwrap_or_default())
}

/// The pane to show for a session: the active pane of its active window.
pub fn active_pane(layout: &Value) -> Option<u32> {
    let windows = layout["windows"].as_array()?;
    let w = windows.iter().find(|w| w["active"] == true).or_else(|| windows.first())?;
    let panes = w["panes"].as_array()?;
    let p = panes.iter().find(|p| p["active"] == true).or_else(|| panes.first())?;
    p["id"].as_u64().map(|n| n as u32)
}

/// Whether a layout still has a pane.
pub fn has_pane(layout: &Value, pane: u32) -> bool {
    layout["windows"].as_array().into_iter().flatten().any(|w| {
        w["panes"].as_array().into_iter().flatten().any(|p| p["id"].as_u64() == Some(pane as u64))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_needs_ssh() {
        let h = parse_pairing(r#"{"v":1,"name":"box","host_id":"abc","secret":"s","ssh":{"host":"100.64.0.5","port":2222,"user":"jo"}}"#).unwrap();
        assert_eq!(h.ssh_target(), "jo@100.64.0.5");
        assert_eq!(h.port(), 2222);
        assert!(h.ssh_args(true).contains(&"BatchMode=yes".to_string()));
        assert_eq!(h.ssh_args(false).last().unwrap(), DEFAULT_HELPER);
        assert!(parse_pairing(r#"{"v":1,"host_id":"abc","secret":"s","relay":{"ntfy":"https://ntfy.sh"}}"#).is_err());
        assert!(parse_pairing("{}").is_err());
    }

    #[test]
    fn layout_helpers() {
        let l = json!({"windows":[{"active":false,"panes":[{"id":1,"active":true}]},{"active":true,"panes":[{"id":4,"active":false},{"id":7,"active":true}]}]});
        assert_eq!(active_pane(&l), Some(7));
        assert!(has_pane(&l, 4));
        assert!(!has_pane(&l, 9));
    }

    #[tokio::test]
    async fn talks_the_protocol() {
        // A fake tillerd on the other end of an in-memory pipe.
        let (client, server) = tokio::io::duplex(1 << 16);
        let (cr, cw) = tokio::io::split(client);
        let (mut sr, mut sw) = tokio::io::split(server);
        let (link, mut rx) = Link::over(cr, cw);
        tokio::spawn(async move {
            loop {
                let mut len = [0u8; 4];
                if sr.read_exact(&mut len).await.is_err() {
                    return;
                }
                let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
                sr.read_exact(&mut buf).await.unwrap();
                match buf[0] {
                    KIND_JSON => {
                        let v: Value = serde_json::from_slice(&buf[1..]).unwrap();
                        let r = match v["m"].as_str().unwrap() {
                            "sessions.list" => json!({"t":"res","id":v["id"],"ok":true,"r":{"sessions":[{"name":"main","windows":2}]}}),
                            _ => json!({"t":"res","id":v["id"],"ok":false,"err":{"code":"bad_request","msg":"nope"}}),
                        };
                        sw.write_all(&frame(KIND_JSON, r.to_string().as_bytes())).await.unwrap();
                        let ev = json!({"t":"evt","seq":1,"e":"agent.state","d":{"state":"working"}});
                        sw.write_all(&frame(KIND_JSON, ev.to_string().as_bytes())).await.unwrap();
                    }
                    KIND_PANE_IN => {
                        // Echo typed bytes back as output of the same pane.
                        sw.write_all(&frame(KIND_PANE_OUT, &buf[1..])).await.unwrap();
                    }
                    _ => {}
                }
            }
        });
        let s = sessions(&link).await.unwrap();
        assert_eq!(s[0].name, "main");
        assert_eq!(s[0].windows, 2);
        assert!(link.call("nope", json!({})).await.unwrap_err().to_string().contains("nope"));
        link.input(12, b"ls\r");
        let mut saw_event = false;
        loop {
            match rx.recv().await.unwrap() {
                Incoming::Event { name, .. } => saw_event |= name == "agent.state",
                Incoming::PaneOut { pane, data } => {
                    assert_eq!((pane, data.as_slice()), (12, &b"ls\r"[..]));
                    break;
                }
            }
        }
        assert!(saw_event);
    }
}
