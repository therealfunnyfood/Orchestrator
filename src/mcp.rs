//! Tools the app gives each Claude Code session, served as an MCP server
//! (streamable HTTP, JSON responses) at /mcp/{key} on the app's local server.
//! Every session gets the UI tools (task list, questions, media); the
//! orchestrator also gets the thread tools.

use crate::app::{App, ORCHESTRATOR};
use crate::store::ThreadRow;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const MAX_RESULT_CHARS: usize = 12000;

fn ui_tools() -> Value {
    json!([
        {
            "name": "approve",
            "description": "Internal: Claude Code calls this to ask the user for permission. Never call it yourself.",
            "inputSchema": {"type": "object", "properties": {
                "tool_name": {"type": "string"}, "input": {"type": "object"}, "tool_use_id": {"type": "string"},
            }, "required": ["tool_name"]},
        },
        {
            "name": "update_plan",
            "description": "Show the user a live task list for this conversation. Send the whole list every time \
(it replaces the previous one). Keep exactly one item in_progress while working; mark items completed as you \
finish them. Use it for work with three or more steps.",
            "inputSchema": {"type": "object", "properties": {
                "items": {"type": "array", "items": {"type": "object", "properties": {
                    "content": {"type": "string", "description": "short imperative step, e.g. 'Add login route'"},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                }, "required": ["content", "status"]}},
            }, "required": ["items"]},
        },
        {
            "name": "ask_user",
            "description": "Ask the user one to four multiple-choice questions and wait for their answers. The \
user can also type their own answer. Use only when a decision genuinely belongs to the user (a choice between \
approaches, a preference, approval for something risky); never for things you can decide or look up. Put a \
recommended option first and add ' (Recommended)' to its label.",
            "inputSchema": {"type": "object", "properties": {
                "questions": {"type": "array", "minItems": 1, "maxItems": 4, "items": {"type": "object", "properties": {
                    "question": {"type": "string", "description": "the full question, ending with '?'"},
                    "header": {"type": "string", "description": "very short label, max 12 chars"},
                    "options": {"type": "array", "minItems": 2, "maxItems": 5, "items": {"type": "object", "properties": {
                        "label": {"type": "string"}, "description": {"type": "string"},
                    }, "required": ["label"]}},
                    "multi_select": {"type": "boolean", "description": "allow several options"},
                }, "required": ["question", "options"]}},
            }, "required": ["questions"]},
        },
        {
            "name": "message_orchestrator",
            "description": "Send a message to the orchestrator that manages you, right now, without ending your turn: \
e.g. a shared resource is free, you're blocked and need something, or a result it asked you to report straight \
away. It's delivered immediately (and wakes the orchestrator if it's idle). Your final reply also reaches it, so \
use this only for things it needs before you finish.",
            "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"]},
        },
        {
            "name": "show_media",
            "description": "Show the user a file inline in their chat: an image (png, jpg, gif, webp, svg), video \
(mp4, webm), audio (mp3, wav, ogg) or PDF. Use it for screenshots, charts, generated images, recordings or \
documents the user should look at. Relative paths are taken from your working directory.",
            "inputSchema": {"type": "object", "properties": {
                "path": {"type": "string"}, "caption": {"type": "string"},
            }, "required": ["path"]},
        },
    ])
}

/// A thread task's estimate from spawn_thread / send_to_thread arguments.
fn task_estimate(args: &Value, task: &str) -> Option<crate::app::Estimate> {
    let low = args.get("estimate_minutes").and_then(Value::as_f64)?;
    crate::app::Estimate::from_minutes(low, args.get("estimate_max_minutes").and_then(Value::as_f64), arg(args, "kind").unwrap_or(""), task)
}

fn thread_tools() -> Value {
    let effort = json!({"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]});
    json!([
        {
            "name": "spawn_thread",
            "description": "Create a new worker thread and start it on a task (runs in the background). \
cwd defaults to the workspace folder; a relative cwd is taken inside the workspace. Billing (plan or \
API) is handled by the app from the user's project setting. model is optional (e.g. claude-opus-5-5, claude-sonnet-5-5, claude-haiku-5-5). effort is \
optional: low, medium, high, xhigh or max.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string", "description": "short unique id, e.g. 'api-tests'"},
                "task": {"type": "string", "description": "the full brief for the thread"},
                "cwd": {"type": "string"}, "model": {"type": "string"}, "effort": effort,
                "effort_reason": {"type": "string", "description": "required for xhigh/max: why it's worth it (shown to the user, who must approve)"},
                "estimate_minutes": {"type": "number", "description": "how long you expect this task to take, in minutes (low end if you give a range)"},
                "estimate_max_minutes": {"type": "number", "description": "high end of the range"},
                "kind": {"type": "string", "description": "kind of work, e.g. small-fix, bugfix, feature, refactor, tests, research, docs, review, setup"},
            }, "required": ["name", "task"]},
        },
        {
            "name": "send_to_thread",
            "description": "Send a message to an existing thread. If the thread is mid-turn it reads the message at its \
next step (use this to redirect or add info); otherwise it starts a turn in the background. when=\"after\" queues \
it to run once the current turn finishes instead.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"}, "message": {"type": "string"},
                "when": {"type": "string", "enum": ["now", "after"], "description": "default now"},
                "estimate_minutes": {"type": "number", "description": "if this starts new work: how long you expect it to take, in minutes"},
                "estimate_max_minutes": {"type": "number"},
                "kind": {"type": "string"},
            }, "required": ["name", "message"]},
        },
        {
            "name": "set_estimate",
            "description": "Record how long you expect the user's current request to take as a whole, until it and \
all its threads are done. Tell the user the same range. The app shows it counting down, times the real \
duration, and uses the results to calibrate your future estimates. Call again to revise it.",
            "inputSchema": {"type": "object", "properties": {
                "low_minutes": {"type": "number"}, "high_minutes": {"type": "number"},
                "kind": {"type": "string", "description": "kind of work, e.g. bugfix, feature, refactor, research"},
                "summary": {"type": "string", "description": "a few words on what's being done"},
            }, "required": ["low_minutes", "high_minutes", "summary"]},
        },
        {
            "name": "timing_report",
            "description": "How long work has really taken in this project over the last `days` (default 30): whole \
requests and thread tasks by kind (median and 80th-percentile minutes), how your estimates compared with \
the actual times, and your recent estimates. Use it to give realistic timelines.",
            "inputSchema": {"type": "object", "properties": {"days": {"type": "number"}}},
        },
        {
            "name": "wait_for_threads",
            "description": "Wait until the named threads (or all of this workspace's threads if names is empty) finish their current turn, or until timeout_seconds passes (default 900).",
            "inputSchema": {"type": "object", "properties": {
                "names": {"type": "array", "items": {"type": "string"}},
                "timeout_seconds": {"type": "number"},
            }},
        },
        {
            "name": "list_threads",
            "description": "List this workspace's threads with status, backend, model, effort and cost.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "read_thread",
            "description": "Read a thread's status, the last message sent to it and its last result.",
            "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
        },
        {
            "name": "configure_thread",
            "description": "Change a thread's model or effort for its next turns.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"}, "model": {"type": "string"}, "effort": effort,
                "effort_reason": {"type": "string", "description": "required for xhigh/max: why it's worth it (shown to the user, who must approve)"},
            }, "required": ["name"]},
        },
        {
            "name": "efficiency_report",
            "description": "Efficiency of this project's thread turns over the last `days` (default 7): per \
model+effort the number of turns, average and total API-rate cost, average time and steps, errors, and your \
verdicts; plus the 25 most recent turns. Use it to tune which model/effort you pick.",
            "inputSchema": {"type": "object", "properties": {"days": {"type": "number"}}},
        },
        {
            "name": "rate_thread",
            "description": "Record your verdict on a thread's latest turn: right (good fit), too_weak (it needed a \
stronger model/effort) or too_strong (a cheaper one would have done). note: a few words on why.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"}, "verdict": {"type": "string", "enum": ["right", "too_weak", "too_strong"]},
                "note": {"type": "string"},
            }, "required": ["name", "verdict"]},
        },
        {
            "name": "save_tuning_notes",
            "description": "Replace your tuning notes for this project: concrete rules about which model and effort \
fit which kinds of work, learned from efficiency_report and your verdicts. They're shown to you in every \
future conversation in this workspace.",
            "inputSchema": {"type": "object", "properties": {"notes": {"type": "string"}}, "required": ["notes"]},
        },
        {
            "name": "list_past_sessions",
            "description": "List earlier Claude Code conversations about this project (newest first): id, title, \
last activity, number of prompts, git branch, first and last prompt. Optional query filters by words in the \
title or prompts.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string"}, "limit": {"type": "number", "description": "default 30"},
            }},
        },
        {
            "name": "read_past_session",
            "description": "Read an earlier Claude Code conversation as text (prompts, replies, tool calls). Long \
conversations are cut to their most recent part.",
            "inputSchema": {"type": "object", "properties": {
                "id": {"type": "string"}, "max_chars": {"type": "number", "description": "default 24000, max 120000"},
            }, "required": ["id"]},
        },
        {
            "name": "past_user_messages",
            "description": "Everything the user typed across all earlier Claude Code conversations about this project \
(newest conversation first). Use it to collect the user's rules, preferences and standing instructions.",
            "inputSchema": {"type": "object", "properties": {
                "max_chars": {"type": "number", "description": "default 60000, max 200000"},
            }},
        },
        {
            "name": "resume_past_session",
            "description": "Continue an earlier Claude Code conversation as a new thread, with its full context. It \
works on a forked copy, so the original conversation is never changed. Prefer a handoff (spawn_thread with a \
complete brief) unless the old conversation's detailed context really matters. message, if given, is sent to it \
right away.",
            "inputSchema": {"type": "object", "properties": {
                "session_id": {"type": "string"}, "name": {"type": "string", "description": "thread name"},
                "message": {"type": "string"},
                "model": {"type": "string"}, "effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]},
            }, "required": ["session_id", "name"]},
        },
        {
            "name": "save_project_notes",
            "description": "Save this project's brief and rules. The brief is shown to you in every future \
conversation in this workspace; the rules are added to every thread's instructions. Each field you pass \
replaces the saved one.",
            "inputSchema": {"type": "object", "properties": {
                "brief": {"type": "string"}, "rules": {"type": "string"},
            }},
        },
        {
            "name": "resolve_thread",
            "description": "Mark a thread resolved once you're done with it: its work is finished and checked, or \
it's been handed off to another thread. Resolved threads are tucked away in the user's sidebar and left out \
of wait_for_threads and list_threads. Sending one new work reopens it. reopen=true reopens it now.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"},
                "note": {"type": "string", "description": "a few words: what it delivered, or where its work went"},
                "reopen": {"type": "boolean"},
            }, "required": ["name"]},
        },
        {
            "name": "stop_thread",
            "description": "Interrupt a thread's running turn. The thread keeps its history and can be messaged again.",
            "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
        },
    ])
}

fn summary(app: &App, t: &ThreadRow) -> String {
    let status = if app.busy(&t.name) { "running" } else { t.status.as_str() };
    format!(
        "- {}: {status}, model={}, effort={}, turns={}, cost=${:.4}, cwd={}",
        t.name,
        if t.model.is_empty() { "default" } else { &t.model },
        if t.effort.is_empty() { "default" } else { &t.effort },
        t.turns,
        t.cost_usd,
        t.cwd
    )
}

fn arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

pub const MEDIA_EXTS: [&str; 15] =
    ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "mp4", "webm", "mov", "mp3", "wav", "ogg", "m4a", "pdf"];

/// Working directory of a session (thread folder, or the workspace for the orchestrator).
fn session_cwd(app: &App, key: &str) -> String {
    if key.starts_with(ORCHESTRATOR) {
        return app.workspace();
    }
    app.store.lock().unwrap().get(key).map(|t| t.cwd.clone()).unwrap_or_else(|| app.workspace())
}

fn answer_text(questions: &Value, answers: &Value) -> String {
    let qs = questions.as_array().cloned().unwrap_or_default();
    let ans = answers.as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    for (i, q) in qs.iter().enumerate() {
        let a = ans.get(i).cloned().unwrap_or(Value::Null);
        let mut picked: Vec<String> = a.get("selected").and_then(Value::as_array)
            .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if let Some(other) = a.get("other").and_then(Value::as_str).filter(|o| !o.trim().is_empty()) {
            picked.push(format!("(own answer) {other}"));
        }
        let q_text = q.get("question").and_then(Value::as_str).unwrap_or("?");
        out.push(format!("Q: {q_text}\nA: {}", if picked.is_empty() { "(no answer)".into() } else { picked.join("; ") }));
    }
    out.join("\n\n")
}

/// Soft effort cap: above the cap needs the user's OK, asked right here (the tool
/// call waits for the answer). Returns the effort to use.
async fn approved_effort(app: &Arc<App>, key: &str, thread: &str, requested: Option<&str>, reason: &str) -> Result<Option<String>, String> {
    let Some(e) = requested else { return Ok(None) };
    let cap = { let c = app.settings().max_effort; if c.is_empty() { "high".to_string() } else { c } };
    if crate::config::effort_rank(e) <= crate::config::effort_rank(&cap) {
        return Ok(Some(e.to_string()));
    }
    let why = if reason.trim().is_empty() { "no reason given".to_string() } else { reason.trim().to_string() };
    let questions = json!([{
        "header": "Effort",
        "question": format!("Run thread {thread} at {e} effort? {e} uses much more of your plan than {cap}. Reason: {why}"),
        "options": [
            {"label": format!("Yes, use {e}"), "description": "Worth it for this task"},
            {"label": format!("No, use {cap}"), "description": "Keep usage down"},
        ],
    }]);
    let answer = app.ask_user(key, questions).await?;
    let yes = answer.pointer("/0/selected").and_then(Value::as_array)
        .is_some_and(|s| s.iter().any(|v| v.as_str().is_some_and(|t| t.starts_with("Yes"))));
    Ok(Some(if yes { e.to_string() } else { cap }))
}

async fn call(app: &Arc<App>, key: &str, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "message_orchestrator" => {
            if key.starts_with(ORCHESTRATOR) {
                return Err("you are the orchestrator; use send_to_thread to message threads".into());
            }
            let text = arg(args, "message").or(arg(args, "text")).filter(|m| !m.trim().is_empty()).ok_or("message is required")?;
            app.message_orchestrator(key, text)?;
            Ok("delivered to the orchestrator".into())
        }
        "update_plan" => {
            let items = args.get("items").cloned().unwrap_or(json!([]));
            if !items.is_array() {
                return Err("items must be a list".into());
            }
            let done = items.as_array().unwrap().iter().filter(|i| i.get("status").and_then(Value::as_str) == Some("completed")).count();
            app.set_plan(key, items.clone());
            Ok(format!("plan updated ({done}/{} done)", items.as_array().unwrap().len()))
        }
        "ask_user" => {
            let questions = args.get("questions").cloned().ok_or("questions is required")?;
            if questions.as_array().map_or(true, |q| q.is_empty()) {
                return Err("questions must be a non-empty list".into());
            }
            let answers = app.ask_user(key, questions.clone()).await?;
            Ok(answer_text(&questions, &answers))
        }
        "show_media" => {
            let raw = arg(args, "path").ok_or("path is required")?;
            let mut path = std::path::PathBuf::from(raw);
            if !path.is_absolute() {
                path = std::path::Path::new(&session_cwd(app, key)).join(path);
            }
            if !path.is_file() {
                return Err(format!("no file at {}", path.display()));
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if !MEDIA_EXTS.contains(&ext.as_str()) {
                return Err(format!("can't show .{ext} files; supported: {}", MEDIA_EXTS.join(", ")));
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            app.hub.emit(key, json!({
                "kind": "media", "path": path.to_string_lossy(), "ext": ext, "size": size,
                "caption": arg(args, "caption").unwrap_or(""),
            }));
            Ok(format!("showing {} to the user", path.display()))
        }
        "spawn_thread" => {
            let n = arg(args, "name").ok_or("name is required")?;
            let task = arg(args, "task").ok_or("task is required")?;
            let effort = approved_effort(app, key, n, arg(args, "effort"), arg(args, "effort_reason").unwrap_or("")).await?;
            let row = app.create_thread(n, arg(args, "cwd"), None, arg(args, "model"), effort.as_deref())?;
            app.note_thread_estimate(n, task_estimate(args, task));
            app.send_to_thread(n, task, "orchestrator", vec![], false)?;
            Ok(format!(
                "started thread {} in {} (model {}, effort {})",
                row.name, row.cwd, if row.model.is_empty() { "default" } else { &row.model }, if row.effort.is_empty() { "medium" } else { &row.effort }
            ))
        }
        "send_to_thread" => {
            let n = arg(args, "name").ok_or("name is required")?;
            let msg = arg(args, "message").ok_or("message is required")?;
            let now = arg(args, "when") != Some("after");
            app.note_thread_estimate(n, task_estimate(args, msg));
            Ok(match app.send_to_thread(n, msg, "orchestrator", vec![], now)? {
                "injected" => {
                    app.estimate_landed_mid_turn(n);
                    format!("delivered to {n} mid-turn; it reads it at its next step")
                }
                "queued" => format!("queued for {n}; runs after its current turn"),
                _ => format!("sent to {n}"),
            })
        }
        "wait_for_threads" => {
            let mut names: Vec<String> = args
                .get("names")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            if names.is_empty() {
                names = app.thread_names_here();
            }
            if let Some(m) = app.unanswered(key) {
                return Ok(format!(
                    "Not waiting yet: the user sent you a message and you haven't replied:\n\"{m}\"\n\n\
They can't see tool calls, only text you write. Write your reply to them as text now (answer it, or \
say what you'll do about it), then call wait_for_threads again."
                ));
            }
            let secs = args.get("timeout_seconds").and_then(Value::as_f64).unwrap_or(900.0).clamp(1.0, 3500.0);
            app.wait_threads(&names, Duration::from_secs_f64(secs)).await;
            let st = app.store.lock().unwrap();
            let mut lines: Vec<String> = names.iter().filter_map(|n| st.get(n)).map(|t| summary(app, t)).collect();
            let still: Vec<&String> = names.iter().filter(|n| app.busy(n)).collect();
            let still_list = still.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            let held = app.take_held(key);
            let nudged = app.take_nudge(key);
            let inbox = app.drain_inbox(key);
            let early = held.is_some() || nudged || inbox.is_some();
            if let Some(inbox) = inbox {
                lines.push(format!("\n[messages from threads]\n{inbox}"));
            }
            if let Some(h) = held {
                lines.push(format!("\n{h}"));
            } else if nudged {
                lines.push("(the user sent you a message, which follows this result: reply to them as text before anything else)".into());
            }
            if early && !still.is_empty() {
                lines.push(format!("(returned early; still running: {still_list})"));
            } else if !still.is_empty() {
                lines.push(format!("still running after timeout: {still_list}"));
            }
            Ok(if lines.is_empty() { "no threads".into() } else { lines.join("\n") })
        }
        "list_threads" => {
            let st = app.store.lock().unwrap();
            let rows = st.in_workspace(&st.workspace);
            let (done, open): (Vec<_>, Vec<_>) = rows.iter().partition(|t| t.resolved);
            let mut out = if open.is_empty() { "no open threads".to_string() } else { open.iter().map(|t| summary(app, t)).collect::<Vec<_>>().join("\n") };
            if !done.is_empty() {
                out += &format!("\nresolved: {}", done.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "));
            }
            Ok(if rows.is_empty() { "no threads yet".into() } else { out })
        }
        "read_thread" => {
            let n = arg(args, "name").ok_or("name is required")?;
            let st = app.store.lock().unwrap();
            let t = st.get(n).ok_or(format!("no thread named '{n}'"))?;
            let mut result = if t.last_result.is_empty() { "(no result yet)".to_string() } else { t.last_result.clone() };
            if result.len() > MAX_RESULT_CHARS {
                let mut end = MAX_RESULT_CHARS;
                while !result.is_char_boundary(end) {
                    end -= 1;
                }
                result.truncate(end);
                result.push_str("\n...[truncated]");
            }
            Ok(format!(
                "{}\ntranscript: {}\n\nlast message sent:\n{}\n\nlast result:\n{result}",
                summary(app, t),
                app.hub.path(n).display(),
                t.last_prompt
            ))
        }
        "list_past_sessions" => {
            let q = arg(args, "query").unwrap_or("").to_lowercase();
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(30).clamp(1, 200) as usize;
            let a = app.clone();
            let all = tokio::task::spawn_blocking(move || a.past_sessions()).await.map_err(|e| e.to_string())?;
            let total = all.len();
            let words: Vec<&str> = q.split_whitespace().collect();
            let hits: Vec<_> = all
                .into_iter()
                .filter(|s| {
                    let hay = format!("{} {} {}", s.title, s.first_prompt, s.last_prompt).to_lowercase();
                    words.iter().all(|w| hay.contains(w))
                })
                .take(limit)
                .collect();
            if hits.is_empty() {
                return Ok(format!("no matching past conversations ({total} in total)"));
            }
            let now = crate::store::now();
            let lines: Vec<String> = hits
                .iter()
                .map(|s| {
                    let age = (now - s.last_active) / 86400.0;
                    let when = if age < 1.0 { format!("{:.0}h ago", age * 24.0) } else { format!("{age:.0}d ago") };
                    format!(
                        "- {} | {} | {when} | {} prompts{} | first: {} | last: {}",
                        s.id, s.title, s.prompts,
                        if s.branch.is_empty() { String::new() } else { format!(" | branch {}", s.branch) },
                        s.first_prompt, s.last_prompt
                    )
                })
                .collect();
            Ok(format!("{} of {total} past conversations:\n{}", hits.len(), lines.join("\n")))
        }
        "read_past_session" => {
            let id = arg(args, "id").ok_or("id is required")?.to_string();
            let max = args.get("max_chars").and_then(Value::as_u64).unwrap_or(24000).clamp(2000, 120000) as usize;
            let a = app.clone();
            tokio::task::spawn_blocking(move || {
                let s = a.past_sessions().into_iter().find(|s| s.id == id).ok_or(format!("no past session {id}"))?;
                Ok::<String, String>(format!("# {}\n(last active {:.0} days ago, {} prompts)\n{}",
                    s.title, (crate::store::now() - s.last_active) / 86400.0, s.prompts, crate::claude_projects::condense(&s.path, max)))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "past_user_messages" => {
            let max = args.get("max_chars").and_then(Value::as_u64).unwrap_or(60000).clamp(5000, 200000) as usize;
            let a = app.clone();
            tokio::task::spawn_blocking(move || crate::claude_projects::all_user_messages(&a.past_sessions(), max))
                .await
                .map_err(|e| e.to_string())
        }
        "resume_past_session" => {
            let sid = arg(args, "session_id").ok_or("session_id is required")?.to_string();
            let name = arg(args, "name").ok_or("name is required")?.to_string();
            let (msg, backend, model, effort) = (
                arg(args, "message").unwrap_or("").to_string(),
                arg(args, "backend").map(str::to_string),
                arg(args, "model").map(str::to_string),
                arg(args, "effort").map(str::to_string),
            );
            let a = app.clone();
            let row = tokio::task::spawn_blocking(move || {
                a.resume_past_session(&name, &sid, &msg, backend.as_deref(), model.as_deref(), effort.as_deref())
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(format!("thread {} continues that conversation (forked copy) in {}", row.name, row.cwd))
        }
        "set_estimate" => {
            let low = args.get("low_minutes").and_then(Value::as_f64).ok_or("low_minutes is required")?;
            let high = args.get("high_minutes").and_then(Value::as_f64);
            let est = crate::app::Estimate::from_minutes(low, high, arg(args, "kind").unwrap_or(""), arg(args, "summary").unwrap_or(""))
                .ok_or("low_minutes must be more than 0")?;
            let range = format!("{:.1}-{:.1} min", est.low_s / 60.0, est.high_s / 60.0).replace(".0", "");
            app.set_job_estimate(est);
            Ok(format!("recorded {range}; the clock runs until you and all threads are idle"))
        }
        "timing_report" => {
            let days = args.get("days").and_then(Value::as_f64).unwrap_or(30.0).clamp(0.1, 365.0);
            Ok(serde_json::to_string_pretty(&app.timing_json(days)).unwrap_or_default())
        }
        "efficiency_report" => {
            let days = args.get("days").and_then(Value::as_f64).unwrap_or(7.0).clamp(0.1, 90.0);
            Ok(serde_json::to_string_pretty(&app.efficiency_json(days)).unwrap_or_default())
        }
        "rate_thread" => {
            let n = arg(args, "name").ok_or("name is required")?;
            app.rate_thread(n, arg(args, "verdict").unwrap_or(""), arg(args, "note").unwrap_or(""))?;
            Ok("recorded".into())
        }
        "save_tuning_notes" => {
            app.save_tuning(arg(args, "notes").ok_or("notes is required")?);
            Ok("saved".into())
        }
        "save_project_notes" => {
            app.save_notes(arg(args, "brief"), arg(args, "rules"));
            Ok("saved".into())
        }
        "configure_thread" => {
            let n = arg(args, "name").ok_or("name is required")?;
            let effort = approved_effort(app, key, n, arg(args, "effort"), arg(args, "effort_reason").unwrap_or("")).await?;
            app.configure_thread(n, None, arg(args, "model"), effort.as_deref())?;
            Ok(format!("updated {n}"))
        }
        "resolve_thread" => {
            let n = arg(args, "name").ok_or("name is required")?;
            let reopen = args.get("reopen").and_then(Value::as_bool).unwrap_or(false);
            app.set_resolved(n, !reopen, arg(args, "note").unwrap_or(""), "orchestrator")?;
            Ok(if reopen { format!("reopened {n}") } else { format!("resolved {n}") })
        }
        "stop_thread" => {
            let n = arg(args, "name").ok_or("name is required")?;
            Ok(if app.stop(n) { "stopped".into() } else { "thread was not running".into() })
        }
        other => Err(format!("unknown tool {other}")),
    }
}

/// Handle one JSON-RPC message for session `key`. Returns None for notifications.
pub async fn handle(app: &Arc<App>, key: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "app", "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => {
            let mut tools = ui_tools().as_array().cloned().unwrap_or_default();
            if key.starts_with(ORCHESTRATOR) {
                tools.extend(thread_tools().as_array().cloned().unwrap_or_default());
            }
            json!({"tools": tools})
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if name == "approve" {
                let tool = args.get("tool_name").and_then(Value::as_str).unwrap_or("").to_string();
                let input = args.get("input").cloned().unwrap_or(json!({}));
                let decision = app.approve(key, &tool, input).await;
                return Some(json!({"jsonrpc": "2.0", "id": id, "result": {
                    "content": [{"type": "text", "text": decision.to_string()}], "isError": false,
                }}));
            }
            let allowed = key.starts_with(ORCHESTRATOR)
                || ["update_plan", "ask_user", "show_media", "message_orchestrator"].contains(&name);
            let outcome = if allowed { call(app, key, name, &args).await } else { Err(format!("{name} is only available to the orchestrator")) };
            match outcome {
                Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
                Err(e) => json!({"content": [{"type": "text", "text": format!("error: {e}")}], "isError": true}),
            }
        }
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {method}")}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}
