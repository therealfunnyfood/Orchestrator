//! Orchestrator: a desktop app where an orchestrator agent runs parallel
//! Claude Code threads, each viewable and chattable, at a permission level you choose.
//!
//!   orchestrator.exe                     native window (closing it keeps running in the tray)
//!   orchestrator.exe --serve [--port N]  server only; prints the URL (for a browser)
//!   orchestrator.exe --uninstall         remove it (what Windows' Installed apps runs)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod budget;
mod business;
mod capacity;
mod claude_projects;
mod config;
mod digest;
mod events;
mod inbox;
mod lines;
mod mcp;
mod needs;
mod migrate;
mod notify;
mod phone;
mod queue;
mod runner;
mod schedule;
mod setup;
mod store;
mod uninstall;
mod updates;
mod vault;
mod web;

use rand::Rng;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

/// Requests from the server side (and the tray) to the window event loop.
enum UiEvent {
    /// Flash the taskbar when something finishes or needs an answer.
    Attention,
    /// Open (or focus) a conversation in its own window.
    PopOut { key: String, title: String },
    /// Open (or focus) a pane (Queue or Needs you) in its own window.
    PopPane(String),
    /// Bring the main window back (tray click, or a second launch of the app).
    Show,
    /// Close everything (e.g. so the installer can update the app).
    Quit,
    Tray(tray_icon::TrayIconEvent),
    Menu(tray_icon::menu::MenuEvent),
    /// Queue items waiting for approval (all workspaces): shown on the tray icon.
    Badge(usize),
    /// Ctrl+Alt+I: save the clipboard to the business inbox.
    Hotkey(global_hotkey::GlobalHotKeyEvent),
}

static PROXY: OnceLock<Mutex<tao::event_loop::EventLoopProxy<UiEvent>>> = OnceLock::new();

pub fn set_badge(n: usize) {
    if let Some(p) = PROXY.get() {
        let _ = p.lock().unwrap().send_event(UiEvent::Badge(n));
    }
}

/// IPC ops that act as the user, spend, or can't be undone (see handle_ipc). mcp.rs tests
/// check web.rs never does these.
const USER_OPS: [&str; 13] = ["send", "edit", "answer", "thread_create", "permission", "settings_user",
    "mix", "configure", "orch_reset", "thread_delete", "queue_msg", "update_install", "resolve"];

async fn user_op(app: &Arc<app::App>, msg: &serde_json::Value) -> Result<serde_json::Value, String> {
    use serde_json::{json, Value};
    let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match s("op").as_str() {
        "send" => {
            let (target, text) = (s("thread"), s("text").trim().to_string());
            if text.is_empty() {
                return Err("empty message".into());
            }
            let attach: Vec<String> = msg.get("attachments").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
            // Default: straight into a running turn. "queue": true waits for it to finish.
            let now = !msg.get("queue").and_then(Value::as_bool).unwrap_or(false);
            let delivery = if target.starts_with(app::ORCHESTRATOR) {
                app.submit_orchestrator(&target, &text, attach, now)
            } else {
                app.send_to_thread(&target, &text, "you", attach, now)?
            };
            Ok(json!({"delivery": delivery, "queued": delivery == "queued"}))
        }
        "edit" => {
            let ts = msg.get("ts").and_then(Value::as_f64).ok_or("ts is required")?;
            let text = s("text").trim().to_string();
            if text.is_empty() {
                return Err("empty message".into());
            }
            app.edit_message(&s("thread"), ts, &text)?;
            Ok(json!({}))
        }
        "answer" => {
            app.answer(&s("id"), msg.get("answers").cloned().unwrap_or(json!([])))?;
            Ok(json!({}))
        }
        "thread_create" => {
            let name = s("name").trim().to_string();
            let opt = |k: &str| msg.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
            let row = app.create_thread(&app.workspace(), &name, opt("cwd").as_deref(), opt("backend").as_deref(), opt("model").as_deref(), opt("effort").as_deref())?;
            if let Some(task) = opt("task").map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
                app.send_to_thread(&name, &task, "you", vec![], false)?;
            }
            Ok(serde_json::to_value(row).unwrap_or(Value::Null))
        }
        "permission" => {
            let level = s("level");
            let thread = s("thread");
            if msg.get("default").and_then(Value::as_bool) == Some(true) {
                app.update_settings(&json!({"permission_default": level}))?;
            }
            if !thread.is_empty() && !thread.starts_with(app::ORCHESTRATOR) {
                app.set_thread_permission(&thread, &level)?;
            } else {
                app.set_project_permission(&level)?;
            }
            Ok(json!({"permission": app.project_permission()}))
        }
        // Settings that are gates or secrets (app::USER_ONLY_SETTINGS); the rest go over HTTP.
        "settings_user" => {
            let patch = msg.get("patch").cloned().unwrap_or(json!({}));
            app.update_settings(&patch)?;
            Ok(app.settings_json())
        }
        // This project's plan/API mix (0 = all plan, 10 = all API). default: also new projects'.
        "mix" => {
            let v = msg.get("value").and_then(Value::as_u64).ok_or("value 0-10 is required")?.min(10);
            if msg.get("default").and_then(Value::as_bool) == Some(true) {
                app.update_settings(&json!({"api_mix": v}))?;
            }
            app.set_mix(v as u8);
            Ok(json!({"mix": app.mix()}))
        }
        // Model, effort and backend (plan / API) of a thread or the orchestrator.
        "configure" => {
            let key = s("key");
            let opt = |k: &str| msg.get(k).and_then(Value::as_str).map(str::to_string);
            let (b, m, e) = (opt("backend"), opt("model"), opt("effort"));
            if key.starts_with(app::ORCHESTRATOR) {
                app.configure_orchestrator(b.as_deref(), m.as_deref(), e.as_deref())?;
            } else {
                app.configure_thread(&key, b.as_deref(), m.as_deref(), e.as_deref())?;
            }
            Ok(json!({}))
        }
        "resolve" => {
            let resolved = msg.get("resolved").and_then(Value::as_bool).unwrap_or(true);
            app.set_resolved(&s("name"), resolved, &s("note"), "you")?;
            Ok(json!({}))
        }
        "orch_reset" => {
            app.reset_orchestrator();
            Ok(app.orch_json())
        }
        "thread_delete" => {
            app.delete_thread(&s("name"))?;
            Ok(json!({}))
        }
        // Queued messages: remove one, run one now, or clear them all.
        "queue_msg" => {
            let (key, id) = (s("thread"), s("id"));
            let ok = match s("action").as_str() {
                "remove" => app.remove_queued(&key, &id),
                "now" => app.send_now(&key, &id),
                "clear" => { app.clear_queue(&key); true }
                other => return Err(format!("unknown action {other}")),
            };
            Ok(json!({"ok": ok}))
        }
        // Download the newer release, verify it, start its installer and quit so it can update.
        "update_install" => {
            let update = app.update.lock().unwrap().clone().ok_or("no update available")?;
            tokio::task::spawn_blocking(move || updates::download_and_launch(&update)).await.map_err(|e| e.to_string())??;
            // Threads waiting out a usage limit continue after the update's restart.
            app.save_resumes_for_update();
            // Give the reply time to reach the page, then quit (stopping threads cleanly).
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                quit_app();
            });
            Ok(json!({}))
        }
        other => Err(format!("unknown op {other}")),
    }
}

/// Messages the desktop window's page sends to the app over the webview's private IPC
/// channel. Only the user's clicks get here: agents can reach the app's HTTP API (they can
/// read its token), but not this channel. So approval decisions come only this way.
fn handle_ipc(app: &Arc<app::App>, body: &str) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(body) else { return };
    let s = |k: &str| msg.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let app = app.clone();
    // The user's own words and gates (send, edit, answer, new thread with a task, permission
    // levels, the effort cap and the API key): never over HTTP, where an agent could act as
    // the user. The page awaits the reply ({"kind": "ipc_reply", "req": ...}).
    if USER_OPS.contains(&s("op").as_str()) {
        rt().spawn(async move {
            let r = user_op(&app, &msg).await;
            let req = msg.get("req").cloned().unwrap_or(serde_json::Value::Null);
            if req.is_null() {
                if let Err(e) = r {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            } else {
                app.hub.broadcast(match r {
                    Ok(v) => serde_json::json!({"kind": "ipc_reply", "req": req, "ok": true, "result": v}),
                    Err(e) => serde_json::json!({"kind": "ipc_reply", "req": req, "ok": false, "error": e}),
                });
            }
        });
        return;
    }
    match s("op").as_str() {
        "queue_decide" => {
            let (id, action, reason) = (s("id"), s("action"), s("reason"));
            let payload = msg.get("payload").and_then(|v| v.as_str()).map(str::to_string);
            let force = msg.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let seen = s("hash");
            rt().spawn(async move {
                if let Err(e) = app.queue_decide(&id, &action, &reason, payload.as_deref(), force, "window", Some(&seen)) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "copy" => {
            let text = s("text");
            std::thread::spawn(move || {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(text);
                }
            });
        }
        "phone_enroll_begin" => {
            let bundle = s("bundle");
            rt().spawn(async move {
                if let Err(e) = app.phone_enroll_begin(&bundle) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "phone_enroll_confirm" => {
            let (device, fp) = (s("device"), s("fingerprint"));
            rt().spawn(async move {
                match app.phone_enroll_confirm(&device, &fp) {
                    Ok(()) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": "phone enrolled"})),
                    Err(e) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e})),
                }
            });
        }
        "phone_enroll_cancel" => {
            rt().spawn(async move { app.phone_enroll_cancel() });
        }
        "phone_revoke" => {
            let device = s("device");
            rt().spawn(async move {
                if let Err(e) = app.phone_revoke(&device) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "set_phone_decide" => {
            let on = msg.get("on").and_then(|v| v.as_bool()).unwrap_or(false);
            rt().spawn(async move {
                if let Err(e) = app.set_phone_decide(on) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "line" => {
            let op = s("action");
            let v = msg.get("line").cloned().unwrap_or(serde_json::Value::Null);
            rt().spawn(async move {
                if let Err(e) = app.line_user(&op, &v) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "set_phone_feed" => {
            let on = msg.get("on").and_then(|v| v.as_bool()).unwrap_or(false);
            rt().spawn(async move {
                if let Err(e) = app.set_phone_feed(on) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "queue_done" => {
            let id = s("id");
            rt().spawn(async move {
                if let Err(e) = app.queue_user_done(&id) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "set_notify" => {
            let events: Vec<String> = msg.get("events").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let phone = s("phone");
            rt().spawn(async move {
                match app.set_notify(events, &phone) {
                    Ok(()) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": "notification settings saved"})),
                    Err(e) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e})),
                }
            });
        }
        "notify_test" => {
            rt().spawn(async move {
                let ws = app.workspace();
                // The test always goes out at once (even in busy mode or with digests on).
                app.notify_at(&ws, "test", crate::digest::Level::Critical, "Test notification", "If you see this, notifications work.");
            });
        }
        "busy_set" => {
            let on = msg.get("on").and_then(|v| v.as_bool()).unwrap_or(false);
            let minutes = msg.get("minutes").and_then(|v| v.as_f64()).unwrap_or(0.0).clamp(0.0, 24.0 * 60.0);
            rt().spawn(async move {
                let text = app.set_busy(on, minutes, "user");
                app.hub.broadcast(serde_json::json!({"kind": "toast", "text": text}));
            });
        }
        "set_digests" => {
            let times: Vec<String> = msg.get("times").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            rt().spawn(async move {
                match app.set_digests(times) {
                    Ok(()) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": "digest times saved"})),
                    Err(e) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e})),
                }
            });
        }
        "digest_now" => {
            rt().spawn(async move {
                let n = app.digest_now();
                let text = if n == 0 { "nothing was held".to_string() } else { format!("sent a digest of {n}") };
                app.hub.broadcast(serde_json::json!({"kind": "toast", "text": text}));
            });
        }
        "inbox_clipboard" => {
            rt().spawn(async move {
                if let Err(e) = app.inbox_from_clipboard() {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "set_lesson_scope" => {
            if let Err(e) = app.set_lesson_scope(&s("scope")) {
                app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
            }
        }
        "set_workspace_kind" => {
            let kind = s("kind");
            rt().spawn(async move {
                if let Err(e) = app.set_workspace_kind(&kind) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "vault_set" => {
            let (name, value, scope) = (s("name"), s("value"), s("scope"));
            rt().spawn(async move {
                match app.vault_set(&name, &value, &scope) {
                    Ok(()) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": format!("secret {name} saved")})),
                    Err(e) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e})),
                }
            });
        }
        "vault_remove" => {
            let name = s("name");
            rt().spawn(async move { app.vault_remove(&name) });
        }
        "schedule" => {
            let enabled = msg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let (op, id, time, days, message, thread) = (s("action"), s("id"), s("time"), s("days"), s("message"), s("thread"));
            rt().spawn(async move {
                if let Err(e) = app.schedule_user(&op, &id, &time, &days, &message, &thread, enabled) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "set_capacity" => {
            let pct = |k: &str, d: f64| msg.get(k).and_then(|v| v.as_f64()).map_or(d, |v| v / 100.0);
            let gates = crate::capacity::Gates { five_hour: pct("five_hour", 0.70), week: pct("week", 0.60), week_stop: pct("week_stop", 0.80), share: pct("share", 0.50) };
            let fallback = msg.get("fallback").and_then(|v| v.as_bool()).unwrap_or(false);
            let cap = msg.get("cap").and_then(|v| v.as_f64()).unwrap_or(0.0);
            rt().spawn(async move {
                match app.set_capacity_settings(gates, fallback, cap) {
                    Ok(()) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": "usage gates saved"})),
                    Err(e) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e})),
                }
            });
        }
        "set_budget" => {
            let n = |k: &str| msg.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let (day, week) = (n("day"), n("week"));
            rt().spawn(async move {
                if let Err(e) = app.set_budget(day, week) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "set_outbound_paused" => {
            let paused = msg.get("paused").and_then(|v| v.as_bool()).unwrap_or(true);
            rt().spawn(async move {
                if let Err(e) = app.set_outbound_paused(paused) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        "set_workspace_paused" => {
            let paused = msg.get("paused").and_then(|v| v.as_bool()).unwrap_or(true);
            rt().spawn(async move {
                if let Err(e) = app.set_workspace_paused(paused) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        // Needs you buttons: done, snooze, unsnooze, dismiss, pin, answer, step, stuck.
        "need" => {
            let (action, id, ws) = (s("action"), s("id"), s("ws"));
            rt().spawn(async move {
                if let Err(e) = app.need_user(&action, &id, Some(&ws), &msg) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        // Cross-workspace messages: allow once / always, deny, and the standing rules.
        // Only the user's clicks reach this: never add an HTTP route for these.
        "xws" => {
            let action = s("action");
            rt().spawn(async move {
                if let Err(e) = app.xws_user(&action, &msg) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
        }
        // "⧉ Pop out" on the Queue and Needs you panes: the new window loads the same page in
        // single-pane mode and talks to the app through this same handler.
        "pop_out_pane" => {
            if let Err(e) = pop_out_pane(&s("pane")) {
                app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
            }
        }
        "queue_approve_low_risk" => {
            let shown: Vec<(String, String)> = msg.get("items").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| {
                Some((x.get("id")?.as_str()?.to_string(), x.get("hash")?.as_str()?.to_string()))
            }).collect()).unwrap_or_default();
            rt().spawn(async move {
                match app.queue_approve_low_risk(&shown) {
                    Ok(n) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": format!("approved {n} low-risk item(s)")})),
                    Err(e) => app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e})),
                }
            });
        }
        _ => {}
    }
}

/// The tray icon, with a red dot when something waits for approval.
fn tray_icon_with_dot(dot: bool) -> Option<tray_icon::Icon> {
    let mut px = include_bytes!("../assets/icon64.rgba").to_vec();
    if dot {
        let (cx, cy, r) = (50.0f32, 14.0f32, 13.0f32);
        for y in 0..64 {
            for x in 0..64 {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                if d <= r {
                    let i = (y * 64 + x) * 4;
                    let edge = d > r - 2.0;
                    px[i..i + 4].copy_from_slice(if edge { &[255, 255, 255, 255] } else { &[220, 38, 38, 255] });
                }
            }
        }
    }
    tray_icon::Icon::from_rgba(px, 64, 64).ok()
}

pub fn attention() {
    if let Some(p) = PROXY.get() {
        let _ = p.lock().unwrap().send_event(UiEvent::Attention);
    }
}

/// Returns false when there's no native window (--serve mode); the page then
/// opens a browser window itself.
pub fn pop_out(key: &str, title: &str) -> bool {
    match PROXY.get() {
        Some(p) => p.lock().unwrap().send_event(UiEvent::PopOut { key: key.into(), title: title.into() }).is_ok(),
        None => false,
    }
}

/// The panes that can be popped out into their own window: (pane, window title).
pub const PANES: [(&str, &str); 2] = [("queue", "Queue"), ("needs", "Needs you")];

/// Open (or focus) a pane's own window. Only from the window's IPC channel.
pub fn pop_out_pane(pane: &str) -> Result<(), String> {
    if !PANES.iter().any(|(p, _)| *p == pane) {
        return Err(format!("no pane {pane}"));
    }
    match PROXY.get() {
        Some(p) => p.lock().unwrap().send_event(UiEvent::PopPane(pane.into())).map_err(|_| "the window is closing".to_string()),
        None => Err("pop-out windows need the desktop app".into()),
    }
}

/// Where each pane window was last (physical x, y, width, height), kept across runs.
fn pane_geometry_file(app: &app::App) -> std::path::PathBuf {
    app.paths.data_dir.join("pane-windows.json")
}

fn load_pane_geometry(app: &app::App) -> HashMap<String, (i32, i32, u32, u32)> {
    std::fs::read_to_string(pane_geometry_file(app)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_pane_geometry(app: &app::App, g: &HashMap<String, (i32, i32, u32, u32)>) {
    if let Ok(t) = serde_json::to_string(g) {
        let _ = std::fs::write(pane_geometry_file(app), t);
    }
}

/// Quit the app (window mode), or the process (--serve). Used before updating.
pub fn quit_app() {
    match PROXY.get() {
        Some(p) => {
            let _ = p.lock().unwrap().send_event(UiEvent::Quit);
        }
        None => std::process::exit(0),
    }
}

pub fn show_main() -> bool {
    match PROXY.get() {
        Some(p) => p.lock().unwrap().send_event(UiEvent::Show).is_ok(),
        None => false,
    }
}

/// If the app is already running, ask it to show its window and return true.
fn wake_existing(paths: &config::Paths) -> bool {
    use std::io::{Read, Write};
    let Ok(text) = std::fs::read_to_string(paths.data_dir.join("instance.json")) else { return false };
    let Ok(info) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let (Some(port), Some(token)) = (info["port"].as_u64(), info["token"].as_str()) else { return false };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port as u16));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(800)) else { return false };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let req = format!(
        "POST /api/show HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Orch-Token: {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200")
}

fn token() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..40).map(|_| CHARS[rng.random_range(0..CHARS.len())] as char).collect()
}

async fn start(desktop: bool, port: u16) -> (Arc<app::App>, String) {
    let paths = config::Paths::resolve();
    // Data stranded in a packaged app's redirected AppData (see migrate.rs).
    let rescued = migrate::rescue_packaged_data(&paths);
    let app = app::App::new(paths, token());
    app.restore_resumes_after_update();
    if let Some(note) = rescued {
        let key = app.orch_key();
        app.hub.emit(&key, serde_json::json!({"kind": "notice", "text": note}));
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("bind 127.0.0.1");
    let port = listener.local_addr().unwrap().port();
    app.port.store(port, Ordering::Relaxed);
    let router = web::router(app.clone(), desktop);
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    // Deadline watchdog for paid orders' capacity reservations (every 30 minutes).
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
            a.capacity_watchdog();
        }
    });
    // Scheduled routines of business workspaces (checked every 30 s).
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            a.schedule_tick();
            a.business_tick();
            a.needs_tick();
            a.digest_tick();
        }
    });
    // Update check: at startup, then twice a day.
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            if a.settings().check_updates {
                if let Ok(found) = tokio::task::spawn_blocking(updates::check).await {
                    *a.update.lock().unwrap() = found;
                    a.touch();
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(12 * 3600)).await;
        }
    });
    let url = format!("http://127.0.0.1:{port}/?t={}", app.token);
    (app, url)
}

/// One runtime for the whole process; 'static so the window's event loop can use it.
fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime"))
}

fn shutdown(app: &app::App) {
    app.stop_all();
    // Give stopped turns a moment to kill their process trees and save.
    rt().block_on(tokio::time::sleep(std::time::Duration::from_millis(600)));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--uninstall") {
        uninstall::run(&args);
        return;
    }

    if args.iter().any(|a| a == "--serve") {
        let port = args
            .iter()
            .position(|a| a == "--port")
            .and_then(|i| args.get(i + 1))
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let (_app, url) = rt().block_on(start(false, port));
        println!("Orchestrator running at {url}");
        rt().block_on(async { tokio::signal::ctrl_c().await.ok() });
        return;
    }

    let paths = config::Paths::resolve();
    if wake_existing(&paths) {
        return; // already running (maybe in the tray): its window is shown instead
    }
    let (app, url) = rt().block_on(start(true, 0));
    let instance = app.paths.data_dir.join("instance.json");
    let _ = std::fs::write(&instance, serde_json::json!({"port": app.port.load(Ordering::Relaxed), "token": app.token}).to_string());
    // orchestrator.exe --import <folder>: open on that Claude Code project and set it up.
    if let Some(path) = args.iter().position(|a| a == "--import").and_then(|i| args.get(i + 1)) {
        let a = app.clone();
        let path = path.clone();
        rt().spawn(async move {
            let _ = tokio::task::spawn_blocking(move || a.import_project(&path)).await;
        });
    }
    // --minimized: started at sign-in; stay in the tray until the user opens the window.
    run_windows(app, url, args.iter().any(|a| a == "--minimized"));
}

fn run_windows(app: Arc<app::App>, url: String, minimized: bool) {
    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopWindowTarget};
    use tao::window::{Icon, Window, WindowBuilder, WindowId};

    let event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    let _ = PROXY.set(Mutex::new(event_loop.create_proxy()));
    let proxy = Mutex::new(event_loop.create_proxy());
    tray_icon::TrayIconEvent::set_event_handler(Some(move |e| {
        let _ = proxy.lock().unwrap().send_event(UiEvent::Tray(e));
    }));
    let proxy = Mutex::new(event_loop.create_proxy());
    tray_icon::menu::MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.lock().unwrap().send_event(UiEvent::Menu(e));
    }));
    // Ctrl+Alt+I anywhere: save the clipboard to the business inbox. If another program
    // already owns the shortcut, the app runs without it.
    let hotkeys = global_hotkey::GlobalHotKeyManager::new().ok();
    if let Some(m) = &hotkeys {
        use global_hotkey::hotkey::{Code, HotKey, Modifiers};
        let _ = m.register(HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyI));
    }
    let proxy = Mutex::new(event_loop.create_proxy());
    global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |e| {
        let _ = proxy.lock().unwrap().send_event(UiEvent::Hotkey(e));
    }));

    let light = app.settings().theme == "light";
    let bg = if light { (255, 255, 255, 255) } else { (24, 24, 24, 255) };
    let icon = || Icon::from_rgba(include_bytes!("../assets/icon64.rgba").to_vec(), 64, 64).ok();
    let theme = Some(if light { tao::window::Theme::Light } else { tao::window::Theme::Dark });
    let mut context = wry::WebContext::new(Some(app.paths.data_dir.join("webview")));

    let ipc_app = app.clone();
    // `at`: a saved position and size (physical pixels), used only if it's on a screen.
    let make = move |target: &EventLoopWindowTarget<UiEvent>, context: &mut wry::WebContext, title: &str, url: &str, w: f64, h: f64,
                     at: Option<(i32, i32, u32, u32)>|
     -> (Window, wry::WebView) {
        let mut builder = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(w, h))
            .with_min_inner_size(LogicalSize::new(520.0, 400.0))
            .with_window_icon(icon())
            .with_theme(theme);
        if let Some((x, y, gw, gh)) = at {
            let on_screen = target.available_monitors().any(|m| {
                let (p, s) = (m.position(), m.size());
                x + 40 >= p.x && y + 10 >= p.y && x + 40 < p.x + s.width as i32 && y + 10 < p.y + s.height as i32
            });
            if on_screen && gw >= 300 && gh >= 200 {
                builder = builder.with_position(tao::dpi::PhysicalPosition::new(x, y)).with_inner_size(tao::dpi::PhysicalSize::new(gw, gh));
            }
        }
        let window = builder.build(target).expect("window");
        let ipc_app = ipc_app.clone();
        let webview = wry::WebViewBuilder::new_with_web_context(context)
            .with_url(url)
            .with_ipc_handler(move |req: wry::http::Request<String>| handle_ipc(&ipc_app, req.body()))
            .with_background_color(bg)
            .with_devtools(cfg!(debug_assertions))
            .build(&window)
            .expect("webview (is the WebView2 runtime installed?)");
        (window, webview)
    };

    let (main_window, main_view) = make(&event_loop, &mut context, "Orchestrator", &url, 1400.0, 900.0, None);
    let main_id = main_window.id();
    if minimized {
        main_window.set_visible(false);
    }

    // Tray icon: the app keeps running when the window is closed.
    use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
    let menu = Menu::new();
    let open_item = MenuItem::with_id("open", "Open Orchestrator", true, None);
    // Kill switch for every business workspace: approved items aren't handed out while on.
    let pause_item = CheckMenuItem::with_id("pause_outbound", "Pause outbound (all business workspaces)", true, app.outbound_paused_all(), None);
    let quit_item = MenuItem::with_id("quit", "Quit Orchestrator", true, None);
    let _ = menu.append_items(&[&open_item, &pause_item, &PredefinedMenuItem::separator(), &quit_item]);
    let tray_icon_img = tray_icon::Icon::from_rgba(include_bytes!("../assets/icon64.rgba").to_vec(), 64, 64).ok();
    let mut tray_builder = tray_icon::TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Orchestrator — click to open, right-click to quit");
    if let Some(i) = tray_icon_img {
        tray_builder = tray_builder.with_icon(i);
    }
    let tray = tray_builder.build().ok();
    let instance_file = app.paths.data_dir.join("instance.json");
    // Pop-out windows, and which conversation each one shows.
    let mut popouts: HashMap<WindowId, (Window, wry::WebView, String)> = HashMap::new();
    // Pane windows (Queue, Needs you), and which pane each one shows.
    let mut panes: HashMap<WindowId, (Window, wry::WebView, String)> = HashMap::new();
    let mut pane_geometry = load_pane_geometry(&app);
    let geometry_of = |w: &Window| -> Option<(i32, i32, u32, u32)> {
        let p = w.outer_position().ok()?;
        let s = w.inner_size();
        (!w.is_minimized()).then_some((p.x, p.y, s.width, s.height))
    };

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep_alive = (&main_view, &tray);
        let show = |w: &Window| {
            w.set_visible(true);
            w.set_minimized(false);
            w.set_focus();
        };
        let mut quit = false;
        match event {
            Event::UserEvent(UiEvent::Show) => show(&main_window),
            Event::UserEvent(UiEvent::Quit) => quit = true,
            Event::UserEvent(UiEvent::Tray(tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            })) => show(&main_window),
            Event::UserEvent(UiEvent::Tray(_)) => {}
            Event::UserEvent(UiEvent::Hotkey(e)) => {
                let _ = &hotkeys;
                if e.state == global_hotkey::HotKeyState::Pressed {
                    let a = app.clone();
                    rt().spawn(async move {
                        if let Err(err) = a.inbox_from_clipboard() {
                            a.hub.broadcast(serde_json::json!({"kind": "toast", "text": err}));
                        }
                    });
                }
            }
            Event::UserEvent(UiEvent::Badge(n)) => {
                if let Some(t) = &tray {
                    let _ = t.set_icon(tray_icon_with_dot(n > 0));
                    let tip = if n > 0 { format!("Orchestrator — {n} waiting for your approval") } else { "Orchestrator — click to open, right-click to quit".into() };
                    let _ = t.set_tooltip(Some(tip));
                }
            }
            Event::UserEvent(UiEvent::Menu(e)) => {
                if e.id == "open" {
                    show(&main_window);
                } else if e.id == "pause_outbound" {
                    let on = pause_item.is_checked();
                    let a = app.clone();
                    rt().spawn(async move { a.set_outbound_paused_all(on) });
                } else if e.id == "quit" {
                    quit = true;
                }
            }
            Event::UserEvent(UiEvent::Attention) => {
                let any_focused = main_window.is_focused() || popouts.values().any(|(w, _, _)| w.is_focused());
                if !any_focused {
                    main_window.request_user_attention(Some(tao::window::UserAttentionType::Informational));
                }
            }
            Event::UserEvent(UiEvent::PopOut { key, title }) => {
                if let Some((w, _, _)) = popouts.values().find(|(_, _, k)| *k == key) {
                    w.set_minimized(false);
                    w.set_focus();
                } else {
                    let pop_url = format!("{url}&view={key}");
                    let (w, v) = make(target, &mut context, &format!("{title} — Orchestrator"), &pop_url, 920.0, 860.0, None);
                    popouts.insert(w.id(), (w, v, key));
                }
            }
            Event::UserEvent(UiEvent::PopPane(pane)) => {
                if let Some((w, _, _)) = panes.values().find(|(_, _, p)| *p == pane) {
                    show(w);
                } else {
                    let title = PANES.iter().find(|(p, _)| *p == pane).map(|(_, t)| *t).unwrap_or("Pane");
                    let pane_url = format!("{url}&pane={pane}");
                    let at = pane_geometry.get(&pane).copied();
                    let (w, v) = make(target, &mut context, &format!("{title} — Orchestrator"), &pane_url, 760.0, 860.0, at);
                    panes.insert(w.id(), (w, v, pane));
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if window_id == main_id {
                    // Keep working in the background; quit from the tray menu. Pane windows
                    // belong to the main window, so they close with it.
                    main_window.set_visible(false);
                    for (w, _, pane) in panes.values() {
                        if let Some(g) = geometry_of(w) {
                            pane_geometry.insert(pane.clone(), g);
                        }
                    }
                    if !panes.is_empty() {
                        save_pane_geometry(&app, &pane_geometry);
                    }
                    panes.clear();
                } else if let Some((w, _, pane)) = panes.remove(&window_id) {
                    if let Some(g) = geometry_of(&w) {
                        pane_geometry.insert(pane, g);
                        save_pane_geometry(&app, &pane_geometry);
                    }
                } else {
                    popouts.remove(&window_id);
                }
            }
            _ => {}
        }
        if quit {
            for (w, _, pane) in panes.values() {
                if let Some(g) = geometry_of(w) {
                    pane_geometry.insert(pane.clone(), g);
                }
            }
            if !panes.is_empty() {
                save_pane_geometry(&app, &pane_geometry);
            }
            panes.clear();
            popouts.clear();
            let _ = std::fs::remove_file(&instance_file);
            shutdown(&app);
            *control_flow = ControlFlow::Exit;
        }
    });
}
