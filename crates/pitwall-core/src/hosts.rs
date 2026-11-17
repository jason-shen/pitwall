//! Host selectors: `all`, `name`, `web-*`, `tag:prod`, `a,b,tag:db`.

use crate::config::HostConfig;
use crate::policy::wildcard;

pub fn select<'a>(hosts: &'a [HostConfig], selector: &str) -> Vec<&'a HostConfig> {
    let parts: Vec<&str> = selector
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    hosts
        .iter()
        .filter(|h| {
            parts.iter().any(|p| {
                if *p == "all" || *p == "*" {
                    true
                } else if let Some(tag) = p.strip_prefix("tag:") {
                    h.tags.iter().any(|t| wildcard(tag, t))
                } else {
                    wildcard(p, &h.name)
                }
            })
        })
        .collect()
}

/// Configured hosts plus the aliases in ~/.ssh/config, read fresh so hosts
/// added while the daemon runs show up. `fallback` is used when config.toml
/// doesn't parse.
pub fn current(fallback: &[HostConfig]) -> Vec<HostConfig> {
    let configured = crate::config::Config::load()
        .map(|c| c.hosts)
        .unwrap_or_else(|_| fallback.to_vec());
    let text = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".ssh/config")).ok())
        .unwrap_or_default();
    merge(configured, ssh_config_hosts(&text))
}

fn merge(mut hosts: Vec<HostConfig>, extra: Vec<HostConfig>) -> Vec<HostConfig> {
    for e in extra {
        if !hosts.iter().any(|h| h.name == e.name || h.addr() == e.name) {
            hosts.push(e);
        }
    }
    hosts
}

/// `Host` aliases that name one machine, with the HostName under each.
/// Patterns (`*`, `?`, `!`) only set options for others, so they're skipped.
/// Includes aren't followed.
fn ssh_config_hosts(text: &str) -> Vec<HostConfig> {
    let mut out: Vec<HostConfig> = vec![];
    // The aliases the current Host line added, for its HostName to go to.
    let mut block: Vec<usize> = vec![];
    for line in text.lines() {
        let mut words = line
            .trim()
            .split(|c: char| c.is_whitespace() || c == '=')
            .filter(|w| !w.is_empty());
        let Some(key) = words.next() else {
            continue;
        };
        if key.eq_ignore_ascii_case("match") {
            block.clear();
        } else if key.eq_ignore_ascii_case("hostname") {
            if let Some(v) = words.next() {
                for &i in &block {
                    out[i].hostname.get_or_insert_with(|| v.to_string());
                }
            }
        } else if key.eq_ignore_ascii_case("host") {
            block.clear();
            for name in words {
                if name.contains(['*', '?', '!']) {
                    continue;
                }
                if let Some(i) = out.iter().position(|h| h.name == name) {
                    block.push(i);
                    continue;
                }
                block.push(out.len());
                out.push(HostConfig {
                    name: name.to_string(),
                    addr: None,
                    port: None,
                    user: None,
                    key: None,
                    tags: vec!["ssh-config".into()],
                    hostname: None,
                });
            }
        }
    }
    out
}

/// Arguments for an interactive `ssh` to `h`. ssh reads ~/.ssh/config
/// itself, so an alias brings its own user, key and ProxyJump.
pub fn ssh_args(h: &HostConfig) -> Vec<String> {
    let mut a = vec![];
    if let Some(p) = h.port {
        a.extend(["-p".into(), p.to_string()]);
    }
    if let Some(k) = &h.key {
        a.extend(["-i".into(), crate::paths::expand(k).display().to_string()]);
    }
    if let Some(u) = &h.user {
        a.extend(["-l".into(), u.clone()]);
    }
    a.extend(["--".into(), h.addr().to_string()]);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_aliases() {
        let text = "Host *\n  ServerAliveInterval 30\nHost box  box\n  HostName 10.0.0.2\nhost=nas !bad\nMatch host x\nHost web-?\n";
        let found = ssh_config_hosts(text);
        let names: Vec<_> = found.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["box", "nas"]);
        assert_eq!(found[0].shown_addr(), "10.0.0.2");
        assert_eq!(found[1].shown_addr(), "nas");
        let merged = merge(vec![h("box", &[])], ssh_config_hosts(text));
        assert_eq!(merged.len(), 2);
    }

    fn h(name: &str, tags: &[&str]) -> HostConfig {
        HostConfig {
            name: name.into(),
            addr: None,
            port: None,
            user: None,
            key: None,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            hostname: None,
        }
    }

    #[test]
    fn selectors() {
        let hs = vec![
            h("web-1", &["prod", "web"]),
            h("web-2", &["staging"]),
            h("db-1", &["prod", "db"]),
        ];
        let names = |s: &str| {
            select(&hs, s)
                .iter()
                .map(|h| h.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("all").len(), 3);
        assert_eq!(names("web-*"), vec!["web-1", "web-2"]);
        assert_eq!(names("tag:prod"), vec!["web-1", "db-1"]);
        assert_eq!(names("db-1,tag:staging"), vec!["web-2", "db-1"]);
        assert!(names("nope").is_empty());
    }
}
