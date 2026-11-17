//! Run commands across SSH hosts in parallel, via OpenSSH or the built-in russh client.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use pitwall_core::config::{Config, HostConfig};
use pitwall_core::paths;
use pitwall_core::proto::{HostInfo, HostResult};

pub fn host_infos(hosts: &[HostConfig]) -> Vec<HostInfo> {
    hosts
        .iter()
        .map(|h| HostInfo {
            name: h.name.clone(),
            addr: match h.port {
                Some(p) => format!("{}:{p}", h.shown_addr()),
                None => h.shown_addr().to_string(),
            },
            user: h.user.clone(),
            tags: h.tags.clone(),
        })
        .collect()
}

pub async fn run(cfg: &Config, hosts: &[HostConfig], command: &str) -> Vec<HostResult> {
    futures::future::join_all(hosts.iter().map(|h| run_one(cfg, h, command))).await
}

async fn run_one(cfg: &Config, h: &HostConfig, command: &str) -> HostResult {
    let start = Instant::now();
    let limit = Duration::from_secs(cfg.ssh.connect_timeout_secs + cfg.ssh.command_timeout_secs);
    let fut = async {
        match cfg.ssh.transport.as_str() {
            "native" => native(cfg, h, command).await,
            _ => openssh(cfg, h, command).await,
        }
    };
    let res = tokio::time::timeout(limit, fut).await;
    let ms = start.elapsed().as_millis() as u64;
    match res {
        Ok(Ok((exit, out, err))) => HostResult {
            host: h.name.clone(),
            exit,
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            ms,
            error: None,
        },
        Ok(Err(e)) => HostResult {
            host: h.name.clone(),
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
            ms,
            error: Some(format!("{e:#}")),
        },
        Err(_) => HostResult {
            host: h.name.clone(),
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
            ms,
            error: Some(format!("timed out after {}s", limit.as_secs())),
        },
    }
}

type Output = (Option<i32>, Vec<u8>, Vec<u8>);

async fn openssh(cfg: &Config, h: &HostConfig, command: &str) -> Result<Output> {
    let mut c = tokio::process::Command::new("ssh");
    c.args([
        "-o",
        "BatchMode=yes",
        "-o",
        &format!("ConnectTimeout={}", cfg.ssh.connect_timeout_secs),
        "-T",
    ]);
    if let Some(p) = h.port {
        c.arg("-p").arg(p.to_string());
    }
    if let Some(k) = &h.key {
        c.arg("-i").arg(paths::expand(k));
    }
    if let Some(u) = &h.user {
        c.arg("-l").arg(u);
    }
    c.arg("--").arg(h.addr()).arg(command);
    c.stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = c
        .output()
        .await
        .map_err(|e| anyhow!("failed to run ssh: {e}"))?;
    let code = out.status.code();
    if code == Some(255) && out.stdout.is_empty() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok((code, out.stdout, out.stderr))
}

// ─── native (russh) ─────────────────────────────────────────────────────

struct Verifier {
    host: String,
    port: u16,
    accept_unknown: bool,
}

impl russh::client::Handler for Verifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(self.accept_unknown);
        };
        match russh::keys::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => Ok(true),
            Ok(false) => {
                if !self.accept_unknown {
                    tracing::warn!(
                        "{}:{} not in known_hosts (set ssh.accept_unknown_hosts to allow)",
                        self.host,
                        self.port
                    );
                }
                Ok(self.accept_unknown)
            }
            Err(e) => {
                tracing::error!("host key check failed for {}: {e}", self.host);
                Ok(false)
            }
        }
    }
}

async fn native(cfg: &Config, h: &HostConfig, command: &str) -> Result<Output> {
    use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
    use russh::{client, ChannelMsg, Disconnect};

    let port = h.port.unwrap_or(22);
    let user = h
        .user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "root".into());
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(cfg.ssh.command_timeout_secs)),
        ..Default::default()
    });
    let handler = Verifier {
        host: h.addr().to_string(),
        port,
        accept_unknown: cfg.ssh.accept_unknown_hosts,
    };
    let mut session = tokio::time::timeout(
        Duration::from_secs(cfg.ssh.connect_timeout_secs),
        client::connect(config, (h.addr(), port), handler),
    )
    .await
    .map_err(|_| anyhow!("connect timed out"))?
    .map_err(|e| anyhow!("connect: {e}"))?;

    let mut authed = false;
    let rsa_hash = session
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten();

    // 1. ssh-agent
    if let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await {
        if let Ok(ids) = agent.request_identities().await {
            for id in ids {
                let key = id.public_key().into_owned();
                match session
                    .authenticate_publickey_with(&user, key, rsa_hash, &mut agent)
                    .await
                {
                    Ok(r) if r.success() => {
                        authed = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    // 2. key files
    if !authed {
        let mut candidates = vec![];
        if let Some(k) = &h.key {
            candidates.push(paths::expand(k));
        }
        for k in ["~/.ssh/id_ed25519", "~/.ssh/id_ecdsa", "~/.ssh/id_rsa"] {
            candidates.push(paths::expand(k));
        }
        for p in candidates.into_iter().filter(|p| p.exists()) {
            let Ok(key) = load_secret_key(&p, None) else {
                continue;
            };
            let r = session
                .authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash))
                .await?;
            if r.success() {
                authed = true;
                break;
            }
        }
    }
    if !authed {
        bail!(
            "authentication failed for {user}@{} (tried ssh-agent and key files)",
            h.addr()
        );
    }

    let mut ch = session.channel_open_session().await?;
    ch.exec(true, command).await?;
    let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), None);
    while let Some(msg) = ch.wait().await {
        match msg {
            ChannelMsg::Data { data } => out.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => err.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
            _ => {}
        }
    }
    let _ = session
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;
    Ok((code, out, err))
}
