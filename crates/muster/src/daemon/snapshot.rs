//! Undo snapshots via a per-task *shadow* git repository.
//!
//! The shadow repo lives in `~/.muster/snapshots/<task>.git` and points its
//! work tree at the task directory with `--work-tree`, so it never touches the
//! user's own repo, refs, index or stash. It still honours the project's
//! `.gitignore`. Each snapshot is a commit on `refs/heads/timeline`; identical
//! trees are deduplicated, so snapshotting often is cheap.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use muster_core::proto::SnapshotInfo;

const TIMELINE: &str = "refs/heads/timeline";
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub struct Store {
    git_dir: PathBuf,
    work: PathBuf,
    lock: Mutex<()>,
}

impl Store {
    pub fn open(git_dir: PathBuf, work: &Path, excludes: &[String]) -> Result<Store> {
        let s = Store {
            git_dir,
            work: work.to_path_buf(),
            lock: Mutex::new(()),
        };
        if !s.git_dir.join("HEAD").exists() {
            std::fs::create_dir_all(&s.git_dir)?;
            let out = Command::new("git")
                .arg("init")
                .arg("--bare")
                .arg("-q")
                .arg(&s.git_dir)
                .output()?;
            if !out.status.success() {
                bail!("git init failed: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
        let info = s.git_dir.join("info");
        std::fs::create_dir_all(&info)?;
        let mut ex = String::from("# managed by muster\n");
        for e in excludes {
            ex.push_str(e);
            ex.push('\n');
        }
        std::fs::write(info.join("exclude"), ex)?;
        Ok(s)
    }

    fn git(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.autocrlf=false",
            ])
            .args([
                "-c",
                "gc.auto=0",
                "-c",
                "core.quotepath=off",
                "-c",
                "advice.addEmbeddedRepo=false",
            ])
            .env("GIT_AUTHOR_NAME", "muster")
            .env("GIT_AUTHOR_EMAIL", "muster@localhost")
            .env("GIT_COMMITTER_NAME", "muster")
            .env("GIT_COMMITTER_EMAIL", "muster@localhost")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .current_dir(&self.work);
        c
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = self
            .git()
            .args(args)
            .output()
            .with_context(|| format!("git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn head(&self) -> Option<String> {
        self.run(&["rev-parse", "-q", "--verify", TIMELINE])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Record the current state. Returns None if nothing changed since the last snapshot.
    pub fn take(&self, label: &str) -> Result<Option<SnapshotInfo>> {
        let _g = self.lock.lock().unwrap();
        self.take_locked(label, false)
    }

    fn take_locked(&self, label: &str, force: bool) -> Result<Option<SnapshotInfo>> {
        // --ignore-errors: unreadable files shouldn't abort the snapshot.
        let _ = self.run(&["add", "-A", "--ignore-errors", "."]);
        let tree = self.run(&["write-tree"])?.trim().to_string();
        let parent = self.head();
        if let Some(p) = &parent {
            let ptree = self
                .run(&["rev-parse", &format!("{p}^{{tree}}")])?
                .trim()
                .to_string();
            if ptree == tree && !force {
                return Ok(None);
            }
        }
        let label = sanitize(label);
        let mut args = vec!["commit-tree", tree.as_str(), "-m", label.as_str()];
        if let Some(p) = &parent {
            args.push("-p");
            args.push(p.as_str());
        }
        let commit = self.run(&args)?.trim().to_string();
        self.run(&["update-ref", TIMELINE, &commit])?;
        self.info(&commit).map(Some)
    }

    fn info(&self, commit: &str) -> Result<SnapshotInfo> {
        let meta = self.run(&["log", "-1", "--format=%ct%x1f%s", commit])?;
        let (ct, subj) = meta.trim_end().split_once('\x1f').unwrap_or(("0", ""));
        let seq: u32 = self
            .run(&["rev-list", "--count", commit])?
            .trim()
            .parse()
            .unwrap_or(0);
        let base = format!("{commit}^");
        let from = if seq <= 1 { EMPTY_TREE } else { base.as_str() };
        let stat = self
            .run(&["diff", "--shortstat", from, commit])
            .unwrap_or_default();
        let (f, ins, del) = parse_shortstat(&stat);
        Ok(SnapshotInfo {
            id: commit.to_string(),
            seq,
            label: subj.to_string(),
            unix: ct.parse().unwrap_or(0),
            files_changed: f,
            insertions: ins,
            deletions: del,
        })
    }

    /// Newest first.
    pub fn list(&self) -> Result<Vec<SnapshotInfo>> {
        let Some(head) = self.head() else {
            return Ok(vec![]);
        };
        let out = self.run(&["log", "--format=@@%H%x1f%ct%x1f%s", "--shortstat", &head])?;
        let total: u32 = self
            .run(&["rev-list", "--count", &head])?
            .trim()
            .parse()
            .unwrap_or(0);
        let mut v: Vec<SnapshotInfo> = Vec::new();
        for line in out.lines() {
            if let Some(h) = line.strip_prefix("@@") {
                let mut it = h.splitn(3, '\x1f');
                let id = it.next().unwrap_or("").to_string();
                let ct = it.next().unwrap_or("0").parse().unwrap_or(0);
                let label = it.next().unwrap_or("").to_string();
                let seq = total.saturating_sub(v.len() as u32);
                v.push(SnapshotInfo {
                    id,
                    seq,
                    label,
                    unix: ct,
                    files_changed: 0,
                    insertions: 0,
                    deletions: 0,
                });
            } else if line.contains("changed") {
                if let Some(last) = v.last_mut() {
                    let (f, i, d) = parse_shortstat(line);
                    last.files_changed = f;
                    last.insertions = i;
                    last.deletions = d;
                }
            }
        }
        // The root commit has no parent so --shortstat prints nothing for it; compute vs empty tree.
        if let Some(root) = v.last_mut() {
            if root.files_changed == 0 {
                let (f, i, d) = parse_shortstat(
                    &self
                        .run(&["diff", "--shortstat", EMPTY_TREE, &root.id])
                        .unwrap_or_default(),
                );
                root.files_changed = f;
                root.insertions = i;
                root.deletions = d;
            }
        }
        Ok(v)
    }

    pub fn count(&self) -> u32 {
        self.head()
            .and_then(|h| self.run(&["rev-list", "--count", &h]).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn resolve(&self, r: &str) -> Result<String> {
        if r == "base" || r == "first" {
            let head = self.head().ok_or_else(|| anyhow!("no snapshots yet"))?;
            let roots = self.run(&["rev-list", "--max-parents=0", &head])?;
            return Ok(roots.lines().next().unwrap_or("").to_string());
        }
        if let Some(c) = r.strip_suffix('^') {
            let c = self.resolve(c)?;
            return Ok(self
                .run(&["rev-parse", "-q", "--verify", &format!("{c}^")])
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| EMPTY_TREE.to_string()));
        }
        if let Ok(n) = r.trim_start_matches('#').parse::<u32>() {
            if let Some(s) = self.list()?.into_iter().find(|s| s.seq == n) {
                return Ok(s.id);
            }
        }
        let out = self
            .run(&["rev-parse", "-q", "--verify", &format!("{r}^{{commit}}")])
            .map_err(|_| anyhow!("unknown snapshot `{r}`"))?;
        Ok(out.trim().to_string())
    }

    /// Unified diff. `from` defaults to the first snapshot; `to` to the live working tree.
    pub fn diff(&self, from: Option<&str>, to: Option<&str>) -> Result<String> {
        let _g = self.lock.lock().unwrap();
        let to = match to {
            Some(t) => self.resolve(t)?,
            None => {
                let _ = self.take_locked("checkpoint", false)?;
                self.head().ok_or_else(|| anyhow!("no snapshots yet"))?
            }
        };
        let from = match from {
            Some(f) => self.resolve(f)?,
            None => self.resolve("base")?,
        };
        let stat = self.run(&["diff", "--stat", &from, &to])?;
        let patch = self.run(&["diff", "--no-color", &from, &to])?;
        Ok(format!("{stat}\n{patch}"))
    }

    /// Make the working tree match `snapshot` (non-ignored files only).
    /// A safety snapshot is taken first, so a revert can itself be reverted.
    pub fn restore(&self, snapshot: &str) -> Result<SnapshotInfo> {
        let _g = self.lock.lock().unwrap();
        let target = self.resolve(snapshot)?;
        let tinfo = self.info(&target)?;
        self.take_locked(&format!("before revert to #{}", tinfo.seq), false)?;

        // Index now mirrors the live tree.
        let current: HashSet<String> = self
            .run(&["ls-files", "-z"])?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let wanted: HashSet<String> = self
            .run(&["ls-tree", "-r", "-z", "--name-only", &target])?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        for f in current.difference(&wanted) {
            let p = self.work.join(f);
            if p.is_file() || p.is_symlink() {
                let _ = std::fs::remove_file(&p);
                prune_empty_dirs(&self.work, p.parent());
            }
        }
        self.run(&["read-tree", &target])?;
        self.run(&["checkout-index", "-a", "-f"])?;
        let after = self.take_locked(
            &format!("reverted to #{}: {}", tinfo.seq, tinfo.label),
            true,
        )?;
        after.ok_or_else(|| anyhow!("revert produced no snapshot"))
    }
}

fn prune_empty_dirs(root: &Path, mut dir: Option<&Path>) {
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) {
            break;
        }
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn sanitize(label: &str) -> String {
    let one_line: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let t = one_line.trim();
    let t = if t.is_empty() { "snapshot" } else { t };
    if t.chars().count() > 120 {
        format!("{}…", t.chars().take(119).collect::<String>())
    } else {
        t.to_string()
    }
}

fn parse_shortstat(s: &str) -> (u32, u32, u32) {
    let (mut f, mut i, mut d) = (0, 0, 0);
    for part in s.split(',') {
        let part = part.trim();
        let n: u32 = part
            .split_whitespace()
            .next()
            .and_then(|x| x.parse().ok())
            .unwrap_or(0);
        if part.contains("file") {
            f = n;
        } else if part.contains("insertion") {
            i = n;
        } else if part.contains("deletion") {
            d = n;
        }
    }
    (f, i, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_dedupe_diff_restore() {
        let base = std::env::temp_dir().join(format!("muster-snap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let work = base.join("work");
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        std::fs::write(work.join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(work.join("ignored.log"), "noise\n").unwrap();

        let s = Store::open(base.join("shadow.git"), &work, &["node_modules/".into()]).unwrap();
        let s1 = s.take("start").unwrap().expect("first snapshot");
        assert_eq!(s1.seq, 1);
        assert!(s.take("again").unwrap().is_none(), "dedupe");

        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        std::fs::write(work.join("src/new.rs"), "fn main(){}\n").unwrap();
        let s2 = s.take("edit").unwrap().unwrap();
        assert_eq!(s2.seq, 2);
        assert_eq!(s2.files_changed, 2);

        let d = s.diff(None, None).unwrap();
        assert!(d.contains("+two"));
        assert!(d.contains("src/new.rs"));

        // agent wrecks things
        std::fs::remove_file(work.join("a.txt")).unwrap();
        std::fs::write(work.join("junk.txt"), "junk").unwrap();

        let r = s.restore(&s1.id).unwrap();
        assert!(r.label.starts_with("reverted to #1"));
        assert_eq!(
            std::fs::read_to_string(work.join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(!work.join("junk.txt").exists());
        assert!(!work.join("src").exists(), "emptied dir pruned");
        assert!(work.join("ignored.log").exists(), "ignored files untouched");

        let list = s.list().unwrap();
        assert_eq!(list.len(), 4); // start, edit, before revert, reverted
        assert_eq!(list[0].seq, 4);
        assert_eq!(list.last().unwrap().seq, 1);

        // undo the undo
        let before = list
            .iter()
            .find(|x| x.label.starts_with("before revert"))
            .unwrap();
        s.restore(&format!("#{}", before.seq)).unwrap();
        assert!(work.join("junk.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
