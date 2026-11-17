//! Briefs: a Markdown doc you write (or have an agent finish) before handing
//! the work to an agent. They live in the project at `.pitwall/briefs/`, made
//! from a few templates whose guide comment tells an agent how to fill them in.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pitwall_core::config::AgentConfig;
use serde_json::Value;

use crate::i18n::{tr, trf};

pub struct Template {
    pub key: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub blurb: &'static str,
    body: &'static str,
}

// `name` also goes into the prompt claude gets, so it stays English there and
// is translated only where it's shown.
impl Template {
    pub fn shown_name(&self) -> &'static str {
        tr(self.name)
    }

    pub fn shown_blurb(&self) -> &'static str {
        tr(self.blurb)
    }
}

pub const TEMPLATES: &[Template] = &[
    Template {
        key: "feature",
        name: "Feature",
        icon: "✦",
        blurb: "Something new the app should do",
        body: "<!-- guide (feature)
The request goes next to Ask; keep it as the user wrote it.
Problem: one or two sentences on who needs this and why.
Scope: what's in and, as important, what's out.
Approach: how it fits the code that's there; name the files and types it touches.
When a choice changes what gets built, recommend one and ask before writing it down.
Steps: small and checkable, in the order you'd do them.
-->

# {title}

**Ask:**

## Problem

## Scope

- In:
- Out:

## Approach

## Choices

## Steps

- [ ]

## Done when

- [ ]
",
    },
    Template {
        key: "bug",
        name: "Bug fix",
        icon: "✕",
        blurb: "Something broken, and how to see it",
        body: "<!-- guide (bug)
What's broken goes next to Ask; keep it as the user wrote it.
Reproduce it before explaining it: exact steps, input and what you saw.
Find the cause in the code before proposing a change, and name the files.
Say how we'll know it's fixed: a test, or steps that now behave.
-->

# {title}

**Ask:**

## What happens

## What should happen

## How to see it

1.

## Cause

## Fix

## Done when

- [ ]
",
    },
    Template {
        key: "refactor",
        name: "Refactor",
        icon: "↻",
        blurb: "Change the shape, keep the behaviour",
        body: "<!-- guide (refactor)
What should change shape goes next to Ask.
Why: what's hard today, with a concrete example from the code.
Behaviour stays the same: say how we'll prove it (tests to run, or to add first).
Plan: steps that each leave the code building and the tests passing.
-->

# {title}

**Ask:**

## Why

## What changes

## What stays the same

## Plan

- [ ]

## How we'll know nothing broke
",
    },
    Template {
        key: "ui",
        name: "UI polish",
        icon: "◐",
        blurb: "How a screen should look and feel",
        body: "<!-- guide (ui)
The screen or flow goes next to Ask.
Now: what it looks like and what's wrong, as the user would put it.
Want: the result, with sizes, colours, spacing and states where they matter.
Name the files that draw it. Keep to the app's look unless the Ask says otherwise.
-->

# {title}

**Ask:**

## Now

## Want

## States

- Empty:
- Loading:
- Error:

## Where in the code

## Steps

- [ ]
",
    },
    Template {
        key: "spike",
        name: "Spike",
        icon: "◇",
        blurb: "Find out first: options and a pick",
        body: "<!-- guide (spike)
The question goes next to Ask.
Find out before building anything: read the code and docs, try things on a scratch branch.
Options: two or three real ones, each with what it costs and what it gets us.
End with a recommendation and why. Don't build the real thing in this step.
-->

# {title}

**Ask:**

## What we know

## Options

### 1.

### 2.

## Recommendation

## Next steps

- [ ]
",
    },
    Template {
        key: "blank",
        name: "Blank",
        icon: "□",
        blurb: "Just a page and an Ask line",
        body: "# {title}

**Ask:**
",
    },
];

/// Where a project's briefs live.
pub fn dir(root: &Path) -> PathBuf {
    root.join(".pitwall").join("briefs")
}

pub fn is_brief(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "md")
        && path
            .parent()
            .is_some_and(|p| p.ends_with(Path::new(".pitwall").join("briefs")))
}

pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// The template a brief was made from, from its guide comment.
pub fn template_of(text: &str) -> Option<&'static Template> {
    let key = text.split_once("<!-- guide (")?.1.split_once(')')?.0.trim();
    TEMPLATES.iter().find(|t| t.key == key)
}

/// The first `# ` heading, or the file's name.
pub fn title_of(path: &Path, text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().replace('-', " "))
                .unwrap_or_default()
        })
}

/// One brief in a project's list.
#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub title: String,
    /// Steps ticked and steps in all.
    pub done: usize,
    pub steps: usize,
    pub template: Option<&'static Template>,
}

/// Ticked and total steps.
pub fn progress(text: &str) -> (usize, usize) {
    let s = pitwall_core::brief::steps(text);
    (s.iter().filter(|s| s.done).count(), s.len())
}

/// A project's briefs in the order they were written, so the list doesn't
/// reshuffle as they're edited.
pub fn list(root: &Path) -> Vec<Entry> {
    pitwall_core::rename::adopt_project(root);
    let Ok(rd) = std::fs::read_dir(dir(root)) else {
        return vec![];
    };
    let mut v: Vec<(std::time::SystemTime, Entry)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|p| {
            let meta = std::fs::metadata(&p).ok();
            let born = meta
                .as_ref()
                .and_then(|m| m.created().or_else(|_| m.modified()).ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let title = title_of(&p, &text);
            let (done, steps) = progress(&text);
            let entry = Entry {
                path: p,
                title,
                done,
                steps,
                template: template_of(&text),
            };
            (born, entry)
        })
        .collect();
    v.sort_by(|a, b| (a.0, &a.1.path).cmp(&(b.0, &b.1.path)));
    v.into_iter().map(|(_, e)| e).collect()
}

/// Write a new brief from a template and return its path. The briefs folder
/// gets a .gitignore so they stay out of commits until you choose otherwise.
pub fn create(root: &Path, title: &str, template: &Template) -> std::io::Result<PathBuf> {
    pitwall_core::rename::adopt_project(root);
    let d = dir(root);
    std::fs::create_dir_all(&d)?;
    let ignore = root.join(".pitwall").join(".gitignore");
    if !ignore.exists() {
        std::fs::write(
            &ignore,
            "# pitwall's briefs; delete this file to commit them\n*\n",
        )?;
    }
    let base = {
        let s = crate::app::slugify(title);
        if s.is_empty() {
            "brief".to_string()
        } else {
            s
        }
    };
    let mut path = d.join(format!("{base}.md"));
    let mut n = 2;
    while path.exists() {
        path = d.join(format!("{base}-{n}.md"));
        n += 1;
    }
    std::fs::write(&path, template.body.replace("{title}", title.trim()))?;
    Ok(path)
}

/// The text without its comments: what an agent is handed.
pub fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// What claude is asked when it finishes a brief. It runs headless, so it
/// can't ask anything; open choices go into the brief instead.
fn finish_prompt(path: &Path, text: &str) -> String {
    let kind = template_of(text).map(|t| t.name).unwrap_or("brief");
    format!(
        "Finish the brief at {path}. It's a {kind} brief: the comment at the top says how to fill \
         it in. Read the code you need, then reply with the finished brief as Markdown and nothing \
         else: start at its `# ` title, leave out the guide comment, no code fence around it and \
         no words before or after it. Your reply replaces the file. Keep the **Ask:** line and \
         anything already written as it is, and fill in every section with specifics from this \
         codebase: file names, functions, steps small enough to check. You can't ask me anything \
         while you work, so when a choice would change what gets built, write the options and \
         your recommendation under Choices, or under an `## Open questions` section at the end if \
         there's no Choices section. Don't change any files.\n\nThe brief as it stands:\n\n{text}",
        path = path.display()
    )
}

pub enum Update {
    /// What it's doing: reading a file, thinking, writing.
    Status(String),
    Done(Result<(), String>),
    Stopped,
}

/// A headless claude filling in a brief.
pub struct Run {
    pub status: String,
    /// The text from before the run is on the undo stack.
    pub recorded: bool,
    /// The brief as it was, put back if the run stops or fails halfway.
    pub original: String,
    child: Arc<Mutex<Child>>,
    stopped: Arc<AtomicBool>,
}

impl Run {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.child.lock().unwrap().kill();
    }
}

/// Run `claude -p` on a brief with read-only tools and stream its reply into
/// the file as it comes. The open editor and preview pick it up from disk.
pub fn finish(
    agent: &AgentConfig,
    root: &Path,
    path: &Path,
    send: impl Fn(Update) + Send + 'static,
) -> std::io::Result<Run> {
    let text = std::fs::read_to_string(path)?;
    let head = guide_of(&text)
        .map(|g| format!("{g}\n\n"))
        .unwrap_or_default();
    // The prompt goes before the flags: --tools takes a list and would
    // swallow a prompt after it. --tools only limits the built-in tools, so
    // --strict-mcp-config keeps out MCP servers that can run commands.
    let mut child = Command::new(&agent.command)
        .arg("-p")
        .arg(finish_prompt(path, &text))
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--include-partial-messages", "--tools", "Read,Grep,Glob"])
        .arg("--strict-mcp-config")
        .envs(&agent.env)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    // Drained on its own so a chatty stderr can't fill the pipe and stall it.
    let errs = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let child = Arc::new(Mutex::new(child));
    let stopped = Arc::new(AtomicBool::new(false));
    let run = Run {
        status: tr("starting").into(),
        recorded: false,
        original: text.clone(),
        child: child.clone(),
        stopped: stopped.clone(),
    };
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let got = stream(BufReader::new(out), &path, &head, &send);
        // Polled rather than wait(): holding the lock through a blocking wait
        // would freeze the UI if Stop is pressed now.
        while matches!(child.lock().unwrap().try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let errs = errs.join().unwrap_or_default();
        if stopped.load(Ordering::Relaxed) {
            return send(Update::Stopped);
        }
        send(Update::Done(got.and_then(|finished| {
            if finished {
                Ok(())
            } else {
                Err(errs
                    .lines()
                    .rfind(|l| !l.trim().is_empty())
                    .unwrap_or(tr("claude exited before it finished"))
                    .to_string())
            }
        })));
    });
    Ok(run)
}

/// Read claude's stream-json events. Text before a tool call is narration
/// ("Let me look at..."), so the brief is the text since the last tool call,
/// from its first `# ` line. Ok(false) when it ended without a result.
fn stream(
    out: impl BufRead,
    path: &Path,
    head: &str,
    send: &dyn Fn(Update),
) -> Result<bool, String> {
    let mut cur = String::new();
    let mut last_write: Option<Instant> = None;
    let mut writing = false;
    for line in out.lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("stream_event") => {
                let e = &v["event"];
                match e["type"].as_str() {
                    Some("message_start") => cur.clear(),
                    Some("content_block_start") => match e["content_block"]["type"].as_str() {
                        Some("tool_use") => cur.clear(),
                        Some("thinking") if !writing => send(Update::Status(tr("thinking").into())),
                        _ => {}
                    },
                    Some("content_block_delta") if e["delta"]["type"] == "text_delta" => {
                        cur.push_str(e["delta"]["text"].as_str().unwrap_or_default());
                        let Some(body) = body_of(&cur) else { continue };
                        if !writing {
                            writing = true;
                            send(Update::Status(tr("writing").into()));
                        }
                        // Every delta would be a write per token; a few a
                        // second still reads as live.
                        if last_write.is_none_or(|t| t.elapsed() > Duration::from_millis(120)) {
                            let _ = write(path, head, body);
                            last_write = Some(Instant::now());
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                let content = v["message"]["content"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for c in content.iter().filter(|c| c["type"] == "tool_use") {
                    writing = false;
                    send(Update::Status(describe(
                        c["name"].as_str().unwrap_or_default(),
                        &c["input"],
                    )));
                }
            }
            Some("result") => {
                let r = v["result"].as_str().unwrap_or_default();
                if v["is_error"] == true || v["subtype"] != "success" {
                    return Err(if r.is_empty() {
                        v["subtype"].as_str().unwrap_or("failed").to_string()
                    } else {
                        r.to_string()
                    });
                }
                let body = body_of(r).ok_or(tr("the reply had no brief in it (no `# ` title)"))?;
                write(path, head, body)
                    .map_err(|e| trf!("couldn't write the brief: {e}", e = e))?;
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}

/// The brief in a reply: from its first `# ` heading, without the closing
/// fence when it was fenced anyway.
fn body_of(s: &str) -> Option<&str> {
    let start = if s.starts_with("# ") {
        0
    } else {
        s.find("\n# ")? + 1
    };
    let body = s[start..].trim_end();
    if s[..start].contains("```") {
        Some(body.strip_suffix("```").unwrap_or(body).trim_end())
    } else {
        Some(body)
    }
}

/// The leading guide comment, which the reply leaves out and we keep.
fn guide_of(text: &str) -> Option<&str> {
    let t = text.trim_start();
    if !t.starts_with("<!--") {
        return None;
    }
    Some(&t[..t.find("-->")? + 3])
}

/// Written whole and renamed over, so the editor never reloads half a file.
fn write(path: &Path, head: &str, body: &str) -> std::io::Result<()> {
    put(path, &format!("{head}{body}\n"))
}

/// Written whole and renamed over, so a reader never sees half a file.
pub fn put(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn describe(tool: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or_default();
    let d = match tool {
        "Read" => {
            let p = Path::new(s("file_path"));
            trf!(
                "reading {file}",
                file = p.file_name().unwrap_or(p.as_os_str()).to_string_lossy()
            )
        }
        "Grep" => trf!("searching for {pattern}", pattern = s("pattern")),
        "Glob" => trf!("looking for {pattern}", pattern = s("pattern")),
        t => t.to_lowercase(),
    };
    if d.chars().count() > 48 {
        format!("{}…", d.chars().take(47).collect::<String>())
    } else {
        d
    }
}

/// The task handed to the agent that builds what a brief describes.
pub fn handoff_prompt(path: &Path, text: &str) -> String {
    format!(
        "Build what this brief describes. It's saved at {path}. Work through its steps in order \
         and keep to its scope; if something in it turns out to be wrong, say so before changing \
         course. As you finish each step, run `pitwall brief tick <n>` with its number \
         (`pitwall brief` lists them): that's how I follow along.\n\n---\n\n{brief}",
        path = path.display(),
        brief = strip_comments(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_brief_has_its_title_template_and_ask() {
        let root = std::env::temp_dir().join(format!("pitwall-briefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = create(&root, "Fix the login loop", &TEMPLATES[1]).unwrap();
        assert!(p.ends_with(".pitwall/briefs/fix-the-login-loop.md"));
        assert!(is_brief(&p));
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(title_of(&p, &text), "Fix the login loop");
        assert_eq!(template_of(&text).map(|t| t.key), Some("bug"));
        // A second one with the same title doesn't overwrite the first.
        let q = create(&root, "Fix the login loop", &TEMPLATES[0]).unwrap();
        assert!(q.ends_with("fix-the-login-loop-2.md"));
        assert_eq!(list(&root).len(), 2);
        assert!(root.join(".pitwall/.gitignore").exists());
        // The guide comment isn't part of what an agent is handed.
        let handed = handoff_prompt(&p, &text);
        assert!(!handed.contains("<!--") && handed.contains("**Ask:**"));
        assert!(guide_of(&text)
            .is_some_and(|g| g.starts_with("<!-- guide (bug)") && g.ends_with("-->")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_brief_is_cut_out_of_a_chatty_or_fenced_reply() {
        assert_eq!(body_of("# A\n\nbody\n"), Some("# A\n\nbody"));
        assert_eq!(body_of("Here it is:\n\n# A\nbody"), Some("# A\nbody"));
        assert_eq!(body_of("```markdown\n# A\nbody\n```\n"), Some("# A\nbody"));
        // A brief that ends in a code block keeps its closing fence.
        assert_eq!(body_of("# A\n```\nx\n```"), Some("# A\n```\nx\n```"));
        assert_eq!(body_of("Let me read app.rs first."), None);
    }

    #[test]
    fn progress_counts_real_steps_only() {
        let text = "<!-- guide\n- [ ] not this\n-->\n# T\n\n## Steps\n\n- [x] one\n- [ ] two\n  * [X] three\n- [ ]\n";
        assert_eq!(progress(text), (2, 3));
    }
}
