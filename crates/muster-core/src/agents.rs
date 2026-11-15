//! Which tasks are agents, and how agents find and talk to each other.

use crate::proto::TaskInfo;

/// Coding-agent CLIs recognised when one is running in a plain terminal.
pub const AGENT_PROGRAMS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "gemini",
    "aider",
    "goose",
    "amp",
    "cursor-agent",
    "crush",
    "qwen",
    "kimi",
    "droid",
    "copilot",
    "cline",
    "auggie",
];

/// The agent a task runs: the one muster started, or one typed into a
/// terminal (`claude` in term-1). None for a terminal running anything else.
pub fn agent_kind(t: &TaskInfo) -> Option<&str> {
    if t.agent != "shell" {
        return Some(&t.agent);
    }
    t.fg.as_deref().filter(|f| AGENT_PROGRAMS.contains(f))
}

pub fn is_agent(t: &TaskInfo) -> bool {
    agent_kind(t).is_some()
}

/// How a message from another agent reads in the receiver's prompt. One
/// line: a newline typed into an agent's input box would submit it early.
pub fn message_line(from: Option<&str>, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match from {
        Some(f) => format!("[message from agent {f}; reply with: muster tell {f} \"...\"] {text}"),
        None => text,
    }
}

/// What an agent is told about working alongside the others. Claude Code
/// agents started by muster get it in their system prompt; any agent can
/// print it with `muster guide`.
pub const GUIDE: &str = "\
You are running inside muster, a workspace where several coding agents work side by side. \
Your own task id is in $MUSTER_TASK_ID. You can coordinate with the other agents through the \
`muster` command in your shell:
- `muster agents` lists every agent and terminal: id, status (working, idle, waiting, exited), \
folder and what it is working on.
- `muster tell <id> \"<message>\"` types a message into that agent's prompt and submits it. \
It is told the message came from you and how to answer.
- `muster read <id>` prints the last lines of its screen (`-n 80` for more).
- `muster wait <id>` blocks until it finishes its turn (`--timeout <secs>`, default 600).
A message from another agent arrives in your prompt as `[message from agent <id>; reply with: ...] <text>`. \
Answer it with `muster tell <id> \"...\"` once you have something to say. Only message other agents \
when the work calls for it: to hand something off, ask for something only they know, or report back.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_one_line_and_say_who_sent_them() {
        let m = message_line(Some("fix-auth"), "tests pass\nnow merge  it");
        assert_eq!(
            m,
            "[message from agent fix-auth; reply with: muster tell fix-auth \"...\"] tests pass now merge it"
        );
        assert_eq!(message_line(None, "hi\nthere"), "hi there");
    }
}
