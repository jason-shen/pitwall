//! Ops investigations: an LLM loop that diagnoses fleet problems by running
//! policy-gated commands over SSH, then writes the session up as a runbook.

use std::fmt::Write as _;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use pitwall_core::proto::{ApprovalSource, Decision, HostResult, OpsEvent, Push};
use pitwall_core::{hosts, paths, policy};
use serde_json::{json, Value};

use super::{fleet, redact, Daemon};

pub fn start(d: Arc<Daemon>, question: String, selector: Option<String>) -> Result<u64> {
    let key = std::env::var(&d.cfg.ops.api_key_env).map_err(|_| {
        anyhow!(
            "set {} in the daemon's environment to use `pitwall ask`",
            d.cfg.ops.api_key_env
        )
    })?;
    if d.cfg.hosts.is_empty() {
        bail!(
            "no hosts configured; add [[hosts]] to {}",
            paths::config().display()
        );
    }
    let session = d.next_ops.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(async move {
        d.emit(Push::Ops(OpsEvent::Started {
            session,
            question: question.clone(),
        }));
        if let Err(e) = investigate(&d, session, &key, &question, selector.as_deref()).await {
            d.emit(Push::Ops(OpsEvent::Failed {
                session,
                error: format!("{e:#}"),
            }));
        }
    });
    Ok(session)
}

struct Step {
    hosts: Vec<String>,
    command: String,
    why: String,
    outcome: String,
}

fn tools() -> Value {
    json!([
        {
            "name": "run_command",
            "description": "Run one shell command on one or more hosts over SSH and get stdout/stderr/exit code per host. \
                Read-only commands run immediately. Mutating commands wait for a human to approve. Destructive commands are blocked. \
                Prefer narrow, read-only diagnostics (logs, status, metrics). Never try to bypass the policy.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "hosts": { "type": "string", "description": "Host selector: a name, a glob like web-*, tag:<tag>, all, or a comma-separated list" },
                    "command": { "type": "string", "description": "A single shell command" },
                    "why": { "type": "string", "description": "One sentence: what you expect to learn" }
                },
                "required": ["hosts", "command", "why"]
            }
        }
    ])
}

fn system_prompt(d: &Daemon, scope: Option<&str>) -> String {
    let mut s = String::from(
        "You are an experienced SRE investigating a production question by running commands on servers over SSH.\n\
         Work like a careful on-call engineer: form a hypothesis, run the smallest command that tests it, read the output, repeat.\n\
         Prefer read-only diagnostics. Every command is checked by a policy: read-only runs automatically, mutating commands need \
         a human to approve, destructive commands are blocked. Do not try to work around blocks.\n\
         Output from servers has secrets redacted and long output truncated.\n\n\
         When you have enough evidence, stop calling tools and answer in Markdown with these sections:\n\
         ## Summary\n## Evidence (commands and the key lines they showed)\n## Likely cause\n## Suggested fix (commands to run, NOT executed)\n\n\
         Available hosts:\n",
    );
    let targets = match scope {
        Some(sel) => hosts::select(&d.cfg.hosts, sel),
        None => d.cfg.hosts.iter().collect(),
    };
    for h in targets {
        let _ = writeln!(
            s,
            "- {} ({}) tags: [{}]",
            h.name,
            h.addr(),
            h.tags.join(", ")
        );
    }
    if let Some(sel) = scope {
        let _ = writeln!(s, "\nStay within hosts matching `{sel}`.");
    }
    s
}

fn format_results(results: &[HostResult], max: usize) -> String {
    let per = (max / results.len().max(1)).max(600);
    let mut out = String::new();
    for r in results {
        let _ = writeln!(
            out,
            "### {} (exit {}, {} ms)",
            r.host,
            r.exit.map(|c| c.to_string()).unwrap_or("?".into()),
            r.ms
        );
        if let Some(e) = &r.error {
            let _ = writeln!(out, "error: {e}");
        }
        if !r.stdout.trim().is_empty() {
            let _ = writeln!(
                out,
                "stdout:\n{}",
                redact::truncate(&redact::redact(&r.stdout), per)
            );
        }
        if !r.stderr.trim().is_empty() {
            let _ = writeln!(
                out,
                "stderr:\n{}",
                redact::truncate(&redact::redact(&r.stderr), per / 2)
            );
        }
    }
    out
}

async fn investigate(
    d: &Arc<Daemon>,
    session: u64,
    key: &str,
    question: &str,
    scope: Option<&str>,
) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    let url = format!("{}/v1/messages", d.cfg.ops.base_url.trim_end_matches('/'));
    let system = system_prompt(d, scope);
    let mut messages: Vec<Value> = vec![json!({ "role": "user", "content": question })];
    let mut steps: Vec<Step> = Vec::new();
    let emit = |e: OpsEvent| d.emit(Push::Ops(e));

    for _ in 0..d.cfg.ops.max_steps {
        let body = json!({
            "model": d.cfg.ops.model,
            "max_tokens": 4096,
            "system": system,
            "tools": tools(),
            "messages": messages,
        });
        let resp = http
            .post(&url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("call model API")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("decode model response")?;
        if !status.is_success() {
            bail!(
                "model API {status}: {}",
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            );
        }
        let content = v.get("content").cloned().unwrap_or(json!([]));
        messages.push(json!({ "role": "assistant", "content": content }));

        let mut text = String::new();
        let mut uses = Vec::new();
        for block in content.as_array().into_iter().flatten() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""))
                }
                Some("tool_use") => uses.push(block.clone()),
                _ => {}
            }
        }

        if uses.is_empty() {
            let runbook = write_runbook(question, &steps, &text).ok();
            emit(OpsEvent::Finished {
                session,
                answer: text,
                runbook: runbook.map(|p| p.display().to_string()),
            });
            return Ok(());
        }
        if !text.trim().is_empty() {
            emit(OpsEvent::Thinking {
                session,
                text: text.clone(),
            });
        }

        let mut results = Vec::new();
        for u in uses {
            let id = u
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let input = u.get("input").cloned().unwrap_or(json!({}));
            let (content, is_error) = run_tool(d, session, scope, &input, &mut steps).await;
            results.push(json!({ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error }));
        }
        messages.push(json!({ "role": "user", "content": results }));
    }
    let answer = format!(
        "Stopped after {} steps without a final answer.",
        d.cfg.ops.max_steps
    );
    let runbook = write_runbook(question, &steps, &answer).ok();
    emit(OpsEvent::Finished {
        session,
        answer,
        runbook: runbook.map(|p| p.display().to_string()),
    });
    Ok(())
}

async fn run_tool(
    d: &Arc<Daemon>,
    session: u64,
    scope: Option<&str>,
    input: &Value,
    steps: &mut Vec<Step>,
) -> (String, bool) {
    let sel = input
        .get("hosts")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let why = input
        .get("why")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if command.trim().is_empty() {
        return ("error: empty command".into(), true);
    }
    let mut targets: Vec<_> = hosts::select(&d.cfg.hosts, &sel)
        .into_iter()
        .cloned()
        .collect();
    if let Some(scope) = scope {
        let allowed: Vec<String> = hosts::select(&d.cfg.hosts, scope)
            .iter()
            .map(|h| h.name.clone())
            .collect();
        targets.retain(|h| allowed.contains(&h.name));
    }
    if targets.is_empty() {
        return (
            format!("error: selector `{sel}` matches no hosts in scope"),
            true,
        );
    }
    let names: Vec<String> = targets.iter().map(|h| h.name.clone()).collect();
    let c = policy::classify(&command, &d.cfg.policy);
    d.emit(Push::Ops(OpsEvent::Command {
        session,
        hosts: names.clone(),
        command: command.clone(),
        tier: c.tier,
        decision: c.decision,
        why: why.clone(),
    }));

    let audit = |outcome: &str| {
        d.audit.log(json!({ "source": "ops", "session": session, "hosts": names, "command": command, "tier": c.tier.label(), "outcome": outcome }));
    };

    match c.decision {
        Decision::Deny => {
            audit("blocked");
            let reason = format!("blocked by policy ({}): {}", c.tier.label(), c.reason);
            d.emit(Push::Ops(OpsEvent::Blocked {
                session,
                command: command.clone(),
                reason: reason.clone(),
            }));
            steps.push(Step {
                hosts: names.clone(),
                command: command.clone(),
                why,
                outcome: reason.clone(),
            });
            return (format!("error: {reason}. Choose a read-only way to gather this, or include it in Suggested fix."), true);
        }
        Decision::Ask => {
            let src = ApprovalSource::Ops {
                session,
                hosts: names.clone(),
            };
            match d
                .request_approval(
                    src,
                    command.clone(),
                    c.tier,
                    c.reason.clone(),
                    Duration::from_secs(600),
                )
                .await
            {
                Some(true) => audit("approved"),
                other => {
                    let reason = if other.is_none() {
                        "approval timed out"
                    } else {
                        "denied by the user"
                    };
                    audit(reason);
                    d.emit(Push::Ops(OpsEvent::Blocked {
                        session,
                        command: command.clone(),
                        reason: reason.into(),
                    }));
                    steps.push(Step {
                        hosts: names.clone(),
                        command: command.clone(),
                        why,
                        outcome: reason.into(),
                    });
                    return (format!("error: {reason}. Do not retry this command; continue with read-only checks."), true);
                }
            }
        }
        Decision::Allow => audit("auto"),
    }

    let results = fleet::run(&d.cfg, &targets, &command).await;
    d.emit(Push::Ops(OpsEvent::Results {
        session,
        command: command.clone(),
        results: results.clone(),
    }));
    let text = format_results(&results, d.cfg.ops.max_output_bytes);
    let summary = results
        .iter()
        .map(|r| {
            format!(
                "{}: {}",
                r.host,
                r.error.clone().unwrap_or_else(|| format!(
                    "exit {}",
                    r.exit.map(|c| c.to_string()).unwrap_or("?".into())
                ))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    steps.push(Step {
        hosts: names,
        command,
        why,
        outcome: summary,
    });
    (text, false)
}

fn write_runbook(question: &str, steps: &[Step], answer: &str) -> Result<std::path::PathBuf> {
    let dir = paths::runbooks();
    std::fs::create_dir_all(&dir)?;
    let slug: String = super::task::slug(question).chars().take(48).collect();
    let path = dir.join(format!(
        "{}-{slug}.md",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let mut md = format!(
        "# {question}\n\n_Investigated by pitwall on {}_\n\n## Timeline\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    );
    if steps.is_empty() {
        md.push_str("No commands were run.\n");
    }
    for (i, s) in steps.iter().enumerate() {
        let _ = writeln!(
            md,
            "{}. **{}** on `{}`  \n   `{}`  \n   → {}",
            i + 1,
            s.why,
            s.hosts.join(", "),
            s.command.replace('`', "'"),
            s.outcome
        );
    }
    let _ = write!(md, "\n---\n\n{answer}\n");
    std::fs::write(&path, md)?;
    Ok(path)
}
