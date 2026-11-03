//! Reads Claude Code's own saved conversations (~/.claude/projects/<folder>/<session>.jsonl)
//! so a project can be imported: list projects, index their sessions, condense a
//! session for a handoff, and convert one into Orchestrator transcript events.

use crate::config::home_dir;
use crate::events::Hub;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

pub fn projects_root() -> PathBuf {
    home_dir().join(".claude").join("projects")
}

/// Claude Code's folder name for a working directory: every non-alphanumeric
/// character becomes '-' (D:\dev\Dock -> D--dev-Dock).
pub fn encode_dir(path: &str) -> String {
    path.trim_end_matches(['\\', '/']).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn mtime_secs(p: &Path) -> f64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Text of a user message, or None for tool results, meta entries and
/// command/reminder wrappers that a person didn't type.
fn user_text(line: &Value) -> Option<String> {
    if line.get("isMeta").and_then(Value::as_bool) == Some(true) || line.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let content = line.pointer("/message/content")?;
    let raw = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            if items.iter().any(|i| i.get("type").and_then(Value::as_str) == Some("tool_result")) {
                return None;
            }
            items.iter().filter_map(|i| i.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
        }
        _ => return None,
    };
    let cleaned = unwrap_wake(&strip_tag(&raw, "system-reminder"));
    let t = cleaned.trim();
    if t.is_empty() || t.starts_with("<command-") || t.starts_with("<local-command") || t.starts_with("Caveat:") {
        return None;
    }
    Some(t.to_string())
}

/// Messages delivered from a channel arrive wrapped in a <wake …><message …>text</message></wake>
/// envelope; keep just what the people wrote.
fn unwrap_wake(s: &str) -> String {
    let t = s.trim_start();
    if t.starts_with("<project_claude_message") {
        return unwrap_relay(t);
    }
    if !t.starts_with("<wake") {
        return s.to_string();
    }
    let mut out = vec![];
    let mut rest = t;
    while let Some(i) = rest.find("<message") {
        let after = &rest[i..];
        let Some(open_end) = after.find('>') else { break };
        let header = &after[..open_end];
        let body = &after[open_end + 1..];
        let Some(close) = body.find("</message>") else { break };
        let text = body[..close].replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&");
        let who = if header.contains("from=\"human\"") { "" } else { "[agent] " };
        if !text.trim().is_empty() {
            out.push(format!("{who}{}", text.trim()));
        }
        rest = &body[close + 10..];
    }
    if out.is_empty() {
        return String::new();
    }
    out.join("\n")
}

/// A note relayed from another Claude session (e.g. a coordinator): label it so it's
/// never mistaken for something the user said.
fn unwrap_relay(t: &str) -> String {
    let from = t
        .find("from=\"")
        .map(|i| &t[i + 6..])
        .and_then(|r| r.find('"').map(|e| r[..e].to_string()))
        .unwrap_or_else(|| "another session".into());
    let note = match (t.find("<note>"), t.find("</note>")) {
        (Some(a), Some(b)) if b > a => t[a + 6..b].trim().to_string(),
        _ => strip_all_tags(t),
    };
    if note.is_empty() {
        return String::new();
    }
    format!("[relayed from the {from} session, not the user] {note}")
}

fn strip_all_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_tag(s: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(&open) {
        out.push_str(&rest[..i]);
        match rest[i..].find(&close) {
            Some(j) => rest = &rest[i + j + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn short(s: &str, n: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= n {
        one_line
    } else {
        one_line.chars().take(n).collect::<String>() + "…"
    }
}

#[derive(Clone)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub first_prompt: String,
    /// First prompt a person typed (not a relay from another agent).
    pub first_human: String,
    pub last_prompt: String,
    pub prompts: usize,
    pub last_active: f64,
    pub size: u64,
    pub cwd: String,
    pub branch: String,
}

impl SessionInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "title": self.title, "first_prompt": self.first_prompt, "last_prompt": self.last_prompt,
            "prompts": self.prompts, "last_active": self.last_active, "size": self.size, "cwd": self.cwd, "branch": self.branch,
        })
    }
}

/// Index one session file, cheaply: only user and title lines are parsed.
fn index_session(path: &Path) -> Option<SessionInfo> {
    let file = std::fs::File::open(path).ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut info = SessionInfo {
        id: path.file_stem()?.to_string_lossy().into_owned(),
        path: path.to_path_buf(),
        title: String::new(),
        first_prompt: String::new(),
        first_human: String::new(),
        last_prompt: String::new(),
        prompts: 0,
        last_active: mtime_secs(path),
        size,
        cwd: String::new(),
        branch: String::new(),
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let is_title = line.contains("\"custom-title\"") || line.contains("\"ai-title\"");
        let is_user = line.contains("\"type\":\"user\"");
        if !is_title && !is_user {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if is_title {
            if let Some(t) = v.get("customTitle").or_else(|| v.get("aiTitle")).or_else(|| v.get("title")).and_then(Value::as_str) {
                info.title = t.to_string();
            }
            continue;
        }
        if info.cwd.is_empty() {
            info.cwd = v.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
        }
        if let Some(b) = v.get("gitBranch").and_then(Value::as_str) {
            info.branch = b.to_string();
        }
        if let Some(t) = user_text(&v) {
            if info.first_prompt.is_empty() {
                info.first_prompt = short(&t, 200);
            }
            if info.first_human.is_empty() && !t.starts_with('[') {
                info.first_human = short(&t, 200);
            }
            info.last_prompt = short(&t, 200);
            info.prompts += 1;
        }
    }
    if info.prompts == 0 {
        return None; // empty or tool-only session
    }
    if info.title.is_empty() {
        let source = if info.first_human.is_empty() { &info.first_prompt } else { &info.first_human };
        info.title = short(source, 70);
    }
    Some(info)
}

/// Session index cache keyed by file path, invalidated by size + mtime.
pub struct Index {
    cache: Mutex<HashMap<PathBuf, (u64, f64, Option<SessionInfo>)>>,
}

impl Index {
    pub fn new() -> Index {
        Index { cache: Mutex::new(HashMap::new()) }
    }

    fn session(&self, path: &Path) -> Option<SessionInfo> {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mt = mtime_secs(path);
        if let Some((s, m, info)) = self.cache.lock().unwrap().get(path) {
            if *s == size && *m == mt {
                return info.clone();
            }
        }
        let info = index_session(path);
        self.cache.lock().unwrap().insert(path.to_path_buf(), (size, mt, info.clone()));
        info
    }

    /// Past sessions for a working directory, newest first.
    pub fn sessions_for(&self, workspace: &str) -> Vec<SessionInfo> {
        let dir = projects_root().join(encode_dir(workspace));
        let Ok(entries) = std::fs::read_dir(&dir) else { return vec![] };
        let mut out: Vec<SessionInfo> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .filter_map(|p| self.session(&p))
            .collect();
        out.sort_by(|a, b| b.last_active.total_cmp(&a.last_active));
        out
    }
}

/// Claude Code projects on this machine: real path, session count, last activity.
pub fn list_projects() -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(projects_root()) else { return vec![] };
    let mut out = vec![];
    for dir in entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()) {
        let mut files: Vec<(f64, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .map(|p| (mtime_secs(&p), p))
            .collect();
        if files.is_empty() {
            continue;
        }
        files.sort_by(|a, b| b.0.total_cmp(&a.0));
        // The real path comes from the "cwd" recorded in a session.
        let cwd = files.iter().take(5).find_map(|(_, p)| first_cwd(p));
        let Some(cwd) = cwd else { continue };
        if encode_dir(&cwd) != dir.file_name().unwrap().to_string_lossy() {
            continue; // a session that started elsewhere; skip ambiguous folders
        }
        let lower = cwd.to_lowercase();
        if lower.contains("\\appdata\\local\\temp") || !Path::new(&cwd).is_dir() {
            continue;
        }
        let size: u64 = files.iter().filter_map(|(_, p)| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        out.push(json!({"path": cwd, "sessions": files.len(), "last_active": files[0].0, "size": size}));
    }
    out.sort_by(|a, b| b["last_active"].as_f64().unwrap_or(0.0).total_cmp(&a["last_active"].as_f64().unwrap_or(0.0)));
    out
}

fn first_cwd(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().map_while(Result::ok).take(400) {
        if let Some(i) = line.find("\"cwd\":\"") {
            let rest = &line[i + 7..];
            let end = rest.find('"')?;
            let raw = format!("\"{}\"", &rest[..end]);
            return serde_json::from_str::<String>(&raw).ok();
        }
    }
    None
}

/// The conversation as readable text (prompts, replies, tool names), keeping the
/// most recent part when it's longer than `max_chars`. Used for handoffs.
pub fn condense(path: &Path, max_chars: usize) -> String {
    let Ok(file) = std::fs::File::open(path) else { return "(couldn't read session)".into() };
    let mut parts: Vec<String> = vec![];
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if !(line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\"")) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(t) = user_text(&v) {
                    parts.push(format!("\n## USER\n{}", short_block(&t, 3000)));
                }
            }
            Some("assistant") => {
                for b in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
                            if !t.is_empty() {
                                parts.push(format!("\n## ASSISTANT\n{}", short_block(t, 2500)));
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let input = b.get("input").cloned().unwrap_or(Value::Null);
                            let arg = ["file_path", "command", "pattern", "path", "url", "description"]
                                .iter()
                                .find_map(|k| input.get(*k).and_then(Value::as_str))
                                .map(|s| short(s, 120))
                                .unwrap_or_default();
                            parts.push(format!("  [tool] {name} {arg}"));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let full = parts.join("\n");
    if full.len() <= max_chars {
        return full;
    }
    let mut start = full.len() - max_chars;
    while !full.is_char_boundary(start) {
        start += 1;
    }
    format!("[… {} earlier characters omitted; this is the most recent part]\n{}", start, &full[start..])
}

fn short_block(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &s[..end])
}

/// Convert a past session into Orchestrator transcript events (most recent
/// `limit` messages), so a resumed thread shows its history.
pub fn to_events(hub: &Hub, key: &str, path: &Path, limit: usize) -> Vec<Value> {
    let Ok(file) = std::fs::File::open(path) else { return vec![] };
    let mut events: Vec<Value> = vec![];
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if !(line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\"")) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if v.get("type").and_then(Value::as_str) == Some("user") {
            if let Some(t) = user_text(&v) {
                events.push(json!({"kind": "user", "text": t, "sender": "you", "imported": true}));
                continue;
            }
        }
        events.extend(hub.translate(key, &v));
    }
    let skip = events.len().saturating_sub(limit);
    events.drain(..skip);
    events
}

/// UUID of the last main-conversation entry in a session file (the point a
/// continued conversation picks up from).
pub fn last_uuid(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut last = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if !(line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\"")) || line.contains("\"isSidechain\":true") {
            continue;
        }
        if let Some(i) = line.find("\"uuid\":\"") {
            let rest = &line[i + 8..];
            if let Some(end) = rest.find('"') {
                last = Some(rest[..end].to_string());
            }
        }
    }
    last
}

/// Every prompt the user typed across the given sessions, newest session first,
/// cut to `max_chars` in total. Rules and preferences live here.
pub fn all_user_messages(sessions: &[SessionInfo], max_chars: usize) -> String {
    // Every conversation gets a fair share, so one huge conversation can't crowd out the rest.
    let share = (max_chars / sessions.len().max(1)).max(600);
    let mut out = String::new();
    for s in sessions {
        let Ok(file) = std::fs::File::open(&s.path) else { continue };
        let mut msgs = vec![];
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("\"type\":\"user\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(t) = user_text(&v) {
                msgs.push(short_block(&t, 1500));
            }
        }
        if msgs.is_empty() {
            continue;
        }
        let mut block = format!("\n### {} ({})\n- {}\n", s.title, s.id, msgs.join("\n- "));
        if block.len() > share {
            let mut end = share;
            while !block.is_char_boundary(end) {
                end -= 1;
            }
            block.truncate(end);
            block.push_str("\n  …[more in this conversation: read_past_session]\n");
        }
        if out.len() + block.len() > max_chars {
            out.push_str("\n[… older conversations omitted; raise max_chars to see more]");
            break;
        }
        out.push_str(&block);
    }
    out
}

/// Claude Code's auto-memory folder for a project.
pub fn memory_dir(workspace: &str) -> PathBuf {
    projects_root().join(encode_dir(workspace)).join("memory")
}
