//! Orchestrator: a desktop app where an orchestrator agent runs parallel
//! Claude Code threads, each viewable and chattable, at a permission level you choose.
//!
//!   orchestrator.exe                     native window (closing it keeps running in the tray)
//!   orchestrator.exe --serve [--port N]  server only; prints the URL (for a browser)
//!   orchestrator.exe --uninstall         remove it (what Windows' Installed apps runs)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod budget;
mod capacity;
mod claude_projects;
mod config;
mod events;
mod mcp;
mod migrate;
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
    /// Bring the main window back (tray click, or a second launch of the app).
    Show,
    /// Close everything (e.g. so the installer can update the app).
    Quit,
    Tray(tray_icon::TrayIconEvent),
    Menu(tray_icon::menu::MenuEvent),
    /// Queue items waiting for approval (all workspaces): shown on the tray icon.
    Badge(usize),
}

static PROXY: OnceLock<Mutex<tao::event_loop::EventLoopProxy<UiEvent>>> = OnceLock::new();

pub fn set_badge(n: usize) {
    if let Some(p) = PROXY.get() {
        let _ = p.lock().unwrap().send_event(UiEvent::Badge(n));
    }
}

/// Messages the desktop window's page sends to the app over the webview's private IPC
/// channel. Only the user's clicks get here: agents can reach the app's HTTP API (they can
/// read its token), but not this channel. So approval decisions come only this way.
fn handle_ipc(app: &Arc<app::App>, body: &str) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(body) else { return };
    let s = |k: &str| msg.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let app = app.clone();
    match s("op").as_str() {
        "queue_decide" => {
            let (id, action, reason) = (s("id"), s("action"), s("reason"));
            let payload = msg.get("payload").and_then(|v| v.as_str()).map(str::to_string);
            let force = msg.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            rt().spawn(async move {
                if let Err(e) = app.queue_decide(&id, &action, &reason, payload.as_deref(), force) {
                    app.hub.broadcast(serde_json::json!({"kind": "toast", "text": e}));
                }
            });
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
        "queue_approve_low_risk" => {
            rt().spawn(async move {
                match app.queue_approve_low_risk() {
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

    let light = app.settings().theme == "light";
    let bg = if light { (255, 255, 255, 255) } else { (24, 24, 24, 255) };
    let icon = || Icon::from_rgba(include_bytes!("../assets/icon64.rgba").to_vec(), 64, 64).ok();
    let theme = Some(if light { tao::window::Theme::Light } else { tao::window::Theme::Dark });
    let mut context = wry::WebContext::new(Some(app.paths.data_dir.join("webview")));

    let ipc_app = app.clone();
    let make = move |target: &EventLoopWindowTarget<UiEvent>, context: &mut wry::WebContext, title: &str, url: &str, w: f64, h: f64|
     -> (Window, wry::WebView) {
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(w, h))
            .with_min_inner_size(LogicalSize::new(520.0, 400.0))
            .with_window_icon(icon())
            .with_theme(theme)
            .build(target)
            .expect("window");
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

    let (main_window, main_view) = make(&event_loop, &mut context, "Orchestrator", &url, 1400.0, 900.0);
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
                    let (w, v) = make(target, &mut context, &format!("{title} — Orchestrator"), &pop_url, 920.0, 860.0);
                    popouts.insert(w.id(), (w, v, key));
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if window_id == main_id {
                    // Keep working in the background; quit from the tray menu.
                    main_window.set_visible(false);
                } else {
                    popouts.remove(&window_id);
                }
            }
            _ => {}
        }
        if quit {
            popouts.clear();
            let _ = std::fs::remove_file(&instance_file);
            shutdown(&app);
            *control_flow = ControlFlow::Exit;
        }
    });
}
