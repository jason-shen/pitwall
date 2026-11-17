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

#[cfg(test)]
mod tests {
    use super::*;

    fn h(name: &str, tags: &[&str]) -> HostConfig {
        HostConfig {
            name: name.into(),
            addr: None,
            port: None,
            user: None,
            key: None,
            tags: tags.iter().map(|s| s.to_string()).collect(),
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
