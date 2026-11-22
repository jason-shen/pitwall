//! Claude Code's saved sessions. Claude files each one under
//! `~/.claude/projects/<folder it started in>/<id>.jsonl` and won't resume it
//! from any other folder, so pitwall has to start `claude --resume` in the
//! session's own folder, which for an agent is usually its worktree.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    /// Where it started, and so where it has to be resumed.
    pub cwd: PathBuf,
    pub title: String,
    pub modified_unix: i64,
}

// The folder is near the top of the file and the title is rewritten as the
// session goes on, so the two ends are enough. Sessions run to tens of MB.
const HEAD: u64 = 64 * 1024;
const TAIL: u64 = 64 * 1024;

fn projects() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .map(|d| d.join("projects"))
}

/// The session a running Claude is on. Claude keeps
/// `~/.claude/sessions/<pid>.json` for each live process and updates it on
/// /resume and /clear, which `--continue` (newest in the folder) can't tell
/// apart when two sessions share a worktree.
pub fn live_session(pid: u32) -> Option<String> {
    let dir = projects()?.parent()?.join("sessions");
    let v: Value =
        serde_json::from_slice(&std::fs::read(dir.join(format!("{pid}.json"))).ok()?).ok()?;
    if v["pid"].as_u64() != Some(pid as u64) {
        return None;
    }
    v["sessionId"].as_str().map(str::to_string)
}

/// The session id in `--resume <id>`, `-r <id>` or `--resume=<id>`.
pub fn resume_id(args: &[String]) -> Option<&str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        if let Some(v) = a.strip_prefix("--resume=") {
            return Some(v).filter(|v| !v.is_empty());
        }
        if a == "--resume" || a == "-r" {
            return it
                .next()
                .map(String::as_str)
                .filter(|v| !v.starts_with('-'));
        }
    }
    None
}

/// The session with this id, whichever folder it's filed under. `--resume`
/// also takes a search term, which isn't an id and finds nothing here.
pub fn find(id: &str) -> Option<Session> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return None;
    }
    let name = format!("{id}.jsonl");
    std::fs::read_dir(projects()?)
        .ok()?
        .flatten()
        .map(|d| d.path().join(&name))
        .filter(|p| p.is_file())
        .filter_map(|p| read(&p))
        .max_by_key(|s| s.modified_unix)
}

/// Sessions started in one of `folders` or below it, newest first. Ones
/// whose folder is gone are left out: there's nowhere to resume them.
pub fn under(folders: &[PathBuf], max: usize) -> Vec<Session> {
    let Some(Ok(dirs)) = projects().map(std::fs::read_dir) else {
        return vec![];
    };
    // Claude shortens very long folder names and adds a hash, so only the
    // start of the name is compared. A prefix also matches sibling folders
    // (`app` and `app-server`); the cwd check below sorts those out.
    let prefixes: Vec<String> = folders
        .iter()
        .map(|f| encode(f).chars().take(200).collect())
        .collect();
    let mut files = vec![];
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if !prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in rd.flatten() {
            let p = f.path();
            // agent-*.jsonl are subagent transcripts in older Claude versions.
            if p.extension() != Some("jsonl".as_ref())
                || f.file_name().to_string_lossy().starts_with("agent-")
            {
                continue;
            }
            if let Ok(m) = f.metadata().and_then(|m| m.modified()) {
                files.push((m, p));
            }
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files
        .iter()
        .filter_map(|(_, p)| read(p))
        .filter(|s| folders.iter().any(|f| s.cwd.starts_with(f)) && s.cwd.is_dir())
        .take(max)
        .collect()
}

/// How Claude names a folder's directory under projects/.
fn encode(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn read(path: &Path) -> Option<Session> {
    let id = path.file_stem()?.to_str()?.to_string();
    let f = File::open(path).ok()?;
    let meta = f.metadata().ok()?;
    let mut cwd = None;
    let mut prompt = None;
    let mut titles = Titles::default();
    // A line cut off at HEAD fails to parse and is skipped.
    for line in BufReader::new((&f).take(HEAD))
        .split(b'\n')
        .map_while(Result::ok)
    {
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = v["cwd"].as_str().map(PathBuf::from);
        }
        if prompt.is_none() {
            prompt = user_text(&v);
        }
        titles.see(&v);
    }
    let len = meta.len();
    if len > HEAD {
        let start = len.saturating_sub(TAIL);
        (&f).seek(SeekFrom::Start(start)).ok()?;
        let mut buf = vec![];
        (&f).take(TAIL).read_to_end(&mut buf).ok()?;
        // The first line is most likely the back half of one.
        for line in buf.split(|b| *b == b'\n').skip(1) {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                titles.see(&v);
            }
        }
    }
    // No title and nothing typed: an empty session, which Claude's own
    // picker doesn't list either.
    let title = titles.best().or(prompt)?;
    Some(Session {
        id,
        cwd: cwd?,
        title,
        modified_unix: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64),
    })
}

/// A name given with /rename beats Claude's own, which beats the old
/// summary lines. The last of each kind is the current one.
#[derive(Default)]
struct Titles {
    custom: Option<String>,
    ai: Option<String>,
    summary: Option<String>,
}

impl Titles {
    fn see(&mut self, v: &Value) {
        let text = |k: &str| v[k].as_str().map(one_line).filter(|s| !s.is_empty());
        match v["type"].as_str() {
            Some("custom-title") => self.custom = text("customTitle").or(self.custom.take()),
            Some("ai-title") => self.ai = text("aiTitle").or(self.ai.take()),
            Some("summary") => self.summary = text("summary").or(self.summary.take()),
            _ => {}
        }
    }

    fn best(self) -> Option<String> {
        self.custom.or(self.ai).or(self.summary)
    }
}

fn user_text(v: &Value) -> Option<String> {
    if v["type"] != "user" || v["isMeta"] == true || v["isSidechain"] == true {
        return None;
    }
    let c = &v["message"]["content"];
    let text = match c.as_str() {
        Some(s) => s,
        // Tool results are user messages too, with no text item.
        None => c.as_array()?.iter().find(|i| i["type"] == "text")?["text"].as_str()?,
    };
    let text = text.trim();
    // Slash commands and their output come wrapped in tags.
    (!text.is_empty() && !text.starts_with('<')).then(|| one_line(text))
}

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resume_id_forms() {
        assert_eq!(resume_id(&v(&["--resume", "abc"])), Some("abc"));
        assert_eq!(resume_id(&v(&["-r", "abc", "hi"])), Some("abc"));
        assert_eq!(resume_id(&v(&["--resume=abc"])), Some("abc"));
        assert_eq!(resume_id(&v(&["--resume"])), None);
        assert_eq!(resume_id(&v(&["--resume", "--verbose"])), None);
        assert_eq!(resume_id(&v(&["--continue"])), None);
        assert_eq!(resume_id(&v(&["--", "-r", "abc"])), None);
    }

    #[test]
    fn encodes_like_claude() {
        assert_eq!(
            encode(Path::new("/Users/me/.pitwall/worktrees/app-term-19")),
            "-Users-me--pitwall-worktrees-app-term-19"
        );
    }

    #[test]
    fn reads_folder_and_title() {
        let dir = std::env::temp_dir().join(format!("pitwall-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("0123abcd.jsonl");
        let lines = [
            r#"{"type":"mode","sessionId":"0123abcd"}"#,
            r#"{"type":"user","cwd":"/tmp/wt","message":{"role":"user","content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","cwd":"/tmp/wt/sub","message":{"role":"user","content":[{"type":"text","text":"profile the\n server"}]}}"#,
            r#"{"type":"ai-title","aiTitle":"Old title"}"#,
            r#"{"type":"ai-title","aiTitle":"Profiling the server"}"#,
        ];
        std::fs::write(&p, lines.join("\n")).unwrap();
        let s = read(&p).unwrap();
        assert_eq!(s.id, "0123abcd");
        assert_eq!(s.cwd, PathBuf::from("/tmp/wt"));
        assert_eq!(s.title, "Profiling the server");

        std::fs::write(&p, lines[..3].join("\n")).unwrap();
        assert_eq!(read(&p).unwrap().title, "profile the server");

        std::fs::write(&p, lines[..2].join("\n")).unwrap();
        assert!(read(&p).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
