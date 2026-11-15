//! Git plumbing for the TUI: status, diffs parsed into hunks, hunk staging, commit, merge.
//! Everything shells out to `git`, so it behaves exactly like the user's own git.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    c
}

fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git(dir)
        .args(args)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = if err.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout).to_string()
        } else {
            err.to_string()
        };
        bail!("{}", msg.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn root(dir: &Path) -> Option<PathBuf> {
    run(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|s| PathBuf::from(s.trim()))
}

pub fn current_branch(dir: &Path) -> Option<String> {
    run(dir, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: String,
    /// Index status (staged), from porcelain X.
    pub x: char,
    /// Worktree status (unstaged), from porcelain Y.
    pub y: char,
}

impl Change {
    pub fn staged(&self) -> bool {
        self.x != ' ' && self.x != '?'
    }
    pub fn unstaged(&self) -> bool {
        self.y != ' '
    }
    pub fn untracked(&self) -> bool {
        self.x == '?'
    }
    /// One letter for display.
    pub fn letter(&self) -> char {
        if self.untracked() {
            'U'
        } else if self.y != ' ' {
            self.y
        } else {
            self.x
        }
    }
}

/// `git status --porcelain` for `dir`, paths relative to `dir`.
pub fn status(dir: &Path) -> Result<Vec<Change>> {
    let out = run(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )?;
    let prefix = run(dir, &["rev-parse", "--show-prefix"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut v = Vec::new();
    let mut parts = out.split('\0');
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let mut ch = entry.chars();
        let x = ch.next().unwrap_or(' ');
        let y = ch.next().unwrap_or(' ');
        let path = entry[3..].to_string();
        if x == 'R' || x == 'C' {
            parts.next(); // original path
        }
        // porcelain paths are repo-relative; make them dir-relative
        let path = path
            .strip_prefix(&prefix)
            .map(str::to_string)
            .unwrap_or(path);
        v.push(Change { path, x, y });
    }
    Ok(v)
}

/// Files for the tree: tracked + untracked-not-ignored, relative to `dir`.
pub fn list_files(dir: &Path) -> Option<Vec<String>> {
    let out = run(dir, &["ls-files", "-co", "--exclude-standard", "-z"]).ok()?;
    let mut v: Vec<String> = out
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    v.sort();
    v.dedup();
    Some(v)
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffMode {
    /// Working tree vs index.
    Unstaged,
    /// Index vs HEAD.
    Staged,
    /// Working tree (incl. untracked) vs a base commit: everything a task did.
    Base(String),
}

impl DiffMode {
    pub fn label(&self) -> &'static str {
        match self {
            DiffMode::Unstaged => "unstaged",
            DiffMode::Staged => "staged",
            DiffMode::Base(_) => "vs base",
        }
    }
}

pub fn diff(dir: &Path, mode: &DiffMode, path: Option<&str>) -> Result<String> {
    let mut args: Vec<String> = vec!["diff".into(), "--no-color".into(), "--no-ext-diff".into()];
    match mode {
        DiffMode::Unstaged => {}
        DiffMode::Staged => args.push("--cached".into()),
        DiffMode::Base(b) => args.push(b.clone()),
    }
    args.push("--".into());
    args.push(path.unwrap_or(".").into());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut text = run(dir, &argv)?;

    // Untracked files never show up in `git diff`; render them as new files.
    if mode != &DiffMode::Staged {
        let untracked = run(
            dir,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                path.unwrap_or("."),
            ],
        )
        .unwrap_or_default();
        for f in untracked.split('\0').filter(|s| !s.is_empty()) {
            let out = git(dir)
                .args(["diff", "--no-color", "--no-index", "--", "/dev/null", f])
                .output();
            if let Ok(o) = out {
                text.push_str(&String::from_utf8_lossy(&o.stdout));
            }
        }
    }
    Ok(text)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineKind {
    FileHeader,
    Meta,
    Hunk,
    Add,
    Del,
    Context,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    pub file: Option<String>,
    pub hunk: Option<usize>,
    /// Line number in the new file (for jumping into the editor).
    pub new_line: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub file: String,
    /// `diff --git`, `index`, `---`, `+++` lines of the file this hunk belongs to.
    pub header: Vec<String>,
    /// The `@@` line and its body.
    pub body: Vec<String>,
}

impl Hunk {
    pub fn patch(&self) -> String {
        let mut s = String::new();
        for l in self.header.iter().chain(self.body.iter()) {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

#[derive(Default)]
pub struct ParsedDiff {
    pub lines: Vec<DiffLine>,
    pub hunks: Vec<Hunk>,
    pub files: Vec<(String, u32, u32)>,
}

fn path_from_header(l: &str) -> Option<String> {
    // "diff --git a/x b/x" -> x (best effort; quoted paths keep their quotes stripped)
    let rest = l.strip_prefix("diff --git ")?;
    let b = rest.rsplit_once(" b/").map(|(_, b)| b).unwrap_or(rest);
    Some(b.trim_matches('"').to_string())
}

pub fn parse(text: &str) -> ParsedDiff {
    let mut lines = Vec::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut files: Vec<(String, u32, u32)> = Vec::new();
    let mut file: Option<String> = None;
    let mut header: Vec<String> = Vec::new();
    let mut in_header = false;
    let mut new_line = 0usize;

    for raw in text.lines() {
        if raw.starts_with("diff --git ") {
            file = path_from_header(raw);
            header = vec![raw.to_string()];
            in_header = true;
            if let Some(f) = &file {
                files.push((f.clone(), 0, 0));
            }
            lines.push(DiffLine {
                kind: LineKind::FileHeader,
                text: raw.to_string(),
                file: file.clone(),
                hunk: None,
                new_line: None,
            });
            continue;
        }
        if in_header && !raw.starts_with("@@") {
            if let Some(p) = raw.strip_prefix("+++ b/") {
                file = Some(p.to_string());
                if let Some(last) = files.last_mut() {
                    last.0 = p.to_string();
                }
            }
            header.push(raw.to_string());
            lines.push(DiffLine {
                kind: LineKind::Meta,
                text: raw.to_string(),
                file: file.clone(),
                hunk: None,
                new_line: None,
            });
            continue;
        }
        if raw.starts_with("@@") {
            in_header = false;
            // @@ -a,b +c,d @@
            new_line = raw
                .split('+')
                .nth(1)
                .and_then(|s| s.split([',', ' ']).next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            hunks.push(Hunk {
                file: file.clone().unwrap_or_default(),
                header: header.clone(),
                body: vec![raw.to_string()],
            });
            lines.push(DiffLine {
                kind: LineKind::Hunk,
                text: raw.to_string(),
                file: file.clone(),
                hunk: Some(hunks.len() - 1),
                new_line: Some(new_line),
            });
            continue;
        }
        let Some(h) = hunks.last_mut() else {
            lines.push(DiffLine {
                kind: LineKind::Meta,
                text: raw.to_string(),
                file: file.clone(),
                hunk: None,
                new_line: None,
            });
            continue;
        };
        h.body.push(raw.to_string());
        let idx = hunks.len() - 1;
        let (kind, nl) = match raw.chars().next() {
            Some('+') => {
                if let Some(f) = files.last_mut() {
                    f.1 += 1;
                }
                new_line += 1;
                (LineKind::Add, Some(new_line - 1))
            }
            Some('-') => {
                if let Some(f) = files.last_mut() {
                    f.2 += 1;
                }
                (LineKind::Del, Some(new_line))
            }
            Some('\\') => (LineKind::Meta, None),
            _ => {
                new_line += 1;
                (LineKind::Context, Some(new_line - 1))
            }
        };
        lines.push(DiffLine {
            kind,
            text: raw.to_string(),
            file: file.clone(),
            hunk: Some(idx),
            new_line: nl,
        });
    }
    ParsedDiff {
        lines,
        hunks,
        files,
    }
}

fn apply_cached(dir: &Path, patch: &str, reverse: bool) -> Result<()> {
    apply(dir, patch, reverse, true)
}

fn apply(dir: &Path, patch: &str, reverse: bool, cached: bool) -> Result<()> {
    let root = root(dir).unwrap_or_else(|| dir.to_path_buf());
    let mut c = git(&root);
    c.arg("apply");
    if cached {
        c.arg("--cached");
    }
    c.args(["--recount", "--whitespace=nowarn"]);
    if reverse {
        c.arg("-R");
    }
    c.arg("-");
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(patch.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("git apply: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn stage_hunk(dir: &Path, h: &Hunk) -> Result<()> {
    if h.header.iter().any(|l| l.starts_with("--- /dev/null")) {
        // new/untracked file: stage the whole file
        return stage_file(dir, &h.file_rel(dir));
    }
    apply_cached(dir, &h.patch(), false)
}

pub fn unstage_hunk(dir: &Path, h: &Hunk) -> Result<()> {
    apply_cached(dir, &h.patch(), true)
}

/// Undo one hunk in the working tree. The hunk has to be from a diff whose
/// new side is the working tree (unstaged, or against the agent's base).
pub fn discard_hunk(dir: &Path, h: &Hunk) -> Result<()> {
    apply(dir, &h.patch(), true, false)
}

impl Hunk {
    /// Hunk paths are repo-relative; convert to `dir`-relative for pathspecs.
    fn file_rel(&self, dir: &Path) -> String {
        let prefix = run(dir, &["rev-parse", "--show-prefix"])
            .unwrap_or_default()
            .trim()
            .to_string();
        self.file
            .strip_prefix(&prefix)
            .unwrap_or(&self.file)
            .to_string()
    }
}

pub fn stage_file(dir: &Path, path: &str) -> Result<()> {
    run(dir, &["add", "-A", "--", path]).map(|_| ())
}

pub fn unstage_file(dir: &Path, path: &str) -> Result<()> {
    if run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).is_ok() {
        run(dir, &["reset", "-q", "--", path]).map(|_| ())
    } else {
        run(dir, &["rm", "--cached", "-q", "-r", "--", path]).map(|_| ())
    }
}

pub fn stage_all(dir: &Path) -> Result<()> {
    run(dir, &["add", "-A", "--", "."]).map(|_| ())
}

pub fn discard_file(dir: &Path, c: &Change) -> Result<()> {
    if c.untracked() {
        std::fs::remove_file(dir.join(&c.path))?;
        Ok(())
    } else {
        run(dir, &["checkout", "-q", "--", &c.path]).map(|_| ())
    }
}

pub fn commit(dir: &Path, message: &str) -> Result<String> {
    let out = run(dir, &["commit", "-q", "-m", message])?;
    let head = run(dir, &["log", "-1", "--format=%h %s"]).unwrap_or_default();
    Ok(format!("{}{}", out.trim(), head.trim()))
}

/// Untracked files in the main checkout that the agent's work would bring in
/// (committed on its branch, or still lying in its worktree); git refuses
/// to merge over them.
fn untracked_in_the_way(
    root: &Path,
    worktree: &Path,
    target: &str,
    branch: &str,
) -> Result<Vec<String>> {
    let untracked: std::collections::HashSet<String> =
        run(root, &["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .map(String::from)
            .collect();
    if untracked.is_empty() {
        return Ok(vec![]);
    }
    let mut incoming: Vec<String> = run(
        root,
        &[
            "diff",
            "--name-only",
            "--diff-filter=AR",
            &format!("{target}...{branch}"),
        ],
    )?
    .lines()
    .map(String::from)
    .collect();
    if worktree.is_dir() {
        incoming.extend(status(worktree)?.into_iter().map(|c| c.path));
    }
    let mut hit: Vec<String> = incoming
        .into_iter()
        .filter(|p| untracked.contains(p))
        .collect();
    hit.sort();
    hit.dedup();
    Ok(hit)
}

fn ensure_clean(repo_root: &Path, target: &str) -> Result<()> {
    let dirty = run(
        repo_root,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    if !dirty.trim().is_empty() {
        bail!(
            "{} has uncommitted changes on {target}; commit or stash them first",
            repo_root.display()
        );
    }
    Ok(())
}

/// Bring an agent's work home: commit what it left uncommitted in its
/// worktree, then merge its branch into `target` in the main checkout.
pub fn accept(
    root: &Path,
    worktree: &Path,
    branch: &str,
    target: &str,
    message: &str,
) -> Result<String> {
    if current_branch(root).as_deref() != Some(target) {
        bail!(
            "{} isn't on {target}; check it out, then accept again",
            root.display()
        );
    }
    // Everything that can refuse the merge is checked before committing, so
    // a refusal doesn't leave a commit behind in the agent's branch.
    ensure_clean(root, target)?;
    let in_the_way = untracked_in_the_way(root, worktree, target, branch)?;
    if !in_the_way.is_empty() {
        bail!(
            "{} has untracked files the merge would overwrite: {}; move them, then accept again",
            root.display(),
            in_the_way.join(", ")
        );
    }
    let mut out = String::new();
    if worktree.is_dir() && !status(worktree)?.is_empty() {
        if message.trim().is_empty() {
            bail!("the agent left uncommitted work; it needs a commit message");
        }
        stage_all(worktree)?;
        out.push_str(&format!(
            "committed {}; ",
            commit(worktree, message.trim())?
        ));
    }
    out.push_str(&merge(root, branch)?);
    Ok(out)
}

/// Files, added and removed lines between `base` and the working tree,
/// untracked files included: the size of what an agent did.
pub fn shortstat(dir: &Path, base: &str) -> Result<(usize, usize, usize)> {
    let (mut files, mut add, mut del) = (0, 0, 0);
    for l in run(dir, &["diff", "--numstat", base])?.lines() {
        let mut f = l.split('\t');
        // Binary files show `-` for both counts.
        let a = f.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        let d = f.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        files += 1;
        add += a;
        del += d;
    }
    for p in run(dir, &["ls-files", "--others", "--exclude-standard"])?.lines() {
        files += 1;
        if let Ok(meta) = std::fs::metadata(dir.join(p)) {
            // Counting a huge generated file's lines isn't worth the read.
            if meta.len() < 1 << 20 {
                add += std::fs::read(dir.join(p))
                    .map(|b| b.iter().filter(|c| **c == b'\n').count())
                    .unwrap_or(0);
            }
        }
    }
    Ok((files, add, del))
}

/// Merge `branch` into whatever is checked out in `repo_root`.
pub fn merge(repo_root: &Path, branch: &str) -> Result<String> {
    let target = current_branch(repo_root).unwrap_or_else(|| "HEAD".into());
    ensure_clean(repo_root, &target)?;
    match run(
        repo_root,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            &format!("Merge {branch}"),
            branch,
        ],
    ) {
        Ok(_) => Ok(format!("merged {branch} into {target}")),
        Err(e) => {
            let conflicted =
                run(repo_root, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            if conflicted.trim().is_empty() {
                Err(e)
            } else {
                bail!(
                    "merge conflicts in {} ({}); resolve them there or run `git merge --abort`",
                    repo_root.display(),
                    conflicted.lines().collect::<Vec<_>>().join(", ")
                )
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    /// Short hash and subject of its tip.
    pub tip: String,
    pub when: String,
}

/// Local branches, most recently committed first.
pub fn branches(dir: &Path) -> Result<Vec<Branch>> {
    let out = run(
        dir,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(HEAD)%00%(refname:short)%00%(objectname:short) %(subject)%00%(committerdate:relative)",
            "refs/heads",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\0');
            Some(Branch {
                current: f.next()? == "*",
                name: f.next()?.to_string(),
                tip: f.next().unwrap_or_default().to_string(),
                when: f.next().unwrap_or_default().to_string(),
            })
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub when: String,
}

/// The newest `n` commits reachable from `rev`.
pub fn log(dir: &Path, rev: &str, n: usize) -> Result<Vec<Commit>> {
    let out = run(
        dir,
        &[
            "log",
            &format!("-n{n}"),
            "--format=%H%x00%h%x00%s%x00%an%x00%cr",
            rev,
            "--",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\0');
            Some(Commit {
                hash: f.next()?.to_string(),
                short: f.next()?.to_string(),
                subject: f.next().unwrap_or_default().to_string(),
                author: f.next().unwrap_or_default().to_string(),
                when: f.next().unwrap_or_default().to_string(),
            })
        })
        .collect())
}

/// One commit's patch, in the same shape `diff` returns.
pub fn show(dir: &Path, rev: &str) -> Result<String> {
    run(
        dir,
        &[
            "show",
            "--no-color",
            "--no-ext-diff",
            "--format=commit %H%nAuthor: %an <%ae>%nDate:   %cd%n%n    %s%n",
            rev,
        ],
    )
}

pub fn checkout(dir: &Path, branch: &str) -> Result<String> {
    run(dir, &["checkout", "-q", branch])?;
    Ok(format!("checked out {branch}"))
}

pub fn ahead_count(dir: &Path, base: &str) -> usize {
    run(dir, &["rev-list", "--count", &format!("{base}..HEAD")])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "muster-git-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        run(&d, &["init", "-q", "-b", "main"]).unwrap();
        run(&d, &["config", "user.email", "t@t"]).unwrap();
        run(&d, &["config", "user.name", "t"]).unwrap();
        std::fs::write(
            d.join("a.txt"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n",
        )
        .unwrap();
        run(&d, &["add", "."]).unwrap();
        run(&d, &["commit", "-qm", "init"]).unwrap();
        d
    }

    #[test]
    fn accept_commits_the_agents_leftovers_and_merges_them_home() {
        let d = repo();
        let base = run(&d, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let wt = d.with_extension("wt");
        let _ = std::fs::remove_dir_all(&wt);
        run(
            &d,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "muster/x",
                wt.to_str().unwrap(),
                &base,
            ],
        )
        .unwrap();
        // One change committed by the agent, two left lying around.
        std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
        run(&wt, &["commit", "-qam", "agent work"]).unwrap();
        std::fs::write(wt.join("b.txt"), "new\nfile\n").unwrap();
        std::fs::write(wt.join("a.txt"), "changed again\n").unwrap();
        assert_eq!(shortstat(&wt, &base).unwrap(), (2, 3, 15));

        // Not on the target branch: refuse rather than merge somewhere else.
        assert!(accept(&d, &wt, "muster/x", "release", "msg").is_err());
        // Uncommitted work needs a message.
        assert!(accept(&d, &wt, "muster/x", "main", "  ").is_err());
        // An untracked file where the agent adds one refuses before anything
        // is committed.
        std::fs::write(d.join("b.txt"), "mine\n").unwrap();
        let head = run(&wt, &["rev-parse", "HEAD"]).unwrap();
        let err = accept(&d, &wt, "muster/x", "main", "Add b")
            .unwrap_err()
            .to_string();
        assert!(err.contains("b.txt"), "{err}");
        assert_eq!(run(&wt, &["rev-parse", "HEAD"]).unwrap(), head);
        std::fs::remove_file(d.join("b.txt")).unwrap();
        // A dirty main checkout refuses before anything is committed.
        std::fs::write(d.join("a.txt"), "mine\n").unwrap();
        let head = run(&wt, &["rev-parse", "HEAD"]).unwrap();
        assert!(accept(&d, &wt, "muster/x", "main", "Add b").is_err());
        assert_eq!(
            run(&wt, &["rev-parse", "HEAD"]).unwrap(),
            head,
            "a refused accept committed"
        );
        run(&d, &["checkout", "-q", "--", "a.txt"]).unwrap();

        let out = accept(&d, &wt, "muster/x", "main", "Add b").unwrap();
        assert!(
            out.contains("committed") && out.contains("merged muster/x into main"),
            "{out}"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("a.txt")).unwrap(),
            "changed again\n"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("b.txt")).unwrap(),
            "new\nfile\n"
        );
        assert!(status(&wt).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn one_hunk_against_the_base_can_be_thrown_away() {
        let d = repo();
        let base = run(&d, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        // Two far-apart edits make two hunks; one is committed, as an agent's
        // work often is, and the other left in the working tree.
        std::fs::write(
            d.join("a.txt"),
            "ONE\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n",
        )
        .unwrap();
        run(&d, &["commit", "-qam", "agent"]).unwrap();
        std::fs::write(
            d.join("a.txt"),
            "ONE\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\nFIFTEEN\n",
        )
        .unwrap();
        let p = parse(&diff(&d, &DiffMode::Base(base), None).unwrap());
        assert_eq!(p.hunks.len(), 2);
        discard_hunk(&d, &p.hunks[0]).unwrap();
        let now = std::fs::read_to_string(d.join("a.txt")).unwrap();
        assert!(
            now.starts_with("1\n") && now.ends_with("FIFTEEN\n"),
            "{now}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Tests run in parallel in one process, and two of them can read the
    /// same nanosecond, so a counter keeps their repos apart.
    fn rand_suffix() -> String {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{t}-{n}")
    }

    #[test]
    fn branches_log_and_show() {
        let d = repo();
        run(&d, &["checkout", "-q", "-b", "feature"]).unwrap();
        std::fs::write(d.join("b.txt"), "hi\n").unwrap();
        run(&d, &["add", "."]).unwrap();
        run(&d, &["commit", "-qm", "add b"]).unwrap();

        let bs = branches(&d).unwrap();
        let names: Vec<_> = bs.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main") && names.contains(&"feature"));
        assert!(bs.iter().find(|b| b.name == "feature").unwrap().current);

        let l = log(&d, "HEAD", 10).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].subject, "add b");
        assert_eq!(l[1].subject, "init");

        let p = parse(&show(&d, &l[0].hash).unwrap());
        assert_eq!(
            p.files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(),
            vec!["b.txt"]
        );

        checkout(&d, "main").unwrap();
        assert_eq!(current_branch(&d).as_deref(), Some("main"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hunk_staging_and_commit() {
        let d = repo();
        // two separate hunks
        std::fs::write(
            d.join("a.txt"),
            "1\nTWO\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\nFOURTEEN\n15\n",
        )
        .unwrap();
        std::fs::write(d.join("new.txt"), "hello\n").unwrap();

        let st = status(&d).unwrap();
        assert!(st.iter().any(|c| c.path == "a.txt" && c.y == 'M'));
        assert!(st.iter().any(|c| c.path == "new.txt" && c.untracked()));

        let p = parse(&diff(&d, &DiffMode::Unstaged, None).unwrap());
        let a_hunks: Vec<_> = p.hunks.iter().filter(|h| h.file == "a.txt").collect();
        assert_eq!(a_hunks.len(), 2);
        assert!(
            p.hunks.iter().any(|h| h.file == "new.txt"),
            "untracked shown as new file"
        );

        stage_hunk(&d, a_hunks[0]).unwrap();
        let staged = parse(&diff(&d, &DiffMode::Staged, None).unwrap());
        assert_eq!(staged.hunks.len(), 1);
        assert!(staged.hunks[0].body.iter().any(|l| l == "+TWO"));
        let unstaged = parse(&diff(&d, &DiffMode::Unstaged, Some("a.txt")).unwrap());
        assert_eq!(unstaged.hunks.len(), 1);
        assert!(unstaged.hunks[0].body.iter().any(|l| l == "+FOURTEEN"));

        // unstage it again, then stage file + new file
        unstage_hunk(&d, &staged.hunks[0]).unwrap();
        assert!(parse(&diff(&d, &DiffMode::Staged, None).unwrap())
            .hunks
            .is_empty());
        let newh = p.hunks.iter().find(|h| h.file == "new.txt").unwrap();
        stage_hunk(&d, newh).unwrap();
        stage_file(&d, "a.txt").unwrap();
        let msg = commit(&d, "change things").unwrap();
        assert!(msg.contains("change things"));
        assert!(status(&d).unwrap().is_empty());

        // line numbers for jump-to-editor
        let pd =
            parse("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +10,3 @@\n ctx\n+added\n-old\n");
        let added = pd.lines.iter().find(|l| l.kind == LineKind::Add).unwrap();
        assert_eq!(added.new_line, Some(11));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn merge_branch() {
        let d = repo();
        run(&d, &["checkout", "-qb", "muster/x"]).unwrap();
        std::fs::write(d.join("b.txt"), "b\n").unwrap();
        stage_all(&d).unwrap();
        commit(&d, "b").unwrap();
        run(&d, &["checkout", "-q", "main"]).unwrap();
        let m = merge(&d, "muster/x").unwrap();
        assert!(m.contains("into main"));
        assert!(d.join("b.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
