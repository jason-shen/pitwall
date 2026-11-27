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

/// Every worktree of the repo `dir` is in, the main checkout first.
pub fn worktrees(dir: &Path) -> Vec<PathBuf> {
    run(dir, &["worktree", "list", "--porcelain"])
        .map(|s| {
            s.lines()
                .filter_map(|l| l.strip_prefix("worktree "))
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

pub fn head(dir: &Path) -> Option<String> {
    commit_of(dir, "HEAD")
}

pub fn commit_of(dir: &Path, rev: &str) -> Option<String> {
    run(
        dir,
        &["rev-parse", "-q", "--verify", &format!("{rev}^{{commit}}")],
    )
    .ok()
    .map(|s| s.trim().to_string())
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

pub fn init(dir: &Path) -> Result<()> {
    run(dir, &["init", "-q"]).map(|_| ())
}

/// Files bigger than this stay out of a new repo's first commit.
const FIRST_COMMIT_MAX: u64 = 10 * 1024 * 1024;

/// The first commit of a folder just made a repo, so agents have something
/// to branch their worktrees from. Models, datasets and other big files are
/// left out through .git/info/exclude (local, never committed) rather than
/// a .gitignore pitwall would be adding to your project.
pub fn first_commit(dir: &Path) -> Result<usize> {
    let others = run(dir, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let big: Vec<&str> = others
        .split('\0')
        .filter(|r| !r.is_empty())
        .filter(|r| {
            std::fs::symlink_metadata(dir.join(r))
                .is_ok_and(|m| m.is_file() && m.len() > FIRST_COMMIT_MAX)
        })
        .collect();
    if !big.is_empty() {
        let exclude = dir.join(".git").join("info").join("exclude");
        let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
        text.push_str("# over 10 MB, left out by pitwall\n");
        for r in &big {
            text.push('/');
            for c in r.chars() {
                if matches!(c, '\\' | '*' | '?' | '[' | '!' | '#') {
                    text.push('\\');
                }
                text.push(c);
            }
            text.push('\n');
        }
        std::fs::write(&exclude, text)?;
    }
    stage_all(dir)?;
    run(dir, &["commit", "-q", "--allow-empty", "-m", "Initial commit"])?;
    Ok(big.len())
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

/// Whether `dir` can take the merge of `branch` as it is. Git refuses
/// anything staged, and edits to files the merge changes; edits anywhere
/// else stay as they are, through the merge and through an abort.
/// `worktree` is where uncommitted work that's committed first lies.
fn ensure_mergeable(dir: &Path, target: &str, branch: &str, worktree: Option<&Path>) -> Result<()> {
    if !run(dir, &["diff", "--cached", "--name-only"])?
        .trim()
        .is_empty()
    {
        bail!(
            "{} has staged changes on {target}; commit or unstage them first",
            dir.display()
        );
    }
    let edited: std::collections::HashSet<String> = run(dir, &["diff", "--name-only"])?
        .lines()
        .map(String::from)
        .collect();
    if edited.is_empty() {
        return Ok(());
    }
    let mut incoming: Vec<String> = run(
        dir,
        &["diff", "--name-only", &format!("{target}...{branch}")],
    )?
    .lines()
    .map(String::from)
    .collect();
    if let Some(w) = worktree.filter(|w| w.is_dir()) {
        incoming.extend(status(w)?.into_iter().map(|c| c.path));
    }
    let mut hit: Vec<String> = incoming
        .into_iter()
        .filter(|p| edited.contains(p))
        .collect();
    hit.sort();
    hit.dedup();
    if !hit.is_empty() {
        bail!(
            "{} has uncommitted changes on {target} to files the merge changes: {}; commit or stash them first",
            dir.display(),
            hit.join(", ")
        );
    }
    Ok(())
}

/// Where `branch` is checked out: the main checkout, another worktree, or
/// nowhere (None).
fn checked_out_at(repo: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let want = format!("branch refs/heads/{branch}");
    let list = run(repo, &["worktree", "list", "--porcelain"])?;
    Ok(list.split("\n\n").find_map(|block| {
        let mut lines = block.lines();
        let path = lines.next()?.strip_prefix("worktree ")?;
        lines.any(|l| l == want).then(|| PathBuf::from(path))
    }))
}

/// Bring an agent's work home: commit what it left uncommitted in its
/// worktree, then merge its branch into `target`. That happens wherever
/// `target` is checked out; a branch checked out nowhere is merged in a
/// scratch worktree, so your own checkout stays on whatever it's on.
pub fn accept(
    root: &Path,
    worktree: &Path,
    branch: &str,
    target: &str,
    message: &str,
) -> Result<String> {
    if target == branch {
        bail!("can't merge {branch} into itself");
    }
    // Leftovers are committed to whatever is checked out there, so merging
    // `branch` after the agent moved off it would merge nothing and say it
    // worked.
    if worktree.is_dir() {
        match current_branch(worktree) {
            Some(on) if on != branch => bail!(
                "{} is on {on} now, not {branch}; accept {on} instead",
                worktree.display()
            ),
            None => bail!(
                "{} has a detached HEAD; check out a branch there first",
                worktree.display()
            ),
            Some(_) => {}
        }
    }
    let home = checked_out_at(root, target)?;
    if home.is_none() {
        run(root, &["rev-parse", "-q", "--verify", &format!("refs/heads/{target}")])
            .map_err(|_| anyhow::anyhow!("there's no branch {target}"))?;
    }
    // Merged some other way already. Nothing to do, and the checks below
    // would only get in the way: a dirty main checkout doesn't matter now.
    let left = worktree.is_dir() && !status(worktree)?.is_empty();
    if !left && is_merged(root, branch, target) {
        return Ok(format!("{branch} is already merged into {target}"));
    }
    // Everything that can refuse the merge is checked before committing, so
    // a refusal doesn't leave a commit behind in the agent's branch.
    if let Some(dir) = &home {
        ensure_mergeable(dir, target, branch, Some(worktree))?;
        let in_the_way = untracked_in_the_way(dir, worktree, target, branch)?;
        if !in_the_way.is_empty() {
            bail!(
                "{} has untracked files the merge would overwrite: {}; move them, then accept again",
                dir.display(),
                in_the_way.join(", ")
            );
        }
    }
    let mut out = String::new();
    if left {
        if message.trim().is_empty() {
            bail!("the agent left uncommitted work; it needs a commit message");
        }
        stage_all(worktree)?;
        out.push_str(&format!(
            "committed {}; ",
            commit(worktree, message.trim())?
        ));
    }
    match home {
        Some(dir) => out.push_str(&merge(&dir, branch)?),
        None => out.push_str(&merge_elsewhere(root, branch, target)?),
    }
    Ok(out)
}

/// Merge `branch` into `target` in a worktree made for it and removed after.
/// A conflict is backed out, since nobody is there to resolve it.
fn merge_elsewhere(root: &Path, branch: &str, target: &str) -> Result<String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!("pitwall-merge-{}-{nanos}", std::process::id()));
    let p = tmp.to_string_lossy().to_string();
    run(root, &["worktree", "add", "-q", &p, target])?;
    let merged = merge(&tmp, branch);
    let conflicted = if merged.is_err() {
        let files = run(&tmp, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let _ = run(&tmp, &["merge", "--abort"]);
        files.lines().collect::<Vec<_>>().join(", ")
    } else {
        String::new()
    };
    let _ = run(root, &["worktree", "remove", "--force", &p]);
    match merged {
        Ok(_) => Ok(format!("merged {branch} into {target}")),
        Err(_) if !conflicted.is_empty() => bail!(
            "{branch} conflicts with {target} in {conflicted}; nothing was merged. \
             Check out {target} and merge {branch} there to resolve it"
        ),
        Err(e) => Err(e),
    }
}

/// Record where a branch pitwall made goes back to, so the work on it can
/// still be found after its task is gone. It lives in the repo's config, and
/// `git branch -m` and `-d` carry it along or clear it.
pub fn mark_branch(root: &Path, branch: &str, target: &str) -> Result<()> {
    run(root, &["config", &format!("branch.{branch}.pitwallBase"), target]).map(|_| ())
}

fn marked_target(root: &Path, branch: &str) -> Option<String> {
    run(root, &["config", "--get", &format!("branch.{branch}.pitwallBase")])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn branch_exists(root: &Path, branch: &str) -> bool {
    commit_of(root, &format!("refs/heads/{branch}")).is_some()
}

/// Commits on `branch` that `target` lacks. With no target, the ones no
/// other branch has.
fn unmerged_count(root: &Path, branch: &str, target: Option<&str>) -> usize {
    let own = format!("refs/heads/{branch}");
    let out = match target {
        Some(t) => run(root, &["rev-list", "--count", &format!("{t}..{branch}")]),
        None => run(
            root,
            &[
                "rev-list",
                "--count",
                &own,
                "--not",
                &format!("--exclude={own}"),
                "--branches",
            ],
        ),
    };
    out.ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// Give a detached HEAD's commits a branch when no branch has them, or
/// removing the worktree would leave them only in the reflog.
fn rescue_detached(root: &Path, worktree: &Path, branch: &str) -> Result<Option<String>> {
    let Some(sha) = head(worktree) else {
        return Ok(None);
    };
    if !run(root, &["branch", "--contains", &sha])?.trim().is_empty() {
        return Ok(None);
    }
    let mut n = 1;
    let name = loop {
        let name = if n == 1 {
            format!("{branch}-detached")
        } else {
            format!("{branch}-detached-{n}")
        };
        if new_branch_problem(root, &name).is_none() {
            break name;
        }
        n += 1;
    };
    run(root, &["branch", &name, &sha])?;
    Ok(Some(name))
}

/// Remove an agent's worktree without losing anything in it. Changes it
/// never committed are committed to the branch it's on, and a detached HEAD
/// gets a branch; then the folder goes. `branch`, the one the worktree was
/// made with, is deleted once `target` has all of it. Whatever still holds
/// work is named in what comes back, empty when nothing does.
pub fn retire_worktree(
    root: &Path,
    worktree: &Path,
    branch: &str,
    target: Option<&str>,
) -> Result<String> {
    let target = target
        .map(str::to_string)
        .or_else(|| marked_target(root, branch))
        .filter(|t| t != branch && branch_exists(root, t));
    let mut on = None;
    if worktree.is_dir() {
        if !status(worktree)?.is_empty() {
            stage_all(worktree)?;
            // The agent's hooks may well refuse half-done work, and this
            // commit is only here so the folder can go.
            run(
                worktree,
                &[
                    "commit",
                    "-q",
                    "--no-verify",
                    "-m",
                    "Work left uncommitted when its worktree was removed",
                ],
            )?;
        }
        on = match current_branch(worktree) {
            Some(b) => Some(b),
            None => rescue_detached(root, worktree, branch)?,
        };
        if let (Some(b), Some(t)) = (&on, &target) {
            if b != branch && marked_target(root, b).is_none() {
                mark_branch(root, b, t)?;
            }
        }
        run(
            root,
            &["worktree", "remove", "--force", &worktree.to_string_lossy()],
        )?;
    } else {
        let _ = run(root, &["worktree", "prune"]);
    }
    let mut kept = Vec::new();
    if branch_exists(root, branch) {
        let gone = match &target {
            Some(t) if is_merged(root, branch, t) => {
                run(root, &["branch", "-D", branch]).is_ok()
            }
            Some(_) => false,
            None => run(root, &["branch", "-d", branch]).is_ok(),
        };
        if !gone {
            kept.push(branch.to_string());
        }
    }
    if let Some(b) = on.filter(|b| b != branch) {
        if unmerged_count(root, &b, target.as_deref()) > 0 {
            kept.push(b);
        }
    }
    Ok(kept
        .iter()
        .map(|b| {
            let n = unmerged_count(root, b, target.as_deref());
            match &target {
                Some(t) => format!("kept {b}: {n} commit(s) not in {t}"),
                None => format!("kept {b}"),
            }
        })
        .collect::<Vec<_>>()
        .join("; "))
}

/// Work an agent left on a branch, found after its task is gone.
#[derive(Clone, Debug, PartialEq)]
pub struct Leftover {
    pub branch: String,
    /// Where it goes back to; None when that's unknown or deleted.
    pub target: Option<String>,
    /// Commits `target` lacks, or with no target that no other branch has.
    pub ahead: usize,
    /// A worktree of pitwall's it's still checked out in.
    pub worktree: Option<PathBuf>,
    /// Uncommitted changes in that worktree.
    pub dirty: usize,
}

/// Branches pitwall made that still hold work, and its worktrees for this
/// repo under `under`. Whether a live task owns one is for the caller to
/// say; a branch checked out anywhere else is someone working on it.
pub fn leftovers(root: &Path, under: &Path) -> Vec<Leftover> {
    let marks: Vec<(String, String)> = run(
        root,
        &["config", "--get-regexp", r"^branch\..*\.pitwallbase$"],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| {
        let (key, target) = l.split_once(' ')?;
        let branch = key.strip_prefix("branch.")?.strip_suffix(".pitwallbase")?;
        Some((branch.to_string(), target.trim().to_string()))
    })
    .collect();
    let under_real = under.canonicalize().unwrap_or_else(|_| under.to_path_buf());
    let mut ours: Vec<(String, PathBuf)> = Vec::new();
    let mut elsewhere: std::collections::HashSet<String> = Default::default();
    // The first one listed is the main checkout, never an agent's.
    for (i, block) in run(root, &["worktree", "list", "--porcelain"])
        .unwrap_or_default()
        .split("\n\n")
        .enumerate()
    {
        let mut lines = block.lines();
        let Some(path) = lines.next().and_then(|l| l.strip_prefix("worktree ")) else {
            continue;
        };
        let path = PathBuf::from(path);
        let Some(b) = lines.find_map(|l| l.strip_prefix("branch refs/heads/")) else {
            continue;
        };
        if i > 0 && (path.starts_with(under) || path.starts_with(&under_real)) && path.is_dir() {
            ours.push((b.to_string(), path));
        } else {
            elsewhere.insert(b.to_string());
        }
    }
    let mut out: Vec<Leftover> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let found = marks
        .into_iter()
        .map(|(b, t)| (b, Some(t)))
        .chain(ours.iter().map(|(b, _)| (b.clone(), None)));
    for (branch, mark) in found {
        if !seen.insert(branch.clone()) || elsewhere.contains(&branch) || !branch_exists(root, &branch) {
            continue;
        }
        let target = mark
            .or_else(|| marked_target(root, &branch))
            .filter(|t| *t != branch && branch_exists(root, t));
        let worktree = ours.iter().find(|(b, _)| *b == branch).map(|(_, p)| p.clone());
        let ahead = unmerged_count(root, &branch, target.as_deref());
        if ahead == 0 && worktree.is_none() {
            continue;
        }
        let dirty = worktree
            .as_deref()
            .and_then(|w| status(w).ok())
            .map_or(0, |c| c.len());
        out.push(Leftover {
            branch,
            target,
            ahead,
            worktree,
            dirty,
        });
    }
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// Throw a branch away, and the worktree it's checked out in.
pub fn delete_branch(root: &Path, branch: &str, worktree: Option<&Path>) -> Result<String> {
    if let Some(w) = worktree.filter(|w| w.is_dir()) {
        run(root, &["worktree", "remove", "--force", &w.to_string_lossy()])?;
    }
    run(root, &["branch", "-D", branch])?;
    Ok(format!("deleted {branch}"))
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
    ensure_mergeable(repo_root, "HEAD", branch, None)?;
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BranchProblem {
    Empty,
    Invalid,
    Taken,
}

impl std::fmt::Display for BranchProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            BranchProblem::Empty => "give the branch a name",
            BranchProblem::Invalid => "that isn't a valid branch name",
            BranchProblem::Taken => "there's already a branch by that name",
        })
    }
}

/// Why `name` can't be a new branch in the repo at `dir`, if it can't.
pub fn new_branch_problem(dir: &Path, name: &str) -> Option<BranchProblem> {
    let name = name.trim();
    if name.is_empty() {
        return Some(BranchProblem::Empty);
    }
    // check-ref-format alone lets `-x` and `HEAD` through.
    if name.starts_with('-')
        || name == "HEAD"
        || run(dir, &["check-ref-format", "--branch", name]).is_err()
    {
        return Some(BranchProblem::Invalid);
    }
    if commit_of(dir, &format!("refs/heads/{name}")).is_some() {
        return Some(BranchProblem::Taken);
    }
    None
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

/// `target` has every commit on `branch`.
pub fn is_merged(dir: &Path, branch: &str, target: &str) -> bool {
    run(dir, &["merge-base", "--is-ancestor", branch, target]).is_ok()
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
            "pitwall-git-{}-{}",
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
    fn says_why_a_name_cant_be_a_new_branch() {
        let d = repo();
        assert_eq!(new_branch_problem(&d, "fix/login-loop"), None);
        let here = current_branch(&d).unwrap();
        assert_eq!(new_branch_problem(&d, &here), Some(BranchProblem::Taken));
        for bad in ["", "  ", "-x", "HEAD", "a..b", "two words", "end."] {
            assert!(new_branch_problem(&d, bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn a_commit_in_a_worktree_is_ahead_until_merged() {
        let d = repo();
        let base = head(&d).unwrap();
        let wt = d.with_extension("wt3");
        let _ = std::fs::remove_dir_all(&wt);
        let w = wt.to_str().unwrap();
        run(&d, &["worktree", "add", "-q", "-b", "pitwall/z", w, &base]).unwrap();
        assert_eq!(head(&wt).as_deref(), Some(base.as_str()));
        assert_eq!(ahead_count(&wt, "main"), 0);

        std::fs::write(wt.join("z.txt"), "z\n").unwrap();
        commit_all(&wt, "Add z");
        assert_ne!(head(&wt).as_deref(), Some(base.as_str()));
        assert_eq!(ahead_count(&wt, "main"), 1);

        // An edit on main the merge doesn't touch is no reason to refuse,
        // and it's still there after.
        std::fs::write(d.join("a.txt"), "mine\n").unwrap();
        accept(&d, &wt, "pitwall/z", "main", "unused").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "mine\n");
        assert!(d.join("z.txt").is_file());
        assert_eq!(ahead_count(&wt, "main"), 0);
        assert!(is_merged(&d, "pitwall/z", "main"));

        // Accepting it again: already merged, nothing to do.
        let out = accept(&d, &wt, "pitwall/z", "main", "unused").unwrap();
        assert!(out.contains("already merged"), "{out}");
    }

    fn agent_worktree(d: &Path, ext: &str, branch: &str) -> PathBuf {
        let wt = d.with_extension(ext);
        let _ = std::fs::remove_dir_all(&wt);
        let w = wt.to_str().unwrap();
        run(d, &["worktree", "add", "-q", "-b", branch, w, "main"]).unwrap();
        mark_branch(d, branch, "main").unwrap();
        wt
    }

    #[test]
    fn retiring_a_worktree_keeps_what_was_never_committed() {
        let d = repo();
        let under = d.parent().unwrap().to_path_buf();
        let wt = agent_worktree(&d, "wt5", "agent-a");
        std::fs::write(wt.join("half.txt"), "half done\n").unwrap();
        assert_eq!(leftovers(&d, &under).len(), 1);

        let out = retire_worktree(&d, &wt, "agent-a", Some("main")).unwrap();
        assert!(out.contains("kept agent-a: 1 commit(s) not in main"), "{out}");
        assert!(!wt.exists());
        assert_eq!(run(&d, &["show", "agent-a:half.txt"]).unwrap(), "half done\n");
        let left = leftovers(&d, &under);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].branch, "agent-a");
        assert_eq!(left[0].target.as_deref(), Some("main"));
        assert_eq!((left[0].ahead, left[0].worktree.is_none()), (1, true));

        // Merged, it isn't left over, and retiring a worktree for it deletes it.
        run(&d, &["merge", "-q", "--no-ff", "--no-edit", "agent-a"]).unwrap();
        assert!(leftovers(&d, &under).is_empty());
        let wt = agent_worktree(&d, "wt6", "agent-b");
        assert_eq!(retire_worktree(&d, &wt, "agent-b", None).unwrap(), "");
        assert!(!branch_exists(&d, "agent-b"));
    }

    #[test]
    fn an_agent_that_moved_to_another_branch_is_followed() {
        let d = repo();
        let under = d.parent().unwrap().to_path_buf();
        let wt = agent_worktree(&d, "wt7", "agent-c");
        run(&wt, &["checkout", "-q", "-b", "feature-x"]).unwrap();
        std::fs::write(wt.join("x.txt"), "x\n").unwrap();
        commit_all(&wt, "Add x");

        // Merging agent-c now would merge nothing.
        let err = accept(&d, &wt, "agent-c", "main", "m").unwrap_err().to_string();
        assert!(err.contains("on feature-x now"), "{err}");

        // The branch it made is listed while the folder's there, and kept
        // and marked for main once it's gone.
        assert!(leftovers(&d, &under).iter().any(|l| l.branch == "feature-x"));
        let out = retire_worktree(&d, &wt, "agent-c", Some("main")).unwrap();
        assert_eq!(out, "kept feature-x: 1 commit(s) not in main");
        assert!(!branch_exists(&d, "agent-c"));
        let left = leftovers(&d, &under);
        assert_eq!(left.len(), 1);
        assert_eq!(
            (left[0].branch.as_str(), left[0].target.as_deref()),
            ("feature-x", Some("main"))
        );

        // A detached HEAD's commits get a branch before the folder goes.
        let wt = agent_worktree(&d, "wt8", "agent-d");
        run(&wt, &["checkout", "-q", "--detach"]).unwrap();
        std::fs::write(wt.join("y.txt"), "y\n").unwrap();
        commit_all(&wt, "Add y");
        let out = retire_worktree(&d, &wt, "agent-d", Some("main")).unwrap();
        assert!(out.contains("kept agent-d-detached"), "{out}");
        assert_eq!(run(&d, &["show", "agent-d-detached:y.txt"]).unwrap(), "y\n");
    }

    fn commit_all(dir: &Path, msg: &str) {
        run(dir, &["add", "-A"]).unwrap();
        run(dir, &["commit", "-qm", msg]).unwrap();
    }

    #[test]
    fn accept_merges_into_a_branch_nobody_has_checked_out() {
        let d = repo();
        let base = run(&d, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        run(&d, &["branch", "release"]).unwrap();
        let wt = d.with_extension("wt2");
        let _ = std::fs::remove_dir_all(&wt);
        let w = wt.to_str().unwrap();
        run(&d, &["worktree", "add", "-q", "-b", "pitwall/y", w, &base]).unwrap();
        std::fs::write(wt.join("c.txt"), "agent\n").unwrap();

        let out = accept(&d, &wt, "pitwall/y", "release", "Add c").unwrap();
        assert!(out.contains("merged pitwall/y into release"), "{out}");
        // The main checkout stayed on main, without the agent's file.
        assert_eq!(current_branch(&d).as_deref(), Some("main"));
        assert!(!d.join("c.txt").exists());
        assert_eq!(run(&d, &["show", "release:c.txt"]).unwrap(), "agent\n");
        // And the scratch worktree is gone again.
        let list = run(&d, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(!list.contains("pitwall-merge-"), "{list}");

        // A conflict merges nothing and says where.
        std::fs::write(wt.join("a.txt"), "agent side\n").unwrap();
        run(&wt, &["commit", "-qam", "agent"]).unwrap();
        run(&d, &["checkout", "-q", "release"]).unwrap();
        std::fs::write(d.join("a.txt"), "release side\n").unwrap();
        run(&d, &["commit", "-qam", "release"]).unwrap();
        run(&d, &["checkout", "-q", "main"]).unwrap();
        let tip = run(&d, &["rev-parse", "release"]).unwrap();
        let err = accept(&d, &wt, "pitwall/y", "release", "x").unwrap_err().to_string();
        assert!(err.contains("a.txt") && err.contains("nothing was merged"), "{err}");
        assert_eq!(run(&d, &["rev-parse", "release"]).unwrap(), tip);
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
                "pitwall/x",
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

        // No such branch, or the agent's own.
        assert!(accept(&d, &wt, "pitwall/x", "release", "msg").is_err());
        assert!(accept(&d, &wt, "pitwall/x", "pitwall/x", "msg").is_err());
        // Uncommitted work needs a message.
        assert!(accept(&d, &wt, "pitwall/x", "main", "  ").is_err());
        // An untracked file where the agent adds one refuses before anything
        // is committed.
        std::fs::write(d.join("b.txt"), "mine\n").unwrap();
        let head = run(&wt, &["rev-parse", "HEAD"]).unwrap();
        let err = accept(&d, &wt, "pitwall/x", "main", "Add b")
            .unwrap_err()
            .to_string();
        assert!(err.contains("b.txt"), "{err}");
        assert_eq!(run(&wt, &["rev-parse", "HEAD"]).unwrap(), head);
        std::fs::remove_file(d.join("b.txt")).unwrap();
        // A dirty main checkout refuses before anything is committed.
        std::fs::write(d.join("a.txt"), "mine\n").unwrap();
        let head = run(&wt, &["rev-parse", "HEAD"]).unwrap();
        assert!(accept(&d, &wt, "pitwall/x", "main", "Add b").is_err());
        assert_eq!(
            run(&wt, &["rev-parse", "HEAD"]).unwrap(),
            head,
            "a refused accept committed"
        );
        run(&d, &["checkout", "-q", "--", "a.txt"]).unwrap();
        // So does anything staged, which git won't merge over.
        std::fs::write(d.join("c.txt"), "staged\n").unwrap();
        run(&d, &["add", "c.txt"]).unwrap();
        let err = accept(&d, &wt, "pitwall/x", "main", "Add b")
            .unwrap_err()
            .to_string();
        assert!(err.contains("staged"), "{err}");
        run(&d, &["rm", "-q", "--cached", "c.txt"]).unwrap();
        std::fs::remove_file(d.join("c.txt")).unwrap();

        let out = accept(&d, &wt, "pitwall/x", "main", "Add b").unwrap();
        assert!(
            out.contains("committed") && out.contains("merged pitwall/x into main"),
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
        run(&d, &["checkout", "-qb", "pitwall/x"]).unwrap();
        std::fs::write(d.join("b.txt"), "b\n").unwrap();
        stage_all(&d).unwrap();
        commit(&d, "b").unwrap();
        run(&d, &["checkout", "-q", "main"]).unwrap();
        let m = merge(&d, "pitwall/x").unwrap();
        assert!(m.contains("into main"));
        assert!(d.join("b.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_repo_gets_a_first_commit_without_its_big_files() {
        let d = std::env::temp_dir().join(format!("pitwall-git-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("data")).unwrap();
        std::fs::write(d.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(d.join("data/big.bin"), vec![0u8; FIRST_COMMIT_MAX as usize + 1]).unwrap();
        init(&d).unwrap();
        // CI machines may have no identity configured.
        run(&d, &["config", "user.email", "t@example.com"]).unwrap();
        run(&d, &["config", "user.name", "t"]).unwrap();
        assert_eq!(first_commit(&d).unwrap(), 1);
        let files = run(&d, &["ls-files"]).unwrap();
        assert_eq!(files.trim(), "main.rs");
        assert!(status(&d).unwrap().is_empty(), "the big file is excluded, not left untracked");
        let _ = std::fs::remove_dir_all(&d);
    }
}
