//! Configuration. The user's TOML is deep-merged over `default-config.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::proto::Decision;

pub const DEFAULT_TOML: &str = include_str!("../default-config.toml");

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Config {
    pub agents: BTreeMap<String, AgentConfig>,
    pub attention: AttentionConfig,
    pub snapshots: SnapshotConfig,
    pub policy: PolicyConfig,
    pub hooks: HookConfig,
    pub ssh: SshConfig,
    pub ops: OpsConfig,
    pub tui: TuiConfig,
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TuiConfig {
    /// Prefix key, tmux-style: "C-b", "C-a", "C-space", "C-]" ...
    pub prefix: String,
    /// Ring the terminal bell when an agent starts needing you.
    pub bell: bool,
    pub tab_width: usize,
    /// syntect theme for the editor.
    pub theme: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_prompt_mode")]
    pub prompt_mode: String,
    #[serde(default)]
    pub hooks: Option<String>,
    #[serde(default = "yes")]
    pub attention_on_idle: bool,
    /// Arguments that pick up the last session when the daemon restarts the
    /// agent in its worktree (`--continue` for claude). Empty: start fresh.
    #[serde(default)]
    pub resume_args: Vec<String>,
}

fn default_prompt_mode() -> String {
    "arg".into()
}
fn yes() -> bool {
    true
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AttentionConfig {
    pub idle_secs: u64,
    pub patterns: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SnapshotConfig {
    pub auto: bool,
    pub debounce_ms: u64,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Rule {
    pub pattern: String,
    pub decision: Decision,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PolicyConfig {
    pub read_only: Decision,
    pub mutating: Decision,
    pub destructive: Decision,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub read_only_programs: Vec<String>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Config::default().policy
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct HookConfig {
    pub mode: String,
    pub timeout_secs: u64,
    pub auto_allow_read_only: bool,
    pub snapshot_before: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SshConfig {
    pub transport: String,
    pub connect_timeout_secs: u64,
    pub command_timeout_secs: u64,
    pub accept_unknown_hosts: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct OpsConfig {
    pub model: String,
    pub api_key_env: String,
    pub base_url: String,
    pub max_steps: u32,
    pub max_output_bytes: usize,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct HostConfig {
    pub name: String,
    /// Hostname or IP. Defaults to `name` (handy with ~/.ssh/config aliases).
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub user: Option<String>,
    /// Private key path (native transport, or `-i` for openssh).
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl HostConfig {
    pub fn addr(&self) -> &str {
        self.addr.as_deref().unwrap_or(&self.name)
    }
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str(DEFAULT_TOML).expect("built-in default config is valid")
    }
}

impl Config {
    /// Load `~/.config/muster/config.toml` (if present) merged over the
    /// defaults. Sections it doesn't know, like the app's `[app]` and
    /// `[keys]`, are left alone.
    pub fn load() -> Result<Config> {
        crate::paths::migrate_config();
        Self::load_from(&crate::paths::config())
    }

    pub fn load_from(path: &Path) -> Result<Config> {
        let mut base: toml::Table = toml::from_str(DEFAULT_TOML).expect("default config");
        if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?;
            let user: toml::Table =
                toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
            merge(&mut base, user);
        }
        let cfg: Config = toml::Value::Table(base)
            .try_into()
            .with_context(|| format!("invalid config in {}", path.display()))?;
        Ok(cfg)
    }
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_parses() {
        let c = Config::default();
        assert!(c.agents.contains_key("claude"));
        assert_eq!(c.policy.destructive, Decision::Deny);
    }

    #[test]
    fn user_config_merges() {
        let dir = std::env::temp_dir().join(format!("muster-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(
            &p,
            "[agents.aider]\ncommand = \"aider\"\n[attention]\nidle_secs = 2\n\
             [[hosts]]\nname = \"web-1\"\ntags = [\"prod\"]\n",
        )
        .unwrap();
        let c = Config::load_from(&p).unwrap();
        assert!(c.agents.contains_key("aider"));
        assert!(c.agents.contains_key("claude"));
        assert_eq!(c.attention.idle_secs, 2);
        assert!(!c.attention.patterns.is_empty());
        assert_eq!(c.hosts.len(), 1);
    }
}
