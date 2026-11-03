//! Runs one turn of a Claude Code session by driving the `claude` CLI
//! (`-p --output-format stream-json`), with plan -> api fallback.
//!
//!   plan - the CLI's own claude.ai login (subscription); no API key is passed
//!   api  - ANTHROPIC_API_KEY from settings (API credits)
//!   auto - plan, rerun on api if the subscription is rate/usage limited

use crate::config::Settings;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct TurnSpec {
    pub prompt: String,
    pub cwd: PathBuf,
    pub resume: Option<String>,
    pub model: String,
    pub effort: String,
    pub backend: String,
    pub append_system: String,
    /// Comma-separated built-in tool allowlist (None = all).
    pub tools: Option<String>,
    pub mcp_config: Option<String>,
    pub long_tool_calls: bool,
    /// Attached images as (media type, base64 data).
    pub images: Vec<(String, String)>,
    /// Branch from this transcript entry (edit/retry): load the session only up
    /// to it, under a new session id.
    pub resume_at: Option<String>,
    /// Continue `resume` under a new session id, leaving the original untouched
    /// (used for conversations imported from Claude Code).
    pub fork_session: bool,
    /// Claude Code permission mode. Anything but bypassPermissions routes approvals
    /// to the app's `approve` tool (needs mcp_config).
    pub permission_mode: String,
    /// Messages sent while the turn runs (stream-json user lines, see `user_line`).
    /// Claude Code reads them at its next step, like typing while Claude works.
    pub inject: Option<tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>>,
}

/// One stream-json user message for Claude Code's stdin.
pub fn user_line(prompt: &str, images: &[(String, String)]) -> String {
    let mut content = vec![serde_json::json!({"type": "text", "text": prompt})];
    for (mime, data) in images {
        content.push(serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}}));
    }
    serde_json::json!({"type": "user", "message": {"role": "user", "content": content}}).to_string() + "\n"
}

#[derive(Default)]
pub struct TurnResult {
    pub session_id: Option<String>,
    pub text: String,
    pub is_error: bool,
    pub cost_usd: f64,
    /// Agent steps (model round trips) and wall time of the turn.
    pub steps: u64,
    pub duration_ms: u64,
    pub backend_used: String,
    pub key_source: String,
    pub limited: bool,
    pub notes: Vec<String>,
}

pub enum TurnError {
    Stopped,
    Failed(String),
}

/// True when this process was started from inside a Claude Code session (the
/// desktop app's terminal, for example); its session plumbing must not leak
/// into our CLI subprocesses.
fn launched_inside_claude() -> bool {
    std::env::var_os("CLAUDECODE").is_some() || std::env::var_os("CLAUDE_CODE_HOST_SESSION_ID").is_some()
}

fn build_command(settings: &Settings, spec: &TurnSpec, backend: &str, resume: Option<&str>) -> Result<Command, String> {
    let mut cmd = Command::new(settings.claude_exe());
    cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"]);
    let mode = if spec.permission_mode.is_empty() { "bypassPermissions" } else { spec.permission_mode.as_str() };
    cmd.args(["--include-partial-messages", "--permission-mode", mode]);
    if mode != "bypassPermissions" && spec.mcp_config.is_some() {
        // Approvals show up in the app as cards (see mcp.rs `approve`).
        cmd.args(["--permission-prompt-tool", "mcp__app__approve"]);
    }
    cmd.arg("--setting-sources=user,project,local");
    // Questions go through the app's own ask_user tool (rendered as clickable cards), and
    // sessions talk to each other only through the app (message_orchestrator,
    // send_to_thread): Claude Code's own cross-session tools would reach the user's other
    // Claude sessions instead.
    cmd.args(["--disallowedTools", "AskUserQuestion,SendMessage,ListAgents"]);
    if let Some(r) = resume {
        cmd.arg(format!("--resume={r}"));
        if let Some(at) = &spec.resume_at {
            cmd.arg(format!("--resume-session-at={at}"));
        }
        if spec.resume_at.is_some() || spec.fork_session {
            cmd.arg("--fork-session");
        }
    }
    if !spec.model.is_empty() {
        cmd.args(["--model", &spec.model]);
    }
    if !spec.effort.is_empty() {
        cmd.args(["--effort", &spec.effort]);
    }
    if !spec.append_system.is_empty() {
        cmd.args(["--append-system-prompt", &spec.append_system]);
    }
    if let Some(tools) = &spec.tools {
        cmd.args(["--tools", tools]);
    }
    if let Some(mcp) = &spec.mcp_config {
        cmd.args(["--mcp-config", mcp]);
    }

    if launched_inside_claude() {
        for (k, _) in std::env::vars() {
            if k.starts_with("CLAUDE") || k == "ANTHROPIC_BASE_URL" || k == "BAGGAGE" {
                cmd.env_remove(k);
            }
        }
    }
    for k in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_CUSTOM_HEADERS"] {
        cmd.env_remove(k);
    }
    if backend == "api" {
        if settings.api_key.trim().is_empty() {
            return Err("the api backend needs an API key (Settings tab)".into());
        }
        cmd.env("ANTHROPIC_API_KEY", settings.api_key.trim());
        if !settings.api_workspace_id.trim().is_empty() {
            // Keys not scoped to a workspace must name one on every request.
            cmd.env("ANTHROPIC_CUSTOM_HEADERS", format!("anthropic-workspace-id: {}", settings.api_workspace_id.trim()));
        }
    }
    if spec.long_tool_calls {
        // wait_for_threads can legitimately block for many minutes.
        cmd.env("MCP_TOOL_TIMEOUT", "3600000");
    }

    cmd.current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    Ok(cmd)
}

/// Kill a process and everything it started (shells, servers, ...).
pub fn kill_tree(pid: u32) {
    let mut k = std::process::Command::new("taskkill");
    k.args(["/PID", &pid.to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        k.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = k.status();
}

fn looks_limited(line: &Value) -> bool {
    if !line.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        return false;
    }
    if line.get("api_error_status").and_then(Value::as_i64) == Some(429) {
        return true;
    }
    let text = line.get("result").and_then(Value::as_str).unwrap_or("").to_lowercase();
    text.contains("limit") && (text.contains("usage") || text.contains("rate") || text.contains("reset"))
}

async fn run_once(
    settings: &Settings,
    spec: &TurnSpec,
    prompt: &str,
    backend: &str,
    resume: Option<&str>,
    cancel: &mut watch::Receiver<bool>,
    on_line: &mut (dyn FnMut(&Value) + Send),
) -> Result<TurnResult, TurnError> {
    let mut cmd = build_command(settings, spec, backend, resume).map_err(TurnError::Failed)?;
    let mut child = cmd.spawn().map_err(|e| {
        TurnError::Failed(format!("could not start Claude Code ({}): {e}", settings.claude_exe().display()))
    })?;
    let pid = child.id().unwrap_or(0);

    // stdin stays open while the turn runs so messages can be added mid-turn; closing
    // it after the result lets Claude Code exit.
    let mut stdin = child.stdin.take().unwrap();
    let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = writer_rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
        let _ = stdin.shutdown().await;
    });
    let _ = writer_tx.send(user_line(prompt, &spec.images));
    let mut writer = Some(writer_tx);
    let mut inject = match &spec.inject {
        Some(rx) => Some(rx.lock().await),
        None => None,
    };
    let mut stderr = child.stderr.take().unwrap();
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        let text = String::from_utf8_lossy(&buf).to_string();
        let tail: String = text.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
        tail
    });

    let mut out = TurnResult { session_id: resume.map(str::to_string), backend_used: backend.into(), ..Default::default() };
    let mut got_result = false;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                    out.session_id = Some(sid.to_string());
                }
                match v.get("type").and_then(Value::as_str) {
                    Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                        let src = v.get("apiKeySource").and_then(Value::as_str).unwrap_or("");
                        out.key_source = if src.is_empty() || src == "none" { "Claude subscription".into() } else { src.into() };
                    }
                    Some("rate_limit_event") => {
                        if v.pointer("/rate_limit_info/status").and_then(Value::as_str) == Some("rejected") {
                            out.limited = true;
                        }
                    }
                    Some("result") => {
                        got_result = true;
                        // A message that came in as the turn ended gets one more result.
                        out.text = v.get("result").and_then(Value::as_str).unwrap_or("").to_string();
                        out.is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                        out.cost_usd = out.cost_usd.max(v.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0));
                        out.steps += v.get("num_turns").and_then(Value::as_u64).unwrap_or(0);
                        out.duration_ms += v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
                        out.limited |= looks_limited(&v);
                        if out.is_error && out.text.is_empty() {
                            let errs = v.get("errors").map(Value::to_string).unwrap_or_default();
                            out.text = format!("[{}] {errs}", v.get("subtype").and_then(Value::as_str).unwrap_or("error"));
                        }
                    }
                    _ => {}
                }
                if v.get("type").and_then(Value::as_str) == Some("result") {
                    // Stop taking messages (senders fall back to the queue), but pass on
                    // any that already arrived; Claude Code answers them before exiting.
                    let mut more = false;
                    if let Some(rx) = inject.as_mut() {
                        rx.close();
                        while let Ok(line) = rx.try_recv() {
                            more |= writer.as_ref().is_some_and(|w| w.send(line).is_ok());
                        }
                    }
                    if !more {
                        writer = None;
                    }
                }
                on_line(&v);
            }
            line = async { inject.as_mut().unwrap().recv().await }, if inject.is_some() => {
                match (line, &writer) {
                    (Some(line), Some(w)) => { let _ = w.send(line); }
                    (None, _) => inject = None,
                    _ => {}
                }
            }
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    kill_tree(pid);
                    let _ = child.kill().await;
                    return Err(TurnError::Stopped);
                }
            }
        }
    }
    let _ = child.wait().await;
    let err_tail = stderr_task.await.unwrap_or_default();
    if !got_result {
        let msg = err_tail.trim();
        return Err(TurnError::Failed(if msg.is_empty() { "Claude Code exited without a result".into() } else { msg.to_string() }));
    }
    Ok(out)
}

pub async fn run_turn(
    settings: &Settings,
    spec: &TurnSpec,
    cancel: &mut watch::Receiver<bool>,
    on_line: &mut (dyn FnMut(&Value) + Send),
) -> Result<TurnResult, TurnError> {
    let first = if spec.backend == "auto" { "plan" } else { spec.backend.as_str() };
    let result = run_once(settings, spec, &spec.prompt, first, spec.resume.as_deref(), cancel, on_line).await?;

    if spec.backend == "auto" && result.limited && !settings.api_key.trim().is_empty() {
        let retry_prompt = format!(
            "Your previous turn was cut off by a usage limit. Pick up where you left off and finish this request:\n\n{}",
            spec.prompt
        );
        let plain = TurnSpec { resume_at: None, images: spec.images.clone(), tools: spec.tools.clone(), mcp_config: spec.mcp_config.clone(),
            prompt: spec.prompt.clone(), cwd: spec.cwd.clone(), resume: None, model: spec.model.clone(), effort: spec.effort.clone(),
            backend: spec.backend.clone(), append_system: spec.append_system.clone(), long_tool_calls: spec.long_tool_calls, fork_session: false,
            permission_mode: spec.permission_mode.clone(), inject: None };
        let mut retry = run_once(settings, &plain, &retry_prompt, "api", result.session_id.as_deref(), cancel, on_line).await?;
        retry.cost_usd += result.cost_usd;
        retry.steps += result.steps;
        retry.duration_ms += result.duration_ms;
        retry.notes.push("plan was rate-limited; finished on api".into());
        return Ok(retry);
    }
    Ok(result)
}
