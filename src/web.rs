//! Local HTTP server behind the window: UI, JSON API, live websocket, and the
//! orchestrator's MCP endpoint.
//!
//! Threads can run with permissions bypassed, so this server is effectively remote
//! code execution for anyone who can call it. It listens on 127.0.0.1 only,
//! checks the Host header (DNS rebinding) and requires a random per-launch
//! token on every call (other websites can't read it, so they can't forge
//! requests).

use crate::app::{App, ORCHESTRATOR};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

const INDEX: &str = include_str!("../ui/index.html");
const CSS: &str = include_str!("../ui/app.css");
const JS: &str = include_str!("../ui/app.js");

type Shared = Arc<App>;

struct ApiError(String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({"error": self.0}))).into_response()
    }
}

impl From<String> for ApiError {
    fn from(s: String) -> Self {
        ApiError(s)
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

async fn guard(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let port = app.port.load(Ordering::Relaxed);
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return (StatusCode::FORBIDDEN, "bad host").into_response();
    }
    if !req.uri().path().starts_with("/static/") {
        let from_header = req.headers().get("x-orch-token").and_then(|h| h.to_str().ok()).map(str::to_string);
        let from_query = req.uri().query().and_then(|q| {
            q.split('&').find_map(|kv| kv.strip_prefix("t=").map(str::to_string))
        });
        let given = from_header.or(from_query).unwrap_or_default();
        if given.len() != app.token.len() || !constant_eq(given.as_bytes(), app.token.as_bytes()) {
            return (StatusCode::FORBIDDEN, "bad token").into_response();
        }
    }
    next.run(req).await
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn router(app: Shared, desktop: bool) -> Router {
    let index = INDEX.replace("__TOKEN__", &app.token).replace("__DESKTOP__", if desktop { "true" } else { "false" });
    Router::new()
        .route("/", get(move || async move { Html(index) }))
        .route("/static/app.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], CSS) }))
        .route("/static/app.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], JS) }))
        .route("/ws", get(ws))
        .route("/mcp/{key}", post(mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route("/file", get(file))
        .route("/api/upload", post(upload))
        .route("/api/open", post(open_path))
        .route("/api/doc", post(read_doc))
        .route("/api/handoff", post(handoff))
        .route("/api/popout", post(popout))
        .route("/api/claude-projects", get(claude_projects))
        .route("/api/past-sessions", get(past_sessions))
        .route("/api/open-url", post(open_url))
        .route("/api/check-update", post(check_update))
        .route("/api/setup/status", get(setup_status))
        .route("/api/setup/install-cli", post(|| async { crate::setup::install_cli().map(|_| Json(json!({"ok": true}))).map_err(ApiError) }))
        .route("/api/setup/login", post(setup_login))
        .route("/api/setup/finish", post(setup_finish))
        .route("/api/efficiency", get(|State(a): State<Shared>| async move { Json(a.efficiency_json(&a.workspace(), 7.0)) }))
        .route("/api/timing", get(|State(a): State<Shared>| async move { Json(a.timing_json(&a.workspace(), 30.0)) }))
        .route("/api/tuning", post(tuning))
        .route("/api/show", post(|| async { Json(json!({"shown": crate::show_main()})) }))
        .route("/api/plan-usage/refresh", post(refresh_plan_usage))
        .route("/api/inbox", post(inbox_drop))
        .route("/api/queue/drop", post(queue_drop))
        // Read-only business feed for a paired phone (off unless turned on per workspace).
        .route("/api/money", get(|State(a): State<Shared>| async move { let ws = a.workspace(); Json(a.money_json(&ws)) }))
        .route("/api/business", get(|State(a): State<Shared>| async move { Json(json!({"v": 1, "workspaces": a.feed_list()})) }))
        .route("/api/business/{ws}/snapshot", get(feed_snapshot))
        .route("/api/business/{ws}/events", get(feed_events))
        .route("/api/business/{ws}/attachment/{item}/{name}", get(feed_attachment))
        // The one route that decides: only with a signature from an enrolled phone.
        .route("/api/business/{ws}/decide", post(phone_decide))
        // Drafts change nothing; pausing is safe; unpausing and Done are signed.
        .route("/api/business/{ws}/revise", post(phone_revise))
        .route("/api/business/{ws}/pause", post(phone_pause))
        .route("/api/business/{ws}/unpause", post(phone_unpause))
        .route("/api/business/{ws}/manual/{item}/file", post(phone_manual_file))
        .route("/api/business/{ws}/manual/{item}/done", post(phone_manual_done))
        .route("/api/needs", get(|State(a): State<Shared>| async move { let ws = a.workspace(); Json(json!({"needs": a.needs_json(&ws), "needs_all": a.needs_all_json()})) }))
        .route("/api/xws/rules", get(|State(a): State<Shared>| async move { Json(json!({"rules": a.xws_rules_json()})) }))
        .route("/api/state", get(|State(a): State<Shared>| async move { Json(a.state_json()) }))
        .route("/api/all-threads", get(|State(a): State<Shared>| async move { Json(a.all_threads_json()) }))
        .route("/api/events/{key}", get(events))
        .route("/api/search", get(search))
        .route("/api/stop", post(stop))
        .route("/api/workspace", post(workspace))
        .route("/api/pick-folder", post(pick_folder))
        .route("/api/settings", get(|State(a): State<Shared>| async move { Json(a.settings_json()) }).post(save_settings))
        .route("/api/auth-check", post(|State(a): State<Shared>| async move { Json(a.auth_check().await) }))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

async fn ws(State(app): State<Shared>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| pump(app, socket))
}

async fn pump(app: Shared, mut socket: WebSocket) {
    let mut rx = app.hub.tx.subscribe();
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => if socket.send(Message::Text(text.into())).await.is_err() { break },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Too far behind: ask the page to reload its view.
                    if socket.send(Message::Text(json!({"kind": "resync"}).to_string().into())).await.is_err() { break }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}

async fn mcp(State(app): State<Shared>, Path(key): Path<String>, headers: axum::http::HeaderMap, Json(body): Json<Value>) -> Response {
    // The shared token isn't enough: every agent holds it. Only the session's own secret
    // (in its MCP config) lets a caller use that session's tools.
    let secret = headers.get(crate::app::MCP_SECRET_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !app.mcp_secret_ok(&key, secret) {
        return (StatusCode::FORBIDDEN, "bad session secret").into_response();
    }
    let streams = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/event-stream"));
    let token = body.pointer("/params/_meta/progressToken").cloned();
    if body.get("method").and_then(Value::as_str) == Some("tools/call") && streams && token.is_some() {
        return mcp_streamed(app, key, body, token.unwrap());
    }
    match crate::mcp::handle(&app, &key, &body).await {
        Some(reply) => Json(reply).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn events(State(app): State<Shared>, Path(key): Path<String>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(3000);
    Json(Value::Array(app.hub.history(&key, limit)))
}

/// Read-only search of the shown workspace's chats: `?q=...&limit=...` (default 50, max 200).
async fn search(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let text = q.get("q").cloned().unwrap_or_default();
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
    let ws = app.workspace();
    let out = tokio::task::spawn_blocking(move || crate::app::search_chats(&app, &ws, &text, limit)).await;
    Json(out.unwrap_or_else(|_| json!({"hits": [], "truncated": false})))
}

async fn stop(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    Ok(Json(json!({"stopped": app.stop(s(&b, "thread").unwrap_or(""))})))
}

/// Hand a chat (orchestrator or thread) off to a new session.
async fn handoff(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let key = s(&b, "key").unwrap_or("").to_string();
    let delivery = app.handoff(&key)?;
    Ok(Json(json!({"ok": true, "delivery": delivery})))
}

async fn workspace(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let ws = app.set_workspace(s(&b, "path").unwrap_or(""), b.get("create").and_then(Value::as_bool).unwrap_or(false))?;
    Ok(Json(json!({"workspace": ws})))
}

async fn pick_folder(Json(b): Json<Value>) -> ApiResult {
    let start = s(&b, "start").unwrap_or("").to_string();
    let title = s(&b, "title").unwrap_or("Choose a folder").to_string();
    let picked = tokio::task::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title(&title);
        if !start.is_empty() && std::path::Path::new(&start).is_dir() {
            dialog = dialog.set_directory(&start);
        }
        dialog.pick_folder()
    })
    .await
    .map_err(|e| ApiError(e.to_string()))?;
    Ok(Json(json!({"path": picked.map(|p| p.to_string_lossy().into_owned())})))
}

async fn save_settings(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    // Gates and secrets (the effort cap, permission default, API key) change only from the
    // window (IPC op settings_user): agents can reach this route.
    if let Some(k) = crate::app::USER_ONLY_SETTINGS.iter().find(|k| b.get(**k).is_some()) {
        return Err(ApiError(format!("{k} can only be changed in the app's window")));
    }
    app.update_settings(&b)?;
    Ok(Json(app.settings_json()))
}


/// Save a pasted or dropped file so it can be attached to a message.
async fn upload(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    use base64::Engine;
    let name = s(&b, "name").unwrap_or("file");
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || ".-_".contains(c) { c } else { '_' }).collect();
    let data = base64::engine::general_purpose::STANDARD
        .decode(s(&b, "data").unwrap_or(""))
        .map_err(|e| ApiError(format!("bad upload: {e}")))?;
    if data.len() > 50_000_000 {
        return Err(ApiError("file too large (50 MB max)".into()));
    }
    let dir = app.paths.data_dir.join("uploads");
    std::fs::create_dir_all(&dir).map_err(|e| ApiError(e.to_string()))?;
    let path = dir.join(format!("{}-{safe}", (crate::store::now() * 1000.0) as u64));
    std::fs::write(&path, data).map_err(|e| ApiError(e.to_string()))?;
    Ok(Json(json!({"path": path.to_string_lossy()})))
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Serve a local media file for display in the chat (media types only).
async fn file(Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(path) = q.get("path").map(std::path::PathBuf::from) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !crate::mcp::MEDIA_EXTS.contains(&ext.as_str()) {
        return (StatusCode::FORBIDDEN, "not a media file").into_response();
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, mime_for(&ext)),
                // Never let a served file (e.g. an SVG) run script in the app's origin.
                (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; media-src 'self'"),
                (header::CACHE_CONTROL, "private, max-age=60"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Turn a path from a message into an absolute Windows path: strips `file:///`, expands
/// `~`, and takes a relative path from `base` (the chat's folder), then the workspace.
fn resolve_path(app: &App, raw: &str, base: Option<&str>) -> std::path::PathBuf {
    let raw = raw.trim().trim_start_matches("file:///").trim_start_matches("file://");
    let p = raw.replace('/', "\\");
    let p = match p.strip_prefix("~\\") {
        Some(rest) => std::env::var("USERPROFILE").map(|h| format!("{h}\\{rest}")).unwrap_or(p.clone()),
        None => p,
    };
    let path = std::path::PathBuf::from(&p);
    if path.is_absolute() {
        return path;
    }
    let ws = app.workspace();
    let bases = base.into_iter().map(str::to_string).chain(std::iter::once(ws));
    let tries: Vec<_> = bases.map(|b| normalize(&std::path::Path::new(&b).join(&path))).collect();
    tries.iter().find(|t| t.exists()).cloned().unwrap_or_else(|| tries[0].clone())
}

/// Drop `.` and resolve `..` without touching the disk.
fn normalize(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Read a markdown or text file for the in-app viewer.
async fn read_doc(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let path = resolve_path(&app, s(&b, "path").unwrap_or(""), s(&b, "base"));
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !["md", "markdown", "mdx", "txt"].contains(&ext.as_str()) {
        return Err(ApiError("only markdown and text files open in the viewer".into()));
    }
    let meta = tokio::fs::metadata(&path).await.map_err(|_| ApiError(format!("not found: {}", path.display())))?;
    if meta.len() > 4 * 1024 * 1024 {
        return Err(ApiError("file is too large to view (over 4 MB)".into()));
    }
    let bytes = tokio::fs::read(&path).await.map_err(|e| ApiError(e.to_string()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(Json(json!({
        "path": path.to_string_lossy(),
        "dir": path.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default(),
        "name": name,
        "text": String::from_utf8_lossy(&bytes),
    })))
}

/// Open a path: in VS Code when available ("editor"), or in Explorer ("reveal").
async fn open_path(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let resolved = resolve_path(&app, s(&b, "path").unwrap_or(""), s(&b, "base"));
    let path = resolved.to_string_lossy().to_string();
    if path.is_empty() || !resolved.exists() {
        return Err(ApiError(format!("not found: {path}")));
    }
    let how = s(&b, "how").unwrap_or("editor").to_string();
    let line = b.get("line").and_then(Value::as_u64);
    tokio::task::spawn_blocking(move || {
        let has_code = std::env::var_os("PATH").map_or(false, |p| {
            std::env::split_paths(&p).any(|d| d.join("code.cmd").is_file() || d.join("code").is_file())
        });
        let mut cmd = if how == "editor" && has_code {
            let target = match line { Some(l) => format!("{path}:{l}"), None => path.clone() };
            let mut c = std::process::Command::new("cmd");
            c.args(["/c", "code", "-g", &target]);
            c
        } else if how == "reveal" && std::path::Path::new(&path).is_file() {
            let mut c = std::process::Command::new("explorer.exe");
            c.arg(format!("/select,{path}"));
            c
        } else {
            let mut c = std::process::Command::new("explorer.exe");
            c.arg(&path);
            c
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| ApiError(e.to_string()))?
    .map_err(ApiError)?;
    Ok(Json(json!({"ok": true})))
}

/// Open a conversation in its own native window (desktop app only).
async fn popout(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let key = s(&b, "thread").unwrap_or("").to_string();
    if !key.starts_with(ORCHESTRATOR) && app.store.lock().unwrap().get(&key).is_none() {
        return Err(ApiError(format!("no thread named '{key}'")));
    }
    let title = if key.starts_with(ORCHESTRATOR) { "orchestrator".to_string() } else { key.clone() };
    Ok(Json(json!({"native": crate::pop_out(&key, &title)})))
}

/// Claude Code projects that can be imported.
async fn claude_projects(State(app): State<Shared>) -> ApiResult {
    let mut list = tokio::task::spawn_blocking(crate::claude_projects::list_projects).await.map_err(|e| ApiError(e.to_string()))?;
    let st = app.store.lock().unwrap();
    for p in &mut list {
        let path = p["path"].as_str().unwrap_or("").to_string();
        p["imported"] = json!(st.notes(&path).and_then(|n| n.imported_at).is_some());
    }
    Ok(Json(json!({"projects": list})))
}

/// Switch to a Claude Code project and start the orchestrator's setup run.
async fn past_sessions(State(app): State<Shared>) -> ApiResult {
    let a = app.clone();
    let ws = app.workspace();
    let list = tokio::task::spawn_blocking(move || a.past_sessions(&ws)).await.map_err(|e| ApiError(e.to_string()))?;
    Ok(Json(json!({"sessions": list.iter().map(|s| s.to_json()).collect::<Vec<_>>()})))
}

async fn refresh_plan_usage(State(app): State<Shared>) -> ApiResult {
    Ok(Json(json!({"plan_usage": app.refresh_plan_usage().await?})))
}

/// The Inbox drop zone (text or one file as base64). Clipboard capture isn't here: it only
/// runs from the window's IPC channel or the hotkey, so agents can't read the clipboard.
async fn inbox_drop(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let text = s(&b, "text");
    let file = match (s(&b, "name"), s(&b, "data")) {
        (Some(n), Some(d)) => Some((n, d)),
        _ => None,
    };
    let path = app.inbox_drop(text, file).map_err(ApiError)?;
    Ok(Json(json!({"ok": true, "path": path})))
}

/// The workspace behind a feed id, or 404 when there's none with the phone feed on.
fn feed_ws(app: &App, id: &str) -> Result<String, Response> {
    app.feed_ws(id).ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error": "no_feed", "message": "no business workspace with that id has the phone feed on"}))).into_response())
}

async fn feed_snapshot(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    match feed_ws(&app, &id) {
        Ok(ws) => Json(app.feed_snapshot(&ws)).into_response(),
        Err(r) => r,
    }
}

/// Server-sent events: the workspace's business changes, as they happen.
async fn feed_events(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    if let Err(r) = feed_ws(&app, &id) {
        return r;
    }
    let rx = app.hub.tx.subscribe();
    let stream = futures_util::stream::unfold((rx, id), |(mut rx, id)| async move {
        loop {
            match rx.recv().await {
                Ok(line) => {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                    if v.get("kind").and_then(Value::as_str) == Some("business") && v.get("ws").and_then(Value::as_str) == Some(id.as_str()) {
                        let ev = Event::default().event(v.get("event").and_then(Value::as_str).unwrap_or("change")).data(line);
                        return Some((Ok::<Event, std::convert::Infallible>(ev), (rx, id)));
                    }
                }
                // Missed events: tell the phone to reload the snapshot.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let ev = Event::default().event("resync").data(r#"{"event":"resync"}"#);
                    return Some((Ok(ev), (rx, id)));
                }
                Err(_) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// A pending item's attachment: `?size=thumb` (default; JPEG, at most 512 px) or `full`.
async fn feed_attachment(State(app): State<Shared>, Path((id, item, name)): Path<(String, String, String)>, Query(q): Query<HashMap<String, String>>) -> Response {
    let ws = match feed_ws(&app, &id) {
        Ok(ws) => ws,
        Err(r) => return r,
    };
    let Some(path) = app.feed_attachment(&ws, &item, &name) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "no_attachment"}))).into_response();
    };
    let full = q.get("size").is_some_and(|s| s == "full");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let headers = |ct: &'static str| [
        (header::CONTENT_TYPE, ct),
        (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'"),
        (header::CACHE_CONTROL, "private, max-age=300"),
    ];
    if !full {
        let p = path.clone();
        match tokio::task::spawn_blocking(move || crate::business::thumbnail(&p, 512)).await {
            Ok(Ok(jpg)) => return (headers("image/jpeg"), jpg).into_response(),
            _ => return (StatusCode::UNSUPPORTED_MEDIA_TYPE, Json(json!({"error": "no_thumbnail", "message": "not an image; ask for size=full"}))).into_response(),
        }
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => (headers(mime_for(&ext)), bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "no_attachment"}))).into_response(),
    }
}

/// A phone's signed approve or reject. The token gets the request here; only the signature
/// (checked in phone::check_decide) can make it count.
async fn phone_decide(State(app): State<Shared>, Path(id): Path<String>, body: axum::body::Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<crate::phone::DecideRequest>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "bad_request", "message": "expected JSON {item_id, decision, reason, device, ts, nonce, sig}"}))).into_response();
    };
    match app.phone_decide(&id, &req) {
        Ok(v) => Json(v).into_response(),
        Err((status, code, message)) => (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST), Json(json!({"error": code, "message": message}))).into_response(),
    }
}

fn refusal_response(r: crate::phone::Refusal) -> Response {
    let (status, code, message) = r;
    (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST), Json(json!({"error": code, "message": message}))).into_response()
}

fn phone_result(r: Result<Value, crate::phone::Refusal>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => refusal_response(e),
    }
}

fn body_json(body: &[u8]) -> Result<Value, Response> {
    serde_json::from_slice(body).map_err(|_| refusal_response((400, "bad_request", "expected a JSON body".into())))
}

async fn phone_revise(State(app): State<Shared>, Path(id): Path<String>, body: axum::body::Bytes) -> Response {
    let b = match body_json(&body) { Ok(b) => b, Err(r) => return r };
    phone_result(app.phone_revise(&id, s(&b, "item_id").unwrap_or(""), s(&b, "payload").unwrap_or("")))
}

async fn phone_pause(State(app): State<Shared>, Path(id): Path<String>, body: axum::body::Bytes) -> Response {
    let b = match body_json(&body) { Ok(b) => b, Err(r) => return r };
    phone_result(app.phone_pause(&id, s(&b, "target").unwrap_or("")))
}

async fn phone_unpause(State(app): State<Shared>, Path(id): Path<String>, body: axum::body::Bytes) -> Response {
    let b = match body_json(&body) { Ok(b) => b, Err(r) => return r };
    let signed: crate::phone::Signed = serde_json::from_value(b.clone()).unwrap_or_default();
    phone_result(app.phone_unpause(&id, s(&b, "target").unwrap_or(""), &signed))
}

async fn phone_manual_file(State(app): State<Shared>, Path((id, item)): Path<(String, String)>, body: axum::body::Bytes) -> Response {
    let b = match body_json(&body) { Ok(b) => b, Err(r) => return r };
    phone_result(app.phone_manual_file(&id, &item, s(&b, "name").unwrap_or("file"), s(&b, "data").unwrap_or("")))
}

async fn phone_manual_done(State(app): State<Shared>, Path((id, item)): Path<(String, String)>, body: axum::body::Bytes) -> Response {
    let b = match body_json(&body) { Ok(b) => b, Err(r) => return r };
    let signed: crate::phone::Signed = serde_json::from_value(b).unwrap_or_default();
    phone_result(app.phone_manual_done(&id, &item, &signed))
}

/// A file dropped on a manual task's card (saved to inbox/images/<id>/).
async fn queue_drop(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let (id, name, data) = (s(&b, "id").unwrap_or(""), s(&b, "name").unwrap_or("file"), s(&b, "data").unwrap_or(""));
    let path = app.queue_drop_file(id, name, data).map_err(ApiError)?;
    Ok(Json(json!({"ok": true, "path": path})))
}

async fn tuning(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    app.save_tuning(&app.workspace(), s(&b, "notes").unwrap_or(""));
    Ok(Json(json!({"ok": true})))
}

async fn setup_status(State(app): State<Shared>) -> ApiResult {
    let s = app.settings();
    Ok(Json(tokio::task::spawn_blocking(move || crate::setup::status(&s)).await.map_err(|e| ApiError(e.to_string()))?))
}

async fn setup_login(State(app): State<Shared>) -> ApiResult {
    crate::setup::login(&app.settings())?;
    Ok(Json(json!({"ok": true})))
}

/// Finish the wizard (the window sets the mix and permission level over IPC).
async fn setup_finish(State(app): State<Shared>, Json(_b): Json<Value>) -> ApiResult {
    app.update_settings(&json!({"setup_done": true}))?;
    // The plan/API mix and the permission level are set by the window over IPC, not here.
    Ok(Json(json!({"ok": true})))
}

/// Open a web link in the default browser (http/https only).
async fn open_url(Json(b): Json<Value>) -> ApiResult {
    let url = s(&b, "url").unwrap_or("").to_string();
    let ok = (url.starts_with("https://") || url.starts_with("http://"))
        && !url.chars().any(|c| c.is_whitespace() || "\"^&|<>".contains(c));
    if !ok {
        return Err(ApiError("only plain http(s) links can be opened".into()));
    }
    let mut cmd = std::process::Command::new("rundll32.exe");
    cmd.args(["url.dll,FileProtocolHandler", &url]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn().map_err(|e| ApiError(e.to_string()))?;
    Ok(Json(json!({"ok": true})))
}

async fn check_update(State(app): State<Shared>) -> ApiResult {
    let found = tokio::task::spawn_blocking(crate::updates::check).await.map_err(|e| ApiError(e.to_string()))?;
    *app.update.lock().unwrap() = found.clone();
    app.touch();
    Ok(Json(json!({"update": found, "version": crate::updates::VERSION})))
}

/// A tool call answered as a server-sent event stream: progress pings every 20s
/// while it waits (on the user, or on threads), then the result. Claude Code
/// gives up on a tool that's silent for 5 minutes.
fn mcp_streamed(app: Shared, key: String, body: Value, token: Value) -> Response {
    use axum::response::sse::{Event, Sse};
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(8);
    tokio::spawn(async move {
        let call = crate::mcp::handle(&app, &key, &body);
        tokio::pin!(call);
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
        tick.tick().await;
        let mut n = 0u64;
        loop {
            tokio::select! {
                reply = &mut call => {
                    if let Some(reply) = reply {
                        let _ = tx.send(Event::default().event("message").data(reply.to_string())).await;
                    }
                    break;
                }
                _ = tick.tick() => {
                    n += 1;
                    let progress = json!({"jsonrpc": "2.0", "method": "notifications/progress",
                        "params": {"progressToken": token, "progress": n, "message": "still waiting"}});
                    // The client hung up (turn stopped): drop the call, which cleans up its question.
                    if tx.send(Event::default().event("message").data(progress.to_string())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok::<_, std::convert::Infallible>(ev), rx))
    });
    Sse::new(stream).into_response()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve(name: &str) -> (Arc<App>, u16, String) {
        let root = std::env::temp_dir().join(format!("orch-web-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("dev");
        std::fs::create_dir_all(&ws).unwrap();
        let ws = ws.to_string_lossy().to_string();
        std::fs::write(root.join("state.json"), json!({"workspace": ws, "open_workspaces": [ws]}).to_string()).unwrap();
        let app = App::new(Paths { config_dir: root.clone(), data_dir: root.clone() }, "t".repeat(40));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        app.port.store(port, Ordering::Relaxed);
        let r = router(app.clone(), false);
        tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
        (app, port, crate::app::orch_key(&ws))
    }

    async fn post(port: u16, path: &str, extra: &str, body: &str) -> (u16, String) {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Orch-Token: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            "t".repeat(40), body.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        let code = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        (code, out)
    }

    /// The shared token is not enough for /mcp/{key}: only that session's own secret works.
    #[tokio::test]
    async fn mcp_needs_the_sessions_own_secret() {
        let (app, port, key) = serve("secret").await;
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let path = format!("/mcp/{key}");
        let h = crate::app::MCP_SECRET_HEADER;
        // Nothing issued yet: refused even with the token.
        assert_eq!(post(port, &path, "", body).await.0, 403);
        let good = app.mcp_secret(&key);
        let other = app.mcp_secret("worker-b");
        assert_ne!(good, other);
        assert_eq!(post(port, &path, "", body).await.0, 403, "missing secret");
        assert_eq!(post(port, &path, &format!("{h}: {}\r\n", "x".repeat(40)), body).await.0, 403, "wrong secret");
        assert_eq!(post(port, &path, &format!("{h}: {other}\r\n"), body).await.0, 403, "another session's secret");
        let (code, out) = post(port, &path, &format!("{h}: {good}\r\n"), body).await;
        assert_eq!(code, 200, "{out}");
        assert!(out.contains("update_plan"), "{out}");
        // The config a turn gets carries it.
        let cfg: Value = serde_json::from_str(&app.mcp_config_for_test(&key)).unwrap();
        assert_eq!(cfg["mcpServers"]["app"]["headers"][h], good);
    }
}
