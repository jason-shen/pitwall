//! Claude Code hook handling (`muster hook claude` forwards here).
//!
//! * PreToolUse  → snapshot first, then apply the command policy; commands that
//!   need approval wait in muster's inbox (or defer to Claude's own prompt).
//! * Stop        → the agent finished its turn: mark idle.
//! * Notification→ the agent is asking for something: mark waiting.

use std::sync::Arc;
use std::time::Duration;

use muster_core::policy;
use muster_core::proto::{ApprovalSource, Decision, HookReply, HookRequest, TaskStatus, Tier};
use serde_json::Value;

use super::Daemon;

fn none() -> HookReply {
    HookReply {
        decision: None,
        reason: String::new(),
    }
}

fn summarize_tool(tool: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let detail = match tool {
        "Bash" => s("command").to_string(),
        "Edit" | "Write" | "MultiEdit" | "Read" => s("file_path").to_string(),
        "NotebookEdit" => s("notebook_path").to_string(),
        _ => String::new(),
    };
    let detail: String = detail
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    if detail.is_empty() {
        tool.to_string()
    } else {
        format!("{tool}: {detail}")
    }
}

/// The shell command in a tool call, whatever the agent calls its shell tool:
/// Claude's Bash sends `command`; Codex's shell tools send a string or an
/// argv, sometimes wrapped in `bash -lc`. None for tools that aren't one.
pub fn shell_command(input: &Value) -> Option<String> {
    let v = input.get("command").or_else(|| input.get("cmd"))?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let argv: Vec<&str> = v.as_array()?.iter().filter_map(Value::as_str).collect();
    let shells = ["bash", "sh", "zsh", "/bin/bash", "/bin/sh", "/bin/zsh"];
    match argv.as_slice() {
        [sh, flag, script, ..] if shells.contains(sh) && flag.ends_with('c') && flag.starts_with('-') => {
            Some(script.to_string())
        }
        [] => None,
        _ => Some(
            argv.iter()
                .map(|a| {
                    if a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
                        a.to_string()
                    } else {
                        format!("'{}'", a.replace('\'', "'\\''"))
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
    }
}

pub async fn handle(d: &Arc<Daemon>, req: HookRequest) -> HookReply {
    let Ok(payload) = serde_json::from_str::<Value>(&req.payload) else {
        return none();
    };
    let event = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let task = req.task_id.as_deref().and_then(|id| d.task(id).ok());

    match event {
        "Stop" => {
            if let Some(t) = &task {
                d.set_status(t, TaskStatus::Idle);
                if d.cfg.snapshots.auto {
                    d.snapshot(t, "turn ended".into()).await;
                }
            }
            none()
        }
        "Notification" => {
            if let Some(t) = &task {
                let msg = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("needs your attention");
                d.set_status(
                    t,
                    TaskStatus::Waiting {
                        reason: msg.chars().take(160).collect(),
                    },
                );
            }
            none()
        }
        // Codex asking you to approve something on its own prompt.
        "PermissionRequest" => {
            if let Some(t) = &task {
                let what = payload
                    .get("tool_input")
                    .and_then(shell_command)
                    .map(|c| format!("wants to run {c}"))
                    .unwrap_or_else(|| "waiting for your approval".into());
                d.set_status(
                    t,
                    TaskStatus::Waiting {
                        reason: what.chars().take(160).collect(),
                    },
                );
            }
            none()
        }
        "PreToolUse" => pre_tool(d, task, &payload).await,
        _ => none(),
    }
}

async fn pre_tool(
    d: &Arc<Daemon>,
    task: Option<Arc<super::task::Task>>,
    payload: &Value,
) -> HookReply {
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);

    // 1. Snapshot *before* the tool runs, so "revert to here" undoes this step.
    // A read-only command has no step to undo, and agents run a lot of them;
    // each snapshot walks the whole worktree, which adds up in a big repo.
    let command = shell_command(&input);
    let read_only = command.as_deref().is_some_and(|c| {
        policy::tier_of(c, &d.cfg.policy.read_only_programs).0 == muster_core::proto::Tier::ReadOnly
    });
    if let Some(t) = task.as_ref().filter(|_| !read_only) {
        if d.cfg.hooks.snapshot_before.iter().any(|x| x == &tool) {
            d.snapshot(t, format!("before {}", summarize_tool(&tool, &input)))
                .await;
        }
    }

    let Some(command) = command else {
        return none();
    };

    // 2. Policy.
    let c = policy::classify(&command, &d.cfg.policy);
    let task_id = task.as_ref().map(|t| t.id.clone());
    let log = |outcome: &str| {
        d.audit.log(serde_json::json!({
            "source": "hook", "task": task_id, "tool": tool, "command": command,
            "tier": c.tier.label(), "decision": c.decision, "outcome": outcome, "reason": c.reason,
        }));
    };

    match c.decision {
        Decision::Allow => {
            log("allowed");
            if d.cfg.hooks.auto_allow_read_only && c.tier == Tier::ReadOnly
                || c.reason.starts_with("rule")
            {
                HookReply {
                    decision: Some(Decision::Allow),
                    reason: format!("muster: {}", c.reason),
                }
            } else {
                none()
            }
        }
        Decision::Deny => {
            log("denied");
            HookReply {
                decision: Some(Decision::Deny),
                reason: format!(
                    "Blocked by muster policy ({} command: {}). Do not try to work around this; ask the user or find a safer approach.",
                    c.tier.label(),
                    c.reason
                ),
            }
        }
        Decision::Ask => {
            if d.cfg.hooks.mode != "queue" || task.is_none() {
                log("deferred to agent prompt");
                return HookReply {
                    decision: Some(Decision::Ask),
                    reason: format!("muster: {} ({})", c.tier.label(), c.reason),
                };
            }
            let t = task.unwrap();
            let src = ApprovalSource::Task {
                task_id: t.id.clone(),
                tool: tool.clone(),
            };
            let timeout = Duration::from_secs(d.cfg.hooks.timeout_secs.max(5));
            match d
                .request_approval(src, command.clone(), c.tier, c.reason.clone(), timeout)
                .await
            {
                Some(true) => {
                    log("approved in muster");
                    HookReply {
                        decision: Some(Decision::Allow),
                        reason: "approved in muster".into(),
                    }
                }
                Some(false) => {
                    log("denied in muster");
                    HookReply {
                        decision: Some(Decision::Deny),
                        reason: "The user denied this command in muster. Ask them how to proceed."
                            .into(),
                    }
                }
                None => {
                    log("approval timed out; deferred to agent prompt");
                    HookReply {
                        decision: Some(Decision::Ask),
                        reason: "muster approval timed out".into(),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_commands_are_found_in_claude_and_codex_tool_calls() {
        use serde_json::json;
        let c = |v| super::shell_command(&v);
        assert_eq!(c(json!({"command": "ls -la"})).as_deref(), Some("ls -la"));
        assert_eq!(c(json!({"command": ["bash", "-lc", "rm -rf x && ls"]})).as_deref(), Some("rm -rf x && ls"));
        assert_eq!(c(json!({"cmd": "git status"})).as_deref(), Some("git status"));
        assert_eq!(c(json!({"command": ["git", "commit", "-m", "a b"]})).as_deref(), Some("git commit -m 'a b'"));
        assert_eq!(c(json!({"file_path": "a.rs"})), None);
    }
}
