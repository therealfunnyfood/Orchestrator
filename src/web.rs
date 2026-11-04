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
use axum::routing::{get, patch, post};
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
        .route("/api/answer", post(answer))
        .route("/api/upload", post(upload))
        .route("/api/open", post(open_path))
        .route("/api/doc", post(read_doc))
        .route("/api/handoff", post(handoff))
        .route("/api/popout", post(popout))
        .route("/api/edit", post(edit))
        .route("/api/claude-projects", get(claude_projects))
        .route("/api/import", post(import))
        .route("/api/past-sessions", get(past_sessions))
        .route("/api/notes", post(notes))
        .route("/api/mix", post(set_mix))
        .route("/api/permission", post(set_permission))
        .route("/api/open-url", post(open_url))
        .route("/api/check-update", post(check_update))
        .route("/api/update/install", post(install_update))
        .route("/api/setup/status", get(setup_status))
        .route("/api/setup/install-cli", post(|| async { crate::setup::install_cli().map(|_| Json(json!({"ok": true}))).map_err(ApiError) }))
        .route("/api/setup/login", post(setup_login))
        .route("/api/setup/finish", post(setup_finish))
        .route("/api/efficiency", get(|State(a): State<Shared>| async move { Json(a.efficiency_json(7.0)) }))
        .route("/api/timing", get(|State(a): State<Shared>| async move { Json(a.timing_json(30.0)) }))
        .route("/api/tuning", post(tuning))
        .route("/api/show", post(|| async { Json(json!({"shown": crate::show_main()})) }))
        .route("/api/plan-usage/refresh", post(refresh_plan_usage))
        .route("/api/queue", post(queue))
        .route("/api/state", get(|State(a): State<Shared>| async move { Json(a.state_json()) }))
        .route("/api/all-threads", get(|State(a): State<Shared>| async move { Json(a.all_threads_json()) }))
        .route("/api/events/{key}", get(events))
        .route("/api/send", post(send))
        .route("/api/stop", post(stop))
        .route("/api/threads", post(create_thread))
        .route("/api/threads/{name}", patch(patch_thread).delete(delete_thread))
        .route("/api/orchestrator", post(patch_orchestrator))
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

async fn send(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let target = s(&b, "thread").unwrap_or("");
    let text = s(&b, "text").unwrap_or("").trim();
    if text.is_empty() {
        return Err(ApiError("empty message".into()));
    }
    let attach: Vec<String> = b
        .get("attachments")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    // Default: straight into a running turn. "queue": true waits for it to finish.
    let now = !b.get("queue").and_then(Value::as_bool).unwrap_or(false);
    let delivery = if target.starts_with(ORCHESTRATOR) {
        app.submit_orchestrator(text, attach, now)
    } else {
        app.send_to_thread(target, text, "you", attach, now)?
    };
    Ok(Json(json!({"ok": true, "delivery": delivery, "queued": delivery == "queued"})))
}

async fn stop(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    Ok(Json(json!({"stopped": app.stop(s(&b, "thread").unwrap_or(""))})))
}

async fn create_thread(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let name = s(&b, "name").unwrap_or("").trim().to_string();
    let row = app.create_thread(&name, s(&b, "cwd"), s(&b, "backend"), s(&b, "model"), s(&b, "effort"))?;
    if let Some(task) = s(&b, "task").map(str::trim).filter(|t| !t.is_empty()) {
        app.send_to_thread(&name, task, "you", vec![], false)?;
    }
    Ok(Json(serde_json::to_value(row).unwrap()))
}

async fn patch_thread(State(app): State<Shared>, Path(name): Path<String>, Json(b): Json<Value>) -> ApiResult {
    if let Some(r) = b.get("resolved").and_then(Value::as_bool) {
        app.set_resolved(&name, r, s(&b, "note").unwrap_or(""), "you")?;
    }
    app.configure_thread(&name, s(&b, "backend"), s(&b, "model"), s(&b, "effort"))?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_thread(State(app): State<Shared>, Path(name): Path<String>) -> ApiResult {
    app.delete_thread(&name)?;
    Ok(Json(json!({"ok": true})))
}

/// Hand a chat (orchestrator or thread) off to a new session.
async fn handoff(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let key = s(&b, "key").unwrap_or("").to_string();
    let delivery = app.handoff(&key)?;
    Ok(Json(json!({"ok": true, "delivery": delivery})))
}

async fn patch_orchestrator(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    if b.get("reset").and_then(Value::as_bool) == Some(true) {
        app.reset_orchestrator();
    }
    app.configure_orchestrator(s(&b, "backend"), s(&b, "model"), s(&b, "effort"))?;
    Ok(Json(app.orch_json()))
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
    app.update_settings(&b)?;
    Ok(Json(app.settings_json()))
}


async fn answer(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    app.answer(s(&b, "id").unwrap_or(""), b.get("answers").cloned().unwrap_or(json!([])))?;
    Ok(Json(json!({"ok": true})))
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

/// Edit or retry an earlier message (branches the conversation before it).
async fn edit(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let key = s(&b, "thread").unwrap_or("");
    let ts = b.get("ts").and_then(Value::as_f64).ok_or(ApiError("ts is required".into()))?;
    let text = s(&b, "text").unwrap_or("").trim();
    if text.is_empty() {
        return Err(ApiError("empty message".into()));
    }
    app.edit_message(key, ts, text)?;
    Ok(Json(json!({"ok": true})))
}

/// Manage queued messages: remove one, run one now, or clear them all.
async fn queue(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let key = s(&b, "thread").unwrap_or("");
    let id = s(&b, "id").unwrap_or("");
    let ok = match s(&b, "action").unwrap_or("") {
        "remove" => app.remove_queued(key, id),
        "now" => app.send_now(key, id),
        "clear" => { app.clear_queue(key); true }
        other => return Err(ApiError(format!("unknown action {other}"))),
    };
    Ok(Json(json!({"ok": ok})))
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
async fn import(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let path = s(&b, "path").unwrap_or("").to_string();
    let a = app.clone();
    let ws = tokio::task::spawn_blocking(move || a.import_project(&path)).await.map_err(|e| ApiError(e.to_string()))??;
    Ok(Json(json!({"workspace": ws})))
}

async fn past_sessions(State(app): State<Shared>) -> ApiResult {
    let a = app.clone();
    let list = tokio::task::spawn_blocking(move || a.past_sessions()).await.map_err(|e| ApiError(e.to_string()))?;
    Ok(Json(json!({"sessions": list.iter().map(|s| s.to_json()).collect::<Vec<_>>()})))
}

async fn notes(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    app.save_notes(s(&b, "brief"), s(&b, "rules"));
    Ok(Json(json!({"ok": true})))
}

/// Set this project's plan/API mix (0 = all plan, 10 = all API).
async fn set_mix(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let v = b.get("value").and_then(Value::as_u64).ok_or(ApiError("value 0-10 is required".into()))?;
    app.set_mix(v.min(10) as u8);
    Ok(Json(json!({"mix": app.mix()})))
}

async fn refresh_plan_usage(State(app): State<Shared>) -> ApiResult {
    Ok(Json(json!({"plan_usage": app.refresh_plan_usage().await?})))
}

async fn tuning(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    app.save_tuning(s(&b, "notes").unwrap_or(""));
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

/// Finish the wizard: mode is "plan", "api" or "both" and sets the starting plan/API mix.
async fn setup_finish(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let mix: u64 = match s(&b, "mode").unwrap_or("plan") {
        "api" => 10,
        "both" => b.get("mix").and_then(Value::as_u64).unwrap_or(3).min(10),
        _ => 0,
    };
    app.update_settings(&json!({"setup_done": true, "api_mix": mix}))?;
    app.set_mix(mix as u8);
    if let Some(level) = s(&b, "permission").filter(|l| !l.is_empty()) {
        app.update_settings(&json!({"permission_default": level}))?;
        app.set_project_permission(level)?;
    }
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

/// Set the project's permission level, or a thread's override ({thread, level}; level "" = project's).
async fn set_permission(State(app): State<Shared>, Json(b): Json<Value>) -> ApiResult {
    let level = s(&b, "level").unwrap_or("");
    match s(&b, "thread").filter(|t| !t.is_empty() && !t.starts_with(ORCHESTRATOR)) {
        Some(t) => app.set_thread_permission(t, level)?,
        None => app.set_project_permission(level)?,
    }
    Ok(Json(json!({"ok": true, "permission": app.project_permission()})))
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

/// Download the newer release, verify it, start its installer and quit so it can update.
async fn install_update(State(app): State<Shared>) -> ApiResult {
    let update = app.update.lock().unwrap().clone().ok_or(ApiError("no update available".into()))?;
    tokio::task::spawn_blocking(move || crate::updates::download_and_launch(&update))
        .await
        .map_err(|e| ApiError(e.to_string()))??;
    // Give the response time to reach the page, then quit (stopping threads cleanly).
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        crate::quit_app();
    });
    Ok(Json(json!({"ok": true})))
}
