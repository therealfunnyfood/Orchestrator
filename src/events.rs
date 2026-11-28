//! Transcripts: CLI stream-json lines become UI events, saved per thread as
//! JSONL and pushed live to the window over the websocket. Images that tools
//! return are written to the media folder and referenced by path.

use crate::store::now;
use base64::Engine;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

const MAX_FIELD: usize = 8000;

pub struct Hub {
    dir: PathBuf,
    pub media_dir: PathBuf,
    pub tx: broadcast::Sender<String>,
    counter: AtomicU64,
}

impl Hub {
    pub fn new(dir: PathBuf, media_dir: PathBuf) -> Hub {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::create_dir_all(&media_dir);
        let (tx, _) = broadcast::channel(8192);
        Hub { dir, media_dir, tx, counter: AtomicU64::new(0) }
    }

    pub fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.jsonl"))
    }

    /// Record an event in a thread's transcript and send it to live windows. Returns its
    /// time stamp (the chat anchor for "Jump to chat").
    pub fn emit(&self, key: &str, mut event: Value) -> f64 {
        let ts = now();
        event["ts"] = json!(ts);
        event["thread"] = json!(key);
        // Secret values never reach a transcript or the window.
        let line = crate::vault::redact(&event.to_string());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.path(key)) {
            let _ = writeln!(f, "{line}");
        }
        let _ = self.tx.send(line);
        ts
    }

    /// Send a transient event (not saved): state changes, streaming text.
    pub fn broadcast(&self, event: Value) {
        let _ = self.tx.send(crate::vault::redact(&event.to_string()));
    }

    pub fn history(&self, key: &str, limit: usize) -> Vec<Value> {
        let Ok(text) = std::fs::read_to_string(self.path(key)) else { return vec![] };
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(limit);
        lines[start..].iter().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Search one chat's transcript (case-insensitive) for `q`; hits newest first.
    pub fn search(&self, key: &str, thread: &str, q: &str, limit: usize) -> Vec<Value> {
        let Ok(text) = std::fs::read_to_string(self.path(key)) else { return vec![] };
        search_lines(&text, key, thread, q, limit)
    }

    pub fn remove(&self, key: &str) {
        let _ = std::fs::remove_file(self.path(key));
    }

    /// Save base64 data as a file in the media folder; returns its path.
    pub fn save_media(&self, data_b64: &str, ext: &str) -> Option<String> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64.trim()).ok()?;
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let path = self.media_dir.join(format!("{}-{n}.{ext}", (now() * 1000.0) as u64));
        std::fs::write(&path, bytes).ok()?;
        Some(path.to_string_lossy().into_owned())
    }

    /// Translate one line of `claude --output-format stream-json` into UI events.
    /// Streaming deltas are broadcast directly (not saved); the rest are returned.
    pub fn translate(&self, key: &str, line: &Value) -> Vec<Value> {
        let mut out = vec![];
        let sub = !line.get("parent_tool_use_id").map_or(true, Value::is_null);
        match line.get("type").and_then(Value::as_str) {
            Some("stream_event") if !sub => {
                let ev = line.get("event").unwrap_or(&Value::Null);
                match ev.get("type").and_then(Value::as_str) {
                    Some("content_block_delta") => {
                        if let Some(t) = ev.pointer("/delta/text").and_then(Value::as_str) {
                            self.broadcast(json!({"kind": "delta", "thread": key, "text": t}));
                        }
                    }
                    Some("message_start") => self.broadcast(json!({"kind": "draft_reset", "thread": key})),
                    _ => {}
                }
            }
            Some("assistant") => {
                let blocks = line.pointer("/message/content").and_then(Value::as_array);
                for b in blocks.into_iter().flatten() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.trim().is_empty() {
                                out.push(json!({"kind": "text", "text": text, "sub": sub}));
                            }
                        }
                        Some("tool_use") => {
                            let input = serde_json::to_string_pretty(b.get("input").unwrap_or(&Value::Null)).unwrap_or_default();
                            out.push(json!({
                                "kind": "tool_use", "id": b.get("id"), "name": b.get("name"),
                                "input": clip(&input), "sub": sub,
                            }));
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                let blocks = line.pointer("/message/content").and_then(Value::as_array);
                for b in blocks.into_iter().flatten() {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let (text, images) = self.tool_result_parts(b.get("content").unwrap_or(&Value::Null));
                        out.push(json!({
                            "kind": "tool_result", "id": b.get("tool_use_id"), "text": clip(&text), "images": images,
                            "is_error": b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        }));
                    }
                }
            }
            Some("result") => {
                out.push(json!({
                    "kind": "result",
                    "subtype": line.get("subtype"),
                    "is_error": line.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    "turns": line.get("num_turns"),
                    "cost": line.get("turn_cost_usd").or(line.get("total_cost_usd")).and_then(Value::as_f64).unwrap_or(0.0),
                    "duration_ms": line.get("duration_ms"),
                }));
            }
            _ => {}
        }
        out
    }

    fn tool_result_parts(&self, content: &Value) -> (String, Vec<String>) {
        match content {
            Value::String(s) => (s.clone(), vec![]),
            Value::Array(items) => {
                let mut texts = vec![];
                let mut images = vec![];
                for i in items {
                    match i.get("type").and_then(Value::as_str) {
                        Some("text") => texts.push(i.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
                        Some("image") => {
                            let mime = i.pointer("/source/media_type").and_then(Value::as_str).unwrap_or("image/png");
                            let ext = mime.rsplit('/').next().unwrap_or("png").replace("jpeg", "jpg");
                            match i.pointer("/source/data").and_then(Value::as_str).and_then(|d| self.save_media(d, &ext)) {
                                Some(path) => images.push(path),
                                None => texts.push("[image]".into()),
                            }
                        }
                        Some(other) => texts.push(format!("[{other}]")),
                        None => {}
                    }
                }
                (texts.join("\n"), images)
            }
            Value::Null => (String::new(), vec![]),
            other => (other.to_string(), vec![]),
        }
    }
}

pub fn clip(s: &str) -> String {
    if s.len() <= MAX_FIELD {
        return s.to_string();
    }
    let mut end = MAX_FIELD;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[{} more chars]", &s[..end], s.len() - end)
}

impl Hub {
    /// After a restart: mark conversations whose last turn never finished (the app
    /// was closed or crashed mid-turn) so the UI can offer to continue them.
    pub fn mark_interrupted(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(last) else { continue };
            let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
            // A finished turn ends with its result (or a notice); anything else means it was cut off.
            if kind == "result" || kind == "notice" || !text.contains("\"kind\":\"user\"") {
                continue;
            }
            let key = path.file_stem().unwrap().to_string_lossy().into_owned();
            self.emit(&key, json!({"kind": "notice", "text": "interrupted: the app closed in the middle of this turn", "interrupted": true}));
        }
    }
}

/// The searchable words of a saved chat event: what the user, an agent or a question said.
fn event_text(ev: &Value) -> Option<String> {
    match ev.get("kind").and_then(Value::as_str)? {
        "user" | "text" => ev.get("text").and_then(Value::as_str).map(str::to_string),
        "question" => Some(
            ev.get("questions").and_then(Value::as_array)?.iter()
                .filter_map(|q| q.get("question").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        "answer" => ev.get("answers").filter(|a| !a.is_null()).map(|a| match a {
            Value::Object(m) => m.iter().map(|(k, v)| format!("{k} {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect::<Vec<_>>().join(" "),
            v => v.to_string(),
        }),
        _ => None,
    }
}

/// About 60 characters either side of the first match, on one line.
fn snippet(text: &str, lower: &str, q: &str) -> String {
    let one = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(at) = lower.find(q) else { return one(&text.chars().take(120).collect::<String>()) };
    // Lowercasing can change byte lengths; go through char counts to stay on boundaries.
    let at_chars = lower[..at].chars().count();
    let q_chars = q.chars().count();
    let chars: Vec<char> = text.chars().collect();
    let start = at_chars.saturating_sub(60).min(chars.len());
    let end = (at_chars + q_chars + 60).min(chars.len());
    let mut s = one(&chars[start..end].iter().collect::<String>());
    if start > 0 {
        s = format!("…{s}");
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

/// Search JSONL transcript text; newest hits first, at most `limit`.
pub fn search_lines(text: &str, key: &str, thread: &str, q: &str, limit: usize) -> Vec<Value> {
    let q = q.trim().to_lowercase();
    if q.chars().count() < 2 {
        return vec![];
    }
    let mut out = vec![];
    for line in text.lines().rev() {
        if out.len() >= limit {
            break;
        }
        let Ok(ev) = serde_json::from_str::<Value>(line) else { continue };
        let Some(t) = event_text(&ev) else { continue };
        let lower = t.to_lowercase();
        if !lower.contains(&q) {
            continue;
        }
        out.push(json!({
            "key": key, "thread": thread, "ts": ev.get("ts").cloned().unwrap_or(Value::Null),
            "kind": ev.get("kind"), "sender": ev.get("sender").cloned().unwrap_or(Value::Null),
            "snippet": snippet(&t, &lower, &q),
        }));
    }
    out
}

/// Thread report flags for an orchestrator chat message the threads sent: the names in
/// its "[from thread X]" / "[from the X]" headers ("@X" for the app's own news).
pub fn report_from(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in text.lines() {
        let Some(inner) = l.trim().strip_prefix("[from ").and_then(|r| r.strip_suffix(']')) else { continue };
        let name = match inner.strip_prefix("thread ") {
            Some(n) => n.to_string(),
            None => format!("@{}", inner.strip_prefix("the ").unwrap_or(inner)),
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn search_finds_chat_words_newest_first() {
        let lines = [
            json!({"kind": "user", "text": "Please post the Fiverr gig", "sender": "you", "ts": 1.0}),
            json!({"kind": "tool_use", "name": "x", "input": "fiverr in a tool call is not chat", "ts": 2.0}),
            json!({"kind": "text", "text": "Done: the FIVERR gig is live. ".repeat(10), "ts": 3.0}),
            json!({"kind": "question", "questions": [{"question": "Which Fiverr tier?"}], "ts": 4.0}),
            json!({"kind": "answer", "answers": {"Which Fiverr tier?": "Basic"}, "ts": 5.0}),
            json!({"kind": "notice", "text": "fiverr notice", "ts": 6.0}),
        ];
        let text = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n") + "\nnot json fiverr";
        let hits = search_lines(&text, "k1", "orchestrator", "fiverr", 50);
        let ts: Vec<f64> = hits.iter().map(|h| h["ts"].as_f64().unwrap()).collect();
        assert_eq!(ts, vec![5.0, 4.0, 3.0, 1.0]);
        assert_eq!(hits[3]["sender"], "you");
        assert_eq!(hits[3]["key"], "k1");
        let long = hits[2]["snippet"].as_str().unwrap();
        assert!(long.to_lowercase().contains("fiverr") && long.ends_with('…') && long.chars().count() < 140, "{long}");
        // Capped, and too-short queries find nothing.
        assert_eq!(search_lines(&text, "k1", "o", "fiverr", 2).len(), 2);
        assert!(search_lines(&text, "k1", "o", "f", 50).is_empty());
        assert!(search_lines(&text, "k1", "o", "nothing here", 50).is_empty());
        // Non-ASCII text near the match doesn't break the snippet.
        let t = json!({"kind": "text", "text": "ÄÖÜ café straße — Größe gig", "ts": 9.0}).to_string();
        assert_eq!(search_lines(&t, "k", "o", "größe", 5)[0]["snippet"], "ÄÖÜ café straße — Größe gig");
    }

    #[test]
    fn report_headers_name_their_threads() {
        let t = "[from thread alpha]\nalpha finished.\n\n[from the approval queue]\nq1 approved\n\n[from thread beta]\nhi";
        assert_eq!(report_from(t), vec!["alpha", "@approval queue", "beta"]);
        assert!(report_from("plain text").is_empty());
    }
}
