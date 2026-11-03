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

    /// Record an event in a thread's transcript and send it to live windows.
    pub fn emit(&self, key: &str, mut event: Value) {
        event["ts"] = json!(now());
        event["thread"] = json!(key);
        let line = event.to_string();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.path(key)) {
            let _ = writeln!(f, "{line}");
        }
        let _ = self.tx.send(line);
    }

    /// Send a transient event (not saved): state changes, streaming text.
    pub fn broadcast(&self, event: Value) {
        let _ = self.tx.send(event.to_string());
    }

    pub fn history(&self, key: &str, limit: usize) -> Vec<Value> {
        let Ok(text) = std::fs::read_to_string(self.path(key)) else { return vec![] };
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(limit);
        lines[start..].iter().filter_map(|l| serde_json::from_str(l).ok()).collect()
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
