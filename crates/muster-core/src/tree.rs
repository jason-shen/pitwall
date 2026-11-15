//! File tree for the sidebar. Uses `git ls-files` in repos (so .gitignore applies),
//! a bounded directory walk elsewhere.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

const SKIP: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".DS_Store",
];
const MAX_FILES: usize = 20_000;

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub rel: String,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

/// A file list parsed into directories, built on a background thread so a big
/// repo doesn't stall the UI every refresh.
pub struct Listing {
    files: Vec<String>,
    nodes: Node,
    truncated: bool,
}

pub fn build(files: Vec<String>, truncated: bool) -> Listing {
    let mut n = Node::default();
    for f in &files {
        let parts: Vec<&str> = f.split('/').collect();
        let mut cur = &mut n;
        for d in &parts[..parts.len() - 1] {
            cur = cur.dirs.entry(d.to_string()).or_default();
        }
        cur.files.push(parts[parts.len() - 1].to_string());
    }
    for_each_node(&mut n, &mut |n| n.files.sort());
    Listing {
        files,
        nodes: n,
        truncated,
    }
}

fn for_each_node(n: &mut Node, f: &mut impl FnMut(&mut Node)) {
    f(n);
    for c in n.dirs.values_mut() {
        for_each_node(c, f);
    }
}

#[derive(Default)]
pub struct Tree {
    pub root: PathBuf,
    files: Vec<String>,
    nodes: Node,
    pub expanded: HashSet<String>,
    pub rows: Vec<Row>,
    pub truncated: bool,
}

pub fn list(dir: &Path) -> (Vec<String>, bool) {
    if let Some(v) = crate::git::list_files(dir) {
        let t = v.len() > MAX_FILES;
        return (v.into_iter().take(MAX_FILES).collect(), t);
    }
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            let p = e.path();
            let rel = p
                .strip_prefix(dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .to_string();
            match e.file_type() {
                Ok(t) if t.is_dir() => {
                    if depth < 8 {
                        stack.push((p, depth + 1));
                    }
                }
                Ok(_) => out.push(rel),
                Err(_) => {}
            }
            if out.len() >= MAX_FILES {
                out.sort();
                return (out, true);
            }
        }
    }
    out.sort();
    (out, false)
}

impl Tree {
    pub fn set(&mut self, root: &Path, files: Vec<String>, truncated: bool) {
        self.apply(root, build(files, truncated));
    }

    /// Swap in a new listing. Unchanged listings (the usual case on a poll)
    /// leave the rows alone.
    pub fn apply(&mut self, root: &Path, l: Listing) {
        if self.root == root && self.files == l.files && self.truncated == l.truncated {
            return;
        }
        if self.root != root {
            self.expanded.clear();
        }
        self.root = root.to_path_buf();
        self.truncated = l.truncated;
        self.files = l.files;
        self.nodes = l.nodes;
        self.flatten();
    }

    pub fn flatten(&mut self) {
        let mut rows = Vec::new();
        fn walk(n: &Node, prefix: &str, depth: usize, exp: &HashSet<String>, rows: &mut Vec<Row>) {
            for (name, child) in &n.dirs {
                let rel = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                let expanded = exp.contains(&rel);
                rows.push(Row {
                    rel: rel.clone(),
                    name: name.clone(),
                    depth,
                    is_dir: true,
                    expanded,
                });
                if expanded {
                    walk(child, &rel, depth + 1, exp, rows);
                }
            }
            for f in &n.files {
                let rel = if prefix.is_empty() {
                    f.clone()
                } else {
                    format!("{prefix}/{f}")
                };
                rows.push(Row {
                    rel,
                    name: f.clone(),
                    depth,
                    is_dir: false,
                    expanded: false,
                });
            }
        }
        walk(&self.nodes, "", 0, &self.expanded, &mut rows);
        self.rows = rows;
    }

    /// Every listed file, relative to the root (for fuzzy finding).
    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn toggle(&mut self, rel: &str) {
        if !self.expanded.remove(rel) {
            self.expanded.insert(rel.to_string());
        }
        self.flatten();
    }

    /// Expand all parents of `rel` so it becomes visible.
    pub fn reveal(&mut self, rel: &str) {
        let mut acc = String::new();
        let parts: Vec<&str> = rel.split('/').collect();
        for p in &parts[..parts.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(p);
            self.expanded.insert(acc.clone());
        }
        self.flatten();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_expands() {
        let mut t = Tree::default();
        t.set(
            Path::new("/x"),
            vec![
                "README.md".into(),
                "src/main.rs".into(),
                "src/tui/app.rs".into(),
            ],
            false,
        );
        let names: Vec<_> = t.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["src", "README.md"]);
        t.toggle("src");
        let names: Vec<_> = t.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["src", "tui", "main.rs", "README.md"]);
        t.reveal("src/tui/app.rs");
        assert!(t
            .rows
            .iter()
            .any(|r| r.rel == "src/tui/app.rs" && r.depth == 2));
    }
}
