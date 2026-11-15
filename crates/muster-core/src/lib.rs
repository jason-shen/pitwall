//! Shared types for muster: wire protocol, config, command policy, paths.

pub mod agents;
pub mod brief;
pub mod client;
pub mod config;
pub mod editor;
pub mod git;
pub mod hosts;
pub mod kitty;
pub mod mdhl;
pub mod paths;
pub mod policy;
pub mod prompt;
pub mod proto;
pub mod tree;
pub mod wire;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
