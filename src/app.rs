//! Core state: threads, the per-workspace orchestrator, and running turns.

use crate::config::{check_backend, check_effort, check_permission, Paths, Settings};
use crate::claude_projects::{self, SessionInfo};
use crate::events::Hub;
use crate::runner::{run_turn, user_line, TurnError, TurnSpec};
use crate::store::{now, same_path, EffRecord, Store, ThreadRow};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{oneshot, watch, Notify};

pub const ORCHESTRATOR: &str = "orchestrator";

const WORKER_APPEND: &str = "You are a worker thread managed by an orchestrator. Do the task you are \
given end to end without asking for permission. When you finish a turn, end with a short summary of \
what you did, what changed (files, commands), and anything left open. That summary is what the \
orchestrator reads.

Keep your orchestrator in the loop, the way a teammate would. Call message_orchestrator
(it doesn't end your turn) to:
- report a milestone on longer work, or something you found that affects other work;
- ask it a question or for a decision you can't make yourself, then carry on with what you can
  (its answer arrives mid-turn, between your steps; or end your turn with the question open);
- say you're blocked, or that a shared resource (a port, a device, a branch) is free again.
Keep messages short and purposeful: one or two sentences, not a running commentary.
The orchestrator may also message you mid-turn to redirect you or add information: follow it.
Never try to message other Claude sessions any other way.

Depending on the project's permission level, some actions may need the user's approval. If one is
denied, don't retry it the same way: adjust your approach, or explain what you need and why.

You have tools from the `app` MCP server, shown to the user in their window:
- update_plan: for work with several steps, keep a task list (send the whole list each time;
  exactly one item in_progress while working). The user sees it live.
- ask_user: when a decision genuinely belongs to the user, ask multiple-choice questions and
  wait for the answer. Don't ask about things you can decide or look up yourself.
- show_media: show the user an image, video, audio file or PDF (for example a screenshot or a
  chart you made) by its path.";

/// Sender of a handoff request; when that turn ends, the session is replaced by a new
/// one that starts from the handoff the agent wrote.
const HANDOFF: &str = "handoff";
const HANDOFF_PROMPT: &str = "[handoff] This conversation is about to continue in a fresh session that won't see anything above. Write the handoff it will start from. Cover: the goal and what the user asked for, in their words where it matters; what's done and how it was checked; what's in progress and the exact next steps; decisions made and why; files, commands, branches and links that matter; the user's preferences and constraints; open questions and anything risky. If you manage threads, list each one with its job and status. Be specific and complete, and leave out the story of how you got here. Don't take any other action: reply with the handoff only.";

const ORCH_APPEND: &str = r#"
You are the orchestrator. The user talks to you; you get work done by managing worker threads.
Each thread is a full Claude Code agent with its own working folder, and it keeps its own
conversation history. Your thread tools come from the "app" MCP server.

How to work:
- Break the request into independent pieces and give each piece its own thread (spawn_thread).
  Threads run in parallel. Reuse an existing thread with send_to_thread when the work continues
  something it already did.
- Threads work in the workspace folder by default. Never let two threads edit the same files at the
  same time: split work by file or area, or give a thread its own subfolder via cwd.
- Write each brief so a fresh agent can act on it alone: goal, files or area, constraints, and what
  "done" looks like.
- Choose the cheapest model and effort that will do each job well; the user wants usage kept as low
  as possible. See "Model and effort" below.
- Talk to your threads like a lead talks to a team, not just "spawn and wait":
  - When a thread reports, asks or finishes, respond: answer its question, approve or correct its
    approach, give it the next step with send_to_thread, or tell it it's done.
  - send_to_thread reaches a busy thread mid-turn, so redirect early when a thread is heading the
    wrong way, and pass on anything it needs (a decision from the user, another thread's result,
    a changed requirement) as soon as you have it.
  - Relay between threads: when one thread's result unblocks or affects another, tell that thread.
  - Check in with read_thread on long-running threads, and nudge one that seems stuck.
  - Check results before reporting them as done; send a thread back to fix gaps.
  - When you're done with a thread (its work is finished and checked, or you've handed its work to
    another thread), resolve it with resolve_thread and a short note. That keeps the user's sidebar
    tidy. Sending a resolved thread new work reopens it, so resolve freely.
- Use wait_for_threads when you have nothing to do until threads report; it returns early when a
  thread messages you or finishes.
- Do small lookups yourself with Read/Glob/Grep; delegate real work.
- Keep the user informed: say what you started, and summarize results when done.

Your "app" MCP server also gives you update_plan (keep a live task list the user sees),
ask_user (multiple-choice questions when a decision is genuinely the user's) and show_media
(show the user an image, video, audio file or PDF). Threads have these tools too.

# Model and effort
Pick per thread with spawn_thread / configure_thread:
- model: claude-haiku-5-5 for trivial or mechanical work (watching/polling, small doc or text edits,
  renames, simple lookups, summarizing); claude-sonnet-5-5 for routine coding, tests, reviews of
  small changes; claude-opus-5-5 (the default) for hard, ambiguous, cross-cutting or high-stakes work.
- effort: use medium almost always; low for simple work; high only when a task really needs it
  (genuinely hard, or a thread is clearly struggling at medium). xhigh/max cost far more of the user's
  plan: request them only when you're confident it's genuinely worth it, with effort_reason. The
  app then asks the user to approve (the call waits for the answer) and uses high if they decline.
- If a cheaper thread fails or does weak work, upgrade it with configure_thread and send it on,
  rather than starting over.

# Self-tuning
The app records every thread turn (model, effort, steps, time, API-rate cost, errors). After you read
a thread's result, give a verdict with rate_thread: right, too_weak (needed a stronger model/effort)
or too_strong (a cheaper one would have done). After a batch of work, or about once a day, call
efficiency_report and update save_tuning_notes with concrete rules you've learned for this project
(e.g. "docs/README edits: haiku, low"; "parser bugs in src/: opus, medium"). Follow your tuning notes.

# Timelines
Give the user realistic timelines, learned from this project's history:
- When you take on a request that will take more than a couple of minutes, tell the user how long
  you expect it to take as a range (e.g. "about 10-20 minutes"), and record it with set_estimate
  (low/high minutes for the whole request, until all of its threads are done, and a kind).
- Give every spawn_thread / send_to_thread that starts real work estimate_minutes (and
  estimate_max_minutes for a range) and a kind.
- Base estimates on "Timing history" below and timing_report: how long similar kinds of work really
  took here, and how far off your past estimates were. Don't guess optimistically: include time for
  reading code, testing, fixing and your own review. Parallel threads overlap; work that waits on
  another thread doesn't.
- Use short, reusable kinds so the history groups well: small-fix, bugfix, feature, refactor, tests,
  research, docs, review, setup, ops, or another short word when none fits.
- If the work turns out bigger or smaller than expected, tell the user and call set_estimate again
  with the new range.
- The app times everything and shows the user each estimate counting down. After a batch of work,
  look at timing_report and note lessons in your tuning notes (e.g. "bugfixes here take 2x my guess").

Whenever you mention a thread to the user, write it as a link: [thread-name](thread:thread-name).
The user can click it to open that thread beside your chat.

Earlier Claude Code conversations about this project are available through list_past_sessions and
read_past_session. When the user refers to earlier work, look there. To carry on that work, prefer a
handoff: spawn a fresh thread with a complete brief written from what you read. Use
resume_past_session only when the old conversation's detailed context really matters; it continues
a forked copy, so the original is never changed. save_project_notes keeps a project brief and rules
that are shown to you and every thread in this workspace.

The user can message you while you work; those messages arrive mid-turn, between your steps.
Read them and adjust what you're doing (they may redirect you or add information).

Threads message you with message_orchestrator (progress, questions, blockers), and when a thread
finishes a task you gave it, its final report comes to you automatically. These arrive as a
[messages from threads] block: mid-turn between your steps if you're working, in the result of
wait_for_threads, or as a new turn of their own if you're idle. Act on each one: reply to the
thread, move the work on, or tell the user. If nothing needs doing, say so in one line and stop.

The user can also open any thread and talk to it directly. Thread updates that arrived since the
user's last message are prepended to it in a [thread updates] block.
"#;

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !name.starts_with(ORCHESTRATOR)
}

pub fn orch_key(workspace: &str) -> String {
    // FNV-1a of the lowercased path: stable id per workspace folder.
    let mut h: u32 = 0x811c9dc5;
    for b in workspace.trim_end_matches(['\\', '/']).to_lowercase().bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{ORCHESTRATOR}-{h:08x}")
}

pub struct App {
    pub paths: Paths,
    pub settings: RwLock<Settings>,
    pub store: Mutex<Store>,
    pub hub: Hub,
    pub token: String,
    pub port: AtomicU16,
    /// Messages waiting to run, per key (thread name or orchestrator key).
    queues: Mutex<HashMap<String, VecDeque<Queued>>>,
    /// Keys whose worker is running (it drains the queue one turn at a time).
    workers: Mutex<HashSet<String>>,
    /// Stop switch for the turn currently running on each key.
    cancels: Mutex<HashMap<String, watch::Sender<bool>>>,
    /// Feeds messages into the turn currently running on each key.
    injectors: Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<String>>>,
    /// Orchestrators with a mid-turn message from the user waiting; it ends wait_for_threads.
    nudged: Mutex<HashSet<String>>,
    /// Orchestrators blocked in wait_for_threads (thread messages go into its result).
    waiting: Mutex<HashSet<String>>,
    /// Finished-turn notices handed to the orchestrator on its next turn.
    notices: Mutex<Vec<String>>,
    /// Mid-turn messages from threads to their orchestrator, by orchestrator key.
    inbox: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// ask_user questions waiting for an answer, by question id.
    questions: Mutex<HashMap<String, PendingQuestion>>,
    answered_ids: Mutex<std::collections::HashSet<String>>,
    /// When the running turn on each key started (for the UI's timer).
    started: Mutex<HashMap<String, f64>>,
    /// The orchestrator's time estimate for the task each key is working on (a thread's
    /// turn, or the orchestrator's whole request), and ones for messages not started yet.
    estimates: Mutex<HashMap<String, Estimate>>,
    pending_estimates: Mutex<HashMap<String, Estimate>>,
    changed: Notify,
    /// Cached index of Claude Code's saved conversations.
    pub sessions_index: claude_projects::Index,
    /// A newer release on GitHub, if one was found.
    pub update: Mutex<Option<Value>>,
}

impl App {
    pub fn new(paths: Paths, token: String) -> Arc<App> {
        let settings = Settings::load(&paths);
        let mut store = Store::load(paths.data_dir.join("state.json"));
        if store.workspace.is_empty() || !Path::new(&store.workspace).is_dir() {
            store.workspace = paths.default_workspace().to_string_lossy().into_owned();
        }
        let ws = store.workspace.clone();
        store.remember_workspace(&ws);
        store.save();
        let hub = Hub::new(paths.data_dir.join("transcripts"), paths.data_dir.join("media"));
        hub.mark_interrupted();
        Arc::new(App {
            hub,
            paths,
            settings: RwLock::new(settings),
            store: Mutex::new(store),
            token,
            port: AtomicU16::new(0),
            queues: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashSet::new()),
            cancels: Mutex::new(HashMap::new()),
            injectors: Mutex::new(HashMap::new()),
            nudged: Mutex::new(HashSet::new()),
            waiting: Mutex::new(HashSet::new()),
            notices: Mutex::new(Vec::new()),
            inbox: Mutex::new(HashMap::new()),
            questions: Mutex::new(HashMap::new()),
            answered_ids: Mutex::new(Default::default()),
            started: Mutex::new(HashMap::new()),
            estimates: Mutex::new(HashMap::new()),
            pending_estimates: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            sessions_index: claude_projects::Index::new(),
            update: Mutex::new(None),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    pub fn workspace(&self) -> String {
        self.store.lock().unwrap().workspace.clone()
    }

    pub fn orch_key(&self) -> String {
        orch_key(&self.workspace())
    }

    pub fn busy(&self, key: &str) -> bool {
        self.workers.lock().unwrap().contains(key)
    }

    fn running(&self, key: &str) -> bool {
        self.cancels.lock().unwrap().contains_key(key)
    }

    /// Tell windows and waiters that state changed.
    pub fn touch(&self) {
        self.hub.broadcast(json!({"kind": "state"}));
        self.changed.notify_waiters();
    }

    fn status_of(&self, key: &str, stored: &str) -> String {
        if self.running(key) {
            "running".into()
        } else if self.busy(key) {
            "queued".into()
        } else {
            stored.into()
        }
    }

    fn thread_json(&self, t: &ThreadRow) -> Value {
        let mut v = serde_json::to_value(t).unwrap();
        v["status"] = json!(self.status_of(&t.name, &t.status));
        v["busy"] = json!(self.busy(&t.name));
        self.add_live(&mut v, &t.name);
        v
    }

    /// Fields that only exist while the app runs: plan, questions, timer.
    fn add_live(&self, v: &mut Value, key: &str) {
        v["plan"] = self.store.lock().unwrap().plans.get(key).cloned().unwrap_or(json!([]));
        v["needs_input"] = json!(self.questions.lock().unwrap().values().any(|q| q.key == key));
        v["started_at"] = json!(self.started.lock().unwrap().get(key));
        v["estimate"] = match self.estimates.lock().unwrap().get(key) {
            Some(e) => json!({"low_s": e.low_s, "high_s": e.high_s, "kind": e.kind, "set_at": e.set_at, "task": e.task}),
            None => Value::Null,
        };
        let thread_perm = self.store.lock().unwrap().get(key).map(|t| t.permission.clone()).unwrap_or_default();
        v["permission_effective"] = json!(if thread_perm.is_empty() { self.project_permission() } else { thread_perm });
        let queue: Vec<Value> = self.queues.lock().unwrap().get(key).map(|q| {
            q.iter().map(|m| json!({"id": m.id, "text": m.text, "attachments": m.attach, "sender": m.sender, "edit": m.fork.is_some()})).collect()
        }).unwrap_or_default();
        v["queue"] = json!(queue);
    }

    pub fn orch_json(&self) -> Value {
        let s = self.settings();
        let ws = self.workspace();
        let key = orch_key(&ws);
        let row = self.store.lock().unwrap().orchestrators.get(&key).cloned().unwrap_or_default();
        let mut v = json!({
            "name": key, "status": self.status_of(&key, "idle"), "busy": self.busy(&key),
            "backend": s.orch_backend, "model": s.orch_model, "effort": s.orch_effort,
            "cost_usd": row.cost_usd, "turns": row.turns, "cwd": ws,
        });
        self.add_live(&mut v, &key);
        v
    }

    pub fn state_json(&self) -> Value {
        let s = self.settings();
        let (ws, recent, rows) = {
            let st = self.store.lock().unwrap();
            (st.workspace.clone(), st.recent_workspaces.clone(), st.in_workspace(&st.workspace))
        };
        let threads: Vec<Value> = rows.iter().map(|t| self.thread_json(t)).collect();
        let running = self.workers.lock().unwrap().len();
        let waiting = self.questions.lock().unwrap().len();
        json!({
            "workspace": ws, "recent_workspaces": recent, "orchestrator": self.orch_json(),
            "threads": threads, "running": running, "waiting": waiting,
            "project": self.store.lock().unwrap().notes(&ws).cloned().map(|n| json!({
                "brief": n.brief, "rules": n.rules, "tuning": n.tuning, "imported_at": n.imported_at, "updated_at": n.updated_at,
            })),
            "defaults": {
                "thread_backend": s.thread_backend, "thread_model": s.thread_model,
                "thread_effort": s.thread_effort, "api_key_set": !s.api_key.trim().is_empty(),
                "theme": s.theme,
                "api_mix": s.api_mix,
                "setup_done": s.setup_done,
            },
            "app": {"version": crate::updates::VERSION, "repo": crate::updates::REPO, "update": self.update.lock().unwrap().clone()},
            "usage": self.usage_json(),
            "permission": self.project_permission(),
            "plan_usage": self.store.lock().unwrap().plan_usage.clone(),
        })
    }

    pub fn all_threads_json(&self) -> Value {
        let rows = self.store.lock().unwrap().threads.clone();
        json!({"threads": rows.iter().map(|t| self.thread_json(t)).collect::<Vec<_>>(), "workspace": self.workspace()})
    }

    // ---- threads ------------------------------------------------------

    pub fn create_thread(
        &self, name: &str, cwd: Option<&str>, backend: Option<&str>, model: Option<&str>, effort: Option<&str>,
    ) -> Result<ThreadRow, String> {
        if !valid_name(name) {
            return Err("thread name: letters, digits, - and _ (max 48), not starting with 'orchestrator'".into());
        }
        let s = self.settings();
        let backend = backend.filter(|b| !b.is_empty()).unwrap_or(&s.thread_backend).to_string();
        check_backend(&backend)?;
        let effort = effort.map(str::to_string).unwrap_or(if s.thread_effort.is_empty() { "medium".into() } else { s.thread_effort.clone() });
        check_effort(&effort)?;
        let model = model.filter(|m| !m.trim().is_empty()).map(|m| m.trim().to_string()).unwrap_or(s.thread_model.clone());

        let mut st = self.store.lock().unwrap();
        if st.get(name).is_some() {
            return Err(format!("thread '{name}' already exists"));
        }
        let ws = PathBuf::from(&st.workspace);
        let dir = match cwd.map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) if Path::new(c).is_absolute() => PathBuf::from(c),
            Some(c) => ws.join(c),
            None => ws.clone(),
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        let t = now();
        let row = ThreadRow {
            name: name.into(),
            cwd: dir.to_string_lossy().into_owned(),
            workspace: st.workspace.clone(),
            backend,
            model,
            effort,
            created_at: t,
            updated_at: t,
            ..Default::default()
        };
        st.threads.push(row.clone());
        st.save();
        drop(st);
        self.touch();
        Ok(row)
    }

    pub fn configure_thread(&self, name: &str, backend: Option<&str>, model: Option<&str>, effort: Option<&str>) -> Result<(), String> {
        if let Some(b) = backend {
            check_backend(b)?;
        }
        if let Some(e) = effort {
            check_effort(e)?;
        }
        let mut st = self.store.lock().unwrap();
        let row = st.get_mut(name).ok_or(format!("no thread named '{name}'"))?;
        if let Some(b) = backend {
            row.backend = b.into();
        }
        if let Some(m) = model {
            row.model = m.trim().into();
        }
        if let Some(e) = effort {
            row.effort = e.into();
        }
        st.save();
        drop(st);
        self.touch();
        Ok(())
    }

    pub fn delete_thread(&self, name: &str) -> Result<(), String> {
        self.queues.lock().unwrap().remove(name);
        self.stop(name);
        let mut st = self.store.lock().unwrap();
        let before = st.threads.len();
        st.threads.retain(|t| t.name != name);
        if st.threads.len() == before {
            return Err(format!("no thread named '{name}'"));
        }
        st.plans.remove(name);
        st.save();
        drop(st);
        self.hub.remove(name);
        self.touch();
        Ok(())
    }

    pub fn stop(&self, key: &str) -> bool {
        self.questions.lock().unwrap().retain(|_, q| q.key != key);
        match self.cancels.lock().unwrap().get(key) {
            Some(tx) => tx.send(true).is_ok(),
            None => false,
        }
    }

    pub fn stop_all(&self) {
        self.queues.lock().unwrap().clear();
        for tx in self.cancels.lock().unwrap().values() {
            let _ = tx.send(true);
        }
    }

    // ---- the queue ----------------------------------------------------------

    /// Add a message to a conversation's queue and make sure its worker runs.
    /// Returns (message id, whether it had to wait behind other work).
    fn enqueue(self: &Arc<Self>, key: &str, mut msg: Queued, front: bool) -> (String, bool) {
        let waited = self.busy(key);
        msg.id = format!("m{}-{}", (now() * 1000.0) as u64, self.queues.lock().unwrap().values().map(|q| q.len()).sum::<usize>());
        let id = msg.id.clone();
        let start_worker = {
            let mut queues = self.queues.lock().unwrap();
            let q = queues.entry(key.to_string()).or_default();
            if front { q.push_front(msg) } else { q.push_back(msg) }
            self.workers.lock().unwrap().insert(key.to_string())
        };
        if start_worker {
            let (app, key) = (self.clone(), key.to_string());
            tokio::spawn(async move {
                loop {
                    let next = {
                        let mut queues = app.queues.lock().unwrap();
                        let next = queues.get_mut(&key).and_then(VecDeque::pop_front);
                        if next.is_none() {
                            app.workers.lock().unwrap().remove(&key);
                        }
                        next
                    };
                    let Some(msg) = next else { break };
                    app.touch();
                    if key.starts_with(ORCHESTRATOR) {
                        app.orchestrator_turn(&key, &msg).await;
                        // Thread messages that arrived after it last looked get their own turn.
                        if app.has_inbox(&key) && app.queues.lock().unwrap().get(&key).is_none_or(|q| q.is_empty()) {
                            if let Some(text) = app.drain_inbox(&key) {
                                app.queues.lock().unwrap().entry(key.clone()).or_default().push_back(
                                    Queued { id: format!("m{}", (now() * 1000.0) as u64), text, sender: "threads".into(), attach: vec![], fork: None });
                            }
                        }
                    } else {
                        app.thread_turn(&key, &msg).await;
                    }
                    app.touch();
                }
                app.finish_jobs();
                app.touch();
            });
        }
        self.touch();
        (id, waited)
    }

    pub fn remove_queued(&self, key: &str, id: &str) -> bool {
        let removed = self.queues.lock().unwrap().get_mut(key).map_or(false, |q| {
            let before = q.len();
            q.retain(|m| m.id != id);
            q.len() != before
        });
        self.touch();
        removed
    }

    /// Deliver a queued message now: into the turn in progress if it can take it,
    /// otherwise run it next by interrupting that turn.
    pub fn send_now(&self, key: &str, id: &str) -> bool {
        let queued = self.queues.lock().unwrap().get(key).and_then(|q| q.iter().find(|m| m.id == id).cloned());
        if let Some(m) = queued.filter(|m| m.fork.is_none()) {
            if self.inject(key, &m) {
                self.remove_queued(key, id);
                return true;
            }
        }
        let moved = {
            let mut queues = self.queues.lock().unwrap();
            let Some(q) = queues.get_mut(key) else { return false };
            match q.iter().position(|m| m.id == id) {
                Some(i) => {
                    let m = q.remove(i).unwrap();
                    q.push_front(m);
                    true
                }
                None => false,
            }
        };
        if moved {
            self.stop(key);
        }
        moved
    }

    pub fn clear_queue(&self, key: &str) {
        self.queues.lock().unwrap().remove(key);
        self.touch();
    }

    /// Add a message to the turn running on `key`; Claude Code reads it at its next
    /// step. False when nothing is running or the turn is already wrapping up.
    fn inject(&self, key: &str, msg: &Queued) -> bool {
        let Some(tx) = self.injectors.lock().unwrap().get(key).cloned() else { return false };
        let (prompt, images) = with_attachments(&msg.text, &msg.attach);
        if tx.send(user_line(&prompt, &images)).is_err() {
            return false;
        }
        self.hub.emit(key, json!({"kind": "user", "text": msg.text, "sender": msg.sender, "attachments": msg.attach, "injected": true}));
        if key.starts_with(ORCHESTRATOR) {
            self.nudged.lock().unwrap().insert(key.to_string());
        } else {
            let mut st = self.store.lock().unwrap();
            if let Some(r) = st.get_mut(key) {
                r.last_prompt = msg.text.clone();
            }
            st.save();
        }
        self.touch();
        true
    }

    /// Deliver a message: with `now`, into the turn in progress (like typing while
    /// Claude works); otherwise, or if no turn can take it, queued.
    /// Returns "injected", "queued" (waits behind other work) or "sent".
    fn deliver(self: &Arc<Self>, key: &str, msg: Queued, now: bool) -> &'static str {
        if now && self.inject(key, &msg) {
            return "injected";
        }
        if self.enqueue(key, msg, false).1 { "queued" } else { "sent" }
    }

    /// Send a message to a thread. `sender` is "you" or "orchestrator".
    pub fn send_to_thread(self: &Arc<Self>, name: &str, text: &str, sender: &str, attach: Vec<String>, now: bool) -> Result<&'static str, String> {
        if self.store.lock().unwrap().get(name).is_none() {
            return Err(format!("no thread named '{name}'"));
        }
        let msg = Queued { id: String::new(), text: text.into(), sender: sender.into(), attach, fork: None };
        Ok(self.deliver(name, msg, now))
    }

    /// Hand a conversation off to a new session: after the current turn (if any), the
    /// agent writes a handoff, then a fresh session starts from it in the same chat.
    pub fn handoff(self: &Arc<Self>, key: &str) -> Result<&'static str, String> {
        if !key.starts_with(ORCHESTRATOR) && self.store.lock().unwrap().get(key).is_none() {
            return Err(format!("no thread named '{key}'"));
        }
        if self.queues.lock().unwrap().get(key).is_some_and(|q| q.iter().any(|m| m.sender == HANDOFF)) {
            return Err("a handoff is already queued".into());
        }
        let msg = Queued { id: String::new(), text: HANDOFF_PROMPT.into(), sender: HANDOFF.into(), attach: vec![], fork: None };
        Ok(self.deliver(key, msg, false))
    }

    /// The handoff turn finished: forget the old session and start the new one from it.
    fn finish_handoff(self: &Arc<Self>, key: &str, handoff: &str) {
        {
            let mut st = self.store.lock().unwrap();
            if key.starts_with(ORCHESTRATOR) {
                let o = st.orchestrators.entry(key.into()).or_default();
                o.session_id = None;
                o.last_uuid = None;
            } else if let Some(row) = st.get_mut(key) {
                row.session_id = None;
                row.last_uuid = None;
                row.fork_pending = false;
            }
            st.save();
        }
        self.hub.emit(key, json!({"kind": "handoff", "text": "handed off: continuing in a new session"}));
        let text = format!(
            "[handoff from your previous session]
You're continuing work from an earlier session of this conversation, which wrote this handoff for you:

{handoff}

Pick up where it left off. Check the current state before relying on anything above that may have changed."
        );
        let msg = Queued { id: String::new(), text, sender: "handoff-resume".into(), attach: vec![], fork: None };
        self.enqueue(key, msg, true);
    }

    pub fn submit_orchestrator(self: &Arc<Self>, text: &str, attach: Vec<String>, now: bool) -> &'static str {
        let key = self.orch_key();
        let msg = Queued { id: String::new(), text: text.into(), sender: "you".into(), attach, fork: None };
        self.deliver(&key, msg, now)
    }

    /// Edit (or retry) an earlier message: the conversation branches from just
    /// before it, so everything after it is forgotten, and the new text runs.
    pub fn edit_message(self: &Arc<Self>, key: &str, ts: f64, text: &str) -> Result<(), String> {
        let original = self
            .hub
            .history(key, 100_000)
            .into_iter()
            .find(|e| e.get("kind").and_then(Value::as_str) == Some("user") && e.get("ts").and_then(Value::as_f64).is_some_and(|t| (t - ts).abs() < 1e-6))
            .ok_or("couldn't find that message")?;
        let last_handoff = self
            .hub
            .history(key, 100_000)
            .into_iter()
            .filter(|e| e.get("kind").and_then(Value::as_str) == Some("handoff"))
            .filter_map(|e| e.get("ts").and_then(Value::as_f64))
            .last();
        if last_handoff.is_some_and(|h| ts < h) {
            return Err("this message is from before a handoff to a new session, so it can't be edited".into());
        }
        if original.get("injected").and_then(Value::as_bool).unwrap_or(false) {
            return Err("messages sent mid-turn can't be edited; send a follow-up instead".into());
        }
        let resume_at = original.get("resume_at").and_then(Value::as_str).map(str::to_string);
        if resume_at.is_none() && original.get("resume_at").is_none() {
            return Err("this message is from before editing was supported, so it can't be edited".into());
        }
        let attach: Vec<String> = original
            .get("attachments")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let sender = original.get("sender").and_then(Value::as_str).unwrap_or("you").to_string();
        let msg = Queued { id: String::new(), text: text.into(), sender, attach, fork: Some(Fork { at_ts: ts, resume_at }) };
        self.enqueue(key, msg, true);
        self.stop(key);
        Ok(())
    }

    async fn thread_turn(self: &Arc<Self>, name: &str, msg: &Queued) {
        let (text, sender, attach) = (msg.text.as_str(), msg.sender.as_str(), &msg.attach);
        let reopened = {
            let mut st = self.store.lock().unwrap();
            let was = st.get(name).is_some_and(|r| r.resolved);
            if was && msg.sender != HANDOFF && msg.sender != "handoff-resume" {
                if let Some(r) = st.get_mut(name) {
                    r.resolved = false;
                    r.resolved_note.clear();
                }
                true
            } else {
                false
            }
        };
        if reopened {
            self.hub.emit(name, json!({"kind": "notice", "text": "reopened: it has new work"}));
        }
        let Some(row) = ({
            let mut st = self.store.lock().unwrap();
            let row = st.get_mut(name).map(|r| {
                r.status = "running".into();
                r.last_prompt = text.into();
                r.updated_at = now();
                r.clone()
            });
            st.save();
            row
        }) else {
            return; // deleted while queued
        };
        let (tx, mut rx) = watch::channel(false);
        self.cancels.lock().unwrap().insert(name.into(), tx);
        let t0 = now();
        self.started.lock().unwrap().insert(name.into(), t0);
        if msg.sender == "orchestrator" {
            if let Some(mut e) = self.pending_estimates.lock().unwrap().remove(name) {
                e.set_at = t0;
                self.estimates.lock().unwrap().insert(name.into(), e);
            }
        }
        let (resume, resume_at, start_point) = branch_point(row.session_id.clone(), row.last_uuid.clone(), msg);
        if let Some(f) = &msg.fork {
            self.hub.emit(name, json!({"kind": "fork", "at": f.at_ts}));
        }
        self.hub.emit(name, json!({"kind": "user", "text": text, "sender": sender, "attachments": attach, "resume_at": start_point}));
        self.touch();
        let (prompt, images) = with_attachments(text, attach);

        let prior_cost = self.store.lock().unwrap().session_cost(resume.as_deref());
        let spec = TurnSpec {
            prompt,
            cwd: PathBuf::from(&row.cwd),
            resume,
            resume_at,
            model: row.model.clone(),
            effort: row.effort.clone(),
            backend: self.resolve_backend(),
            append_system: self.worker_prompt(),
            tools: (msg.sender == HANDOFF).then(|| "Read,Glob,Grep".into()),
            mcp_config: Some(self.mcp_config(name)),
            long_tool_calls: true,
            images,
            fork_session: row.fork_pending && msg.fork.is_none(),
            permission_mode: self.permission_for(&row),
            inject: Some(self.open_injector(name)),
            prior_cost,
        };
        let settings = self.settings();
        let app = self.clone();
        let key = name.to_string();
        let mut known_sid = if msg.fork.is_some() || row.fork_pending { None } else { row.session_id.clone() };
        let last_uuid = Arc::new(Mutex::new(start_point.clone()));
        let lu = last_uuid.clone();
        let mut on_line = move |v: &Value| {
            app.observe_plan_usage(v);
            for ev in app.hub.translate(&key, v) {
                app.hub.emit(&key, ev);
            }
            if let Some(u) = transcript_uuid(v) {
                *lu.lock().unwrap() = Some(u);
            }
            if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                if known_sid.as_deref() != Some(sid) {
                    known_sid = Some(sid.to_string());
                    let mut st = app.store.lock().unwrap();
                    if let Some(r) = st.get_mut(&key) {
                        r.session_id = Some(sid.to_string());
                    }
                    st.save();
                }
            }
        };
        let result = run_turn(&settings, &spec, &mut rx, &mut on_line).await;
        self.injectors.lock().unwrap().remove(name);
        self.cancels.lock().unwrap().remove(name);
        self.started.lock().unwrap().remove(name);
        crate::attention();
        let last_uuid = last_uuid.lock().unwrap().clone();

        let notice = {
            let mut st = self.store.lock().unwrap();
            if let Some(row) = st.get_mut(name) {
                row.last_uuid = last_uuid;
            }
            let est = self.estimates.lock().unwrap().remove(name);
            if let Ok(r) = &result {
                st.record_turn_usage(r);
                let est_ref = est.as_ref();
                st.record_efficiency(EffRecord {
                    wall_s: now() - est_ref.map_or(t0, |e| e.set_at.min(t0)),
                    est_low_s: est_ref.map_or(0.0, |e| e.low_s),
                    est_high_s: est_ref.map_or(0.0, |e| e.high_s),
                    kind: est_ref.map_or(String::new(), |e| e.kind.clone()),
                    thread: name.to_string(),
                    task: msg.text.chars().take(160).collect(),
                    model: if row.model.is_empty() { "default".into() } else { row.model.clone() },
                    effort: if row.effort.is_empty() { "medium".into() } else { row.effort.clone() },
                    steps: r.steps,
                    duration_ms: r.duration_ms,
                    cost: r.cost_usd,
                    ok: !r.is_error,
                    ..Default::default()
                });
            }
            let notice = match (&result, st.get_mut(name)) {
                (_, None) => None,
                (Ok(r), Some(row)) => {
                    row.session_id = r.session_id.clone().or(row.session_id.take());
                    row.fork_pending = false;
                    let _ = &row.session_id;
                    row.status = if r.is_error { "error" } else { "idle" }.into();
                    row.last_result = r.text.clone();
                    row.turns += 1;
                    row.cost_usd += r.cost_usd;
                    row.updated_at = now();
                    let extra = if r.notes.is_empty() { String::new() } else { format!(" ({})", r.notes.join("; ")) };
                    self.hub.emit(name, json!({"kind": "notice", "text": format!("ran on {} via {}{extra}", r.backend_used, r.key_source)}));
                    Some(format!(
                        "thread {name} {} on {}{extra}",
                        if r.is_error { "errored" } else { "finished" },
                        r.backend_used
                    ))
                }
                (Err(TurnError::Stopped), Some(row)) => {
                    row.status = "stopped".into();
                    row.updated_at = now();
                    self.hub.emit(name, json!({"kind": "notice", "text": "stopped"}));
                    Some(format!("thread {name} was stopped"))
                }
                (Err(TurnError::Failed(e)), Some(row)) => {
                    row.status = "error".into();
                    row.last_result = format!("[failed] {e}");
                    row.updated_at = now();
                    self.hub.emit(name, json!({"kind": "notice", "text": format!("failed: {e}"), "is_error": true}));
                    Some(format!("thread {name} failed: {e}"))
                }
            };
            st.save();
            notice
        };
        if msg.sender == HANDOFF {
            let row = self.store.lock().unwrap().get(name).map(|r| (r.status.clone(), r.last_result.clone()));
            if let Some((_, text)) = row.filter(|(s, t)| s == "idle" && !t.trim().is_empty()) {
                self.finish_handoff(name, &text);
            }
            return; // not news for the orchestrator
        }
        let Some(n) = notice else { return };
        if msg.sender == "orchestrator" {
            // Its task: the orchestrator hears back right away, like a teammate reporting.
            let result = self.store.lock().unwrap().get(name).map(|r| r.last_result.clone()).unwrap_or_default();
            let report: String = result.chars().take(1500).collect();
            let more = if result.chars().count() > 1500 { "\n(cut short; read_thread for the rest)" } else { "" };
            let _ = self.post_to_orchestrator(name, &format!("{n}. Its final report:\n{report}{more}"));
        } else {
            self.notices.lock().unwrap().push(n);
        }
    }

    /// A thread's mid-turn message to its orchestrator. Delivered right away: into a
    /// new orchestrator turn if it's idle, otherwise into its current turn (it wakes
    /// wait_for_threads) or the start of its next one.
    pub fn message_orchestrator(self: &Arc<Self>, from: &str, text: &str) -> Result<(), String> {
        self.hub.emit(from, json!({"kind": "notice", "text": format!("→ messaged the orchestrator: {}", crate::events::clip(text))}));
        self.post_to_orchestrator(from, text)
    }

    /// Hand a thread's message or report to its orchestrator: into its turn if it's
    /// working, as a new turn if it's idle, else at the start of its next turn.
    fn post_to_orchestrator(self: &Arc<Self>, from: &str, text: &str) -> Result<(), String> {
        let ws = self.store.lock().unwrap().get(from).map(|t| t.workspace.clone()).ok_or(format!("no thread named '{from}'"))?;
        let key = orch_key(&ws);
        self.inbox.lock().unwrap().entry(key.clone()).or_default().push((from.to_string(), text.to_string()));
        self.touch();
        if !same_path(&ws, &self.workspace()) {
            return Ok(()); // another project: it reads them when the user switches back
        }
        if self.waiting.lock().unwrap().contains(&key) {
            // touch() above ends the wait; the messages are its result.
        } else if self.running(&key) {
            if let Some(text) = self.drain_inbox(&key) {
                let msg = Queued { id: String::new(), text: text.clone(), sender: "threads".into(), attach: vec![], fork: None };
                if !self.inject(&key, &msg) {
                    // The turn is wrapping up: the worker starts a turn for it next.
                    self.inbox.lock().unwrap().entry(key.clone()).or_default().insert(0, (String::new(), text));
                }
            }
        } else if !self.busy(&key) {
            self.flush_inbox(&key);
        }
        Ok(())
    }

    /// Take the orchestrator's pending thread messages, formatted for a prompt.
    pub fn drain_inbox(&self, key: &str) -> Option<String> {
        let msgs = self.inbox.lock().unwrap().remove(key).filter(|m| !m.is_empty())?;
        Some(
            msgs.iter()
                .map(|(from, text)| if from.is_empty() { text.clone() } else { format!("[from thread {from}]\n{text}") })
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    }

    fn has_inbox(&self, key: &str) -> bool {
        self.inbox.lock().unwrap().get(key).is_some_and(|m| !m.is_empty())
    }

    /// Start an orchestrator turn for waiting thread messages.
    fn flush_inbox(self: &Arc<Self>, key: &str) {
        if let Some(text) = self.drain_inbox(key) {
            let msg = Queued { id: String::new(), text, sender: "threads".into(), attach: vec![], fork: None };
            self.enqueue(key, msg, false);
        }
    }

    /// Whether the user messaged this orchestrator mid-turn since it last checked.
    pub fn take_nudge(&self, key: &str) -> bool {
        self.nudged.lock().unwrap().remove(key)
    }

    fn open_injector(&self, key: &str) -> tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.injectors.lock().unwrap().insert(key.to_string(), tx);
        tokio::sync::Mutex::new(rx)
    }

    /// Block until the named threads are idle, a thread messages the orchestrator,
    /// or the timeout passes.
    pub async fn wait_threads(&self, names: &[String], timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        let key = self.orch_key();
        self.waiting.lock().unwrap().insert(key.clone());
        loop {
            let notified = self.changed.notified();
            if !names.iter().any(|n| self.busy(n)) || self.has_inbox(&key) || self.nudged.lock().unwrap().contains(&key) {
                break;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        self.waiting.lock().unwrap().remove(&key);
    }

    // ---- orchestrator ---------------------------------------------------

    pub fn configure_orchestrator(&self, backend: Option<&str>, model: Option<&str>, effort: Option<&str>) -> Result<(), String> {
        if let Some(b) = backend {
            check_backend(b)?;
        }
        if let Some(e) = effort {
            check_effort(e)?;
        }
        let mut s = self.settings.write().unwrap();
        if let Some(b) = backend {
            s.orch_backend = b.into();
        }
        if let Some(m) = model {
            s.orch_model = m.trim().into();
        }
        if let Some(e) = effort {
            s.orch_effort = e.into();
        }
        let _ = s.save(&self.paths);
        drop(s);
        self.touch();
        Ok(())
    }

    pub fn reset_orchestrator(&self) {
        let key = self.orch_key();
        let mut st = self.store.lock().unwrap();
        let o = st.orchestrators.entry(key.clone()).or_default();
        o.session_id = None;
        o.last_uuid = None;
        st.plans.remove(&key);
        st.save();
        drop(st);
        self.hub.emit(&key, json!({"kind": "notice", "text": "new conversation"}));
        self.touch();
    }

    async fn orchestrator_turn(self: &Arc<Self>, key: &str, msg: &Queued) {
        let (tx, mut rx) = watch::channel(false);
        self.cancels.lock().unwrap().insert(key.into(), tx);
        self.started.lock().unwrap().insert(key.into(), now());
        let (ws, session, last) = {
            let st = self.store.lock().unwrap();
            let o = st.orchestrators.get(key);
            (st.workspace.clone(), o.and_then(|o| o.session_id.clone()), o.and_then(|o| o.last_uuid.clone()))
        };
        let (resume, resume_at, start_point) = branch_point(session, last, msg);
        if let Some(f) = &msg.fork {
            self.hub.emit(key, json!({"kind": "fork", "at": f.at_ts}));
        }
        self.hub.emit(key, json!({"kind": "user", "text": msg.text, "sender": msg.sender, "attachments": msg.attach, "resume_at": start_point}));
        self.touch();
        let (text, images) = with_attachments(&msg.text, &msg.attach);
        let text = text.as_str();

        let notices: Vec<String> = std::mem::take(&mut *self.notices.lock().unwrap());
        let text = match self.drain_inbox(key) {
            Some(inbox) => format!("[messages from threads]\n{inbox}\n\n{text}"),
            None => text.to_string(),
        };
        let text = text.as_str();
        let prompt = if notices.is_empty() {
            text.to_string()
        } else {
            format!("[thread updates]\n{}\n\n{text}", notices.join("\n"))
        };
        let s = self.settings();
        let prior_cost = self.store.lock().unwrap().session_cost(resume.as_deref());
        let spec = TurnSpec {
            prompt,
            cwd: PathBuf::from(&ws),
            resume,
            resume_at,
            model: s.orch_model.clone(),
            effort: s.orch_effort.clone(),
            backend: self.resolve_backend(),
            append_system: self.orchestrator_prompt(),
            tools: Some(if msg.sender == HANDOFF { "Read,Glob,Grep" } else { "Read,Glob,Grep,WebSearch,WebFetch" }.into()),
            mcp_config: Some(self.mcp_config(key)),
            long_tool_calls: true,
            images,
            fork_session: false,
            permission_mode: self.project_permission(),
            inject: Some(self.open_injector(key)),
            prior_cost,
        };
        let app = self.clone();
        let k = key.to_string();
        let last_uuid = Arc::new(Mutex::new(start_point.clone()));
        let lu = last_uuid.clone();
        let mut on_line = move |v: &Value| {
            app.observe_plan_usage(v);
            for ev in app.hub.translate(&k, v) {
                app.hub.emit(&k, ev);
            }
            if let Some(u) = transcript_uuid(v) {
                *lu.lock().unwrap() = Some(u);
            }
            if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                let mut st = app.store.lock().unwrap();
                let o = st.orchestrators.entry(k.clone()).or_default();
                if o.session_id.as_deref() != Some(sid) {
                    o.session_id = Some(sid.to_string());
                    st.save();
                }
            }
        };
        let result = run_turn(&s, &spec, &mut rx, &mut on_line).await;
        self.injectors.lock().unwrap().remove(key);
        self.nudged.lock().unwrap().remove(key);
        self.waiting.lock().unwrap().remove(key); // in case the turn ended mid-wait
        self.cancels.lock().unwrap().remove(key);
        self.started.lock().unwrap().remove(key);
        crate::attention();
        {
            let mut st = self.store.lock().unwrap();
            st.orchestrators.entry(key.into()).or_default().last_uuid = last_uuid.lock().unwrap().clone();
            st.save();
        }
        match result {
            Ok(r) => {
                let mut st = self.store.lock().unwrap();
                st.record_turn_usage(&r);
                let o = st.orchestrators.entry(key.into()).or_default();
                o.cost_usd += r.cost_usd;
                o.turns += 1;
                st.save();
                let extra = if r.notes.is_empty() { String::new() } else { format!(" ({})", r.notes.join("; ")) };
                self.hub.emit(key, json!({"kind": "notice", "text": format!("ran on {} via {}{extra}", r.backend_used, r.key_source)}));
                drop(st);
                if msg.sender == HANDOFF && !r.is_error && !r.text.trim().is_empty() {
                    self.finish_handoff(key, &r.text);
                }
            }
            Err(TurnError::Stopped) => self.hub.emit(key, json!({"kind": "notice", "text": "stopped"})),
            Err(TurnError::Failed(e)) => {
                self.hub.emit(key, json!({"kind": "notice", "text": format!("failed: {e}"), "is_error": true}))
            }
        }
    }

    // ---- workspace & settings --------------------------------------------

    pub fn set_workspace(&self, path: &str, create: bool) -> Result<String, String> {
        let p = PathBuf::from(path.trim());
        if !p.is_absolute() {
            return Err("workspace must be an absolute path".into());
        }
        if !p.is_dir() {
            if !create {
                return Err(format!("folder does not exist: {}", p.display()));
            }
            std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
        }
        if self.busy(&self.orch_key()) {
            return Err("the orchestrator is mid-turn; stop it or wait before switching".into());
        }
        let p = std::fs::canonicalize(&p).unwrap_or(p);
        let mut s = p.to_string_lossy().into_owned();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            s = stripped.to_string();
        }
        let mut st = self.store.lock().unwrap();
        st.workspace = s.clone();
        st.remember_workspace(&s);
        st.save();
        drop(st);
        self.touch();
        Ok(s)
    }

    pub fn update_settings(&self, patch: &Value) -> Result<(), String> {
        let mut guard = self.settings.write().unwrap();
        let s = &mut *guard;
        let get = |k: &str| patch.get(k).and_then(Value::as_str).map(|v| v.trim().to_string());
        if let Some(v) = get("api_key").filter(|v| !v.is_empty()) {
            s.api_key = v;
        }
        if patch.get("clear_api_key").and_then(Value::as_bool) == Some(true) {
            s.api_key.clear();
        }
        if let Some(v) = get("api_workspace_id") {
            s.api_workspace_id = v;
        }
        for (k, slot) in [("orch_backend", &mut s.orch_backend), ("thread_backend", &mut s.thread_backend)] {
            if let Some(v) = patch.get(k).and_then(Value::as_str) {
                check_backend(v)?;
                *slot = v.into();
            }
        }
        for (k, slot) in [("orch_effort", &mut s.orch_effort), ("thread_effort", &mut s.thread_effort)] {
            if let Some(v) = patch.get(k).and_then(Value::as_str) {
                check_effort(v)?;
                *slot = v.into();
            }
        }
        if let Some(v) = get("orch_model") {
            s.orch_model = v;
        }
        if let Some(v) = get("thread_model") {
            s.thread_model = v;
        }
        if let Some(v) = get("cli_path") {
            s.cli_path = v;
        }
        if let Some(v) = get("theme") {
            s.theme = v;
        }
        if let Some(m) = patch.get("api_mix").and_then(Value::as_u64) {
            s.api_mix = m.min(10) as u8;
        }
        if let Some(v) = patch.get("setup_done").and_then(Value::as_bool) {
            s.setup_done = v;
        }
        if let Some(v) = patch.get("check_updates").and_then(Value::as_bool) {
            s.check_updates = v;
        }
        if let Some(v) = patch.get("permission_default").and_then(Value::as_str) {
            check_permission(v)?;
            s.permission_default = v.into();
        }
        if let Some(v) = patch.get("max_effort").and_then(Value::as_str) {
            check_effort(v)?;
            s.max_effort = v.into();
        }
        s.save(&self.paths).map_err(|e| e.to_string())?;
        drop(guard);
        self.touch();
        Ok(())
    }

    pub fn settings_json(&self) -> Value {
        let s = self.settings();
        let key = s.api_key.trim();
        let masked = if key.len() > 12 { format!("{}…{}", &key[..10], &key[key.len() - 4..]) } else if key.is_empty() { String::new() } else { "set".into() };
        json!({
            "api_key_set": !key.is_empty(), "api_key_masked": masked,
            "api_workspace_id": s.api_workspace_id,
            "orch_backend": s.orch_backend, "thread_backend": s.thread_backend,
            "orch_model": s.orch_model, "thread_model": s.thread_model,
            "orch_effort": s.orch_effort, "thread_effort": s.thread_effort,
            "cli_path": s.cli_path, "cli_resolved": s.claude_exe().to_string_lossy(),
            "theme": s.theme,
            "api_mix": s.api_mix,
            "max_effort": s.max_effort,
            "check_updates": s.check_updates,
            "permission_default": s.permission_default,
            "usage": self.usage_json(),
            "permission": self.project_permission(),
            "plan_usage": self.store.lock().unwrap().plan_usage.clone(),
            "config_file": self.paths.settings_file().to_string_lossy(),
            "data_dir": self.paths.data_dir.to_string_lossy(),
        })
    }

    /// Run a one-word turn on each backend and report which login it used.
    pub async fn auth_check(&self) -> Value {
        let s = self.settings();
        let mut results = vec![];
        let mut backends = vec!["plan"];
        if !s.api_key.trim().is_empty() {
            backends.push("api");
        }
        for b in backends {
            let spec = TurnSpec {
                prompt: "ping".into(),
                cwd: PathBuf::from(self.workspace()),
                resume: None,
                model: "claude-haiku-5-5".into(),
                effort: "low".into(),
                backend: b.into(),
                append_system: "Reply with the single word OK.".into(),
                tools: Some(String::new()),
                mcp_config: None,
                long_tool_calls: false,
                images: vec![],
                resume_at: None,
                fork_session: false,
                permission_mode: String::new(),
                inject: None,
                prior_cost: None,
            };
            let (_tx, mut rx) = watch::channel(false);
            let mut sink = |_: &Value| {};
            let r = match run_turn(&s, &spec, &mut rx, &mut sink).await {
                Ok(r) => json!({"backend": b, "source": r.key_source, "ok": !r.is_error, "detail": if r.is_error { r.text } else { String::new() }}),
                Err(TurnError::Failed(e)) => json!({"backend": b, "source": "?", "ok": false, "detail": e}),
                Err(TurnError::Stopped) => json!({"backend": b, "source": "?", "ok": false, "detail": "stopped"}),
            };
            results.push(r);
        }
        json!({"results": results, "api_key_set": !s.api_key.trim().is_empty()})
    }

    /// MCP config pointing a session at its own endpoint on this app.
    fn mcp_config(&self, key: &str) -> String {
        json!({"mcpServers": {"app": {
            "type": "http",
            "url": format!("http://127.0.0.1:{}/mcp/{key}", self.port.load(Ordering::Relaxed)),
            "headers": {"X-Orch-Token": self.token},
        }}})
        .to_string()
    }

    pub fn set_plan(&self, key: &str, items: Value) {
        let mut st = self.store.lock().unwrap();
        st.plans.insert(key.into(), items.clone());
        st.save();
        drop(st);
        self.hub.emit(key, json!({"kind": "plan", "items": items}));
        self.touch();
    }

    /// Post multiple-choice questions to the user and wait for the answers.
    pub async fn ask_user(self: &Arc<Self>, key: &str, questions: Value) -> Result<Value, String> {
        self.ask_user_with(key, questions, json!({})).await
    }

    /// Like ask_user, with extra fields on the question event (e.g. a permission request).
    pub async fn ask_user_with(self: &Arc<Self>, key: &str, questions: Value, extra: Value) -> Result<Value, String> {
        let id = format!("q{}-{:04x}", (now() * 1000.0) as u64, rand::random::<u16>());
        let (tx, rx) = oneshot::channel();
        self.questions.lock().unwrap().insert(id.clone(), PendingQuestion { key: key.into(), tx });
        let mut ev = json!({"kind": "question", "id": id, "questions": questions});
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                ev[k] = v.clone();
            }
        }
        self.hub.emit(key, ev);
        self.touch();
        crate::attention();

        // If the turn is stopped, the CLI hangs up and this future is dropped.
        struct Cleanup(Arc<App>, String, String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let was_waiting = self.0.questions.lock().unwrap().remove(&self.1).is_some();
                let answered = self.0.answered_ids.lock().unwrap().contains(&self.1);
                if was_waiting || !answered {
                    self.0.hub.emit(&self.2, json!({"kind": "answer", "id": self.1, "cancelled": true}));
                    self.0.touch();
                }
            }
        }
        let _cleanup = Cleanup(self.clone(), id.clone(), key.to_string());
        rx.await.map_err(|_| "the question was cancelled".to_string())
    }

    pub fn answer(&self, id: &str, answers: Value) -> Result<(), String> {
        let q = self.questions.lock().unwrap().remove(id).ok_or("that question is no longer waiting")?;
        self.answered_ids.lock().unwrap().insert(id.to_string());
        self.hub.emit(&q.key, json!({"kind": "answer", "id": id, "answers": answers}));
        let _ = q.tx.send(answers);
        self.touch();
        Ok(())
    }

    // ---- projects imported from Claude Code ---------------------------------

    /// Remember the plan's usage limits when a turn reports them.
    pub fn observe_plan_usage(&self, v: &Value) {
        if v.get("type").and_then(Value::as_str) != Some("rate_limit_event") {
            return;
        }
        let Some(info) = v.get("rate_limit_info") else { return };
        if info.get("unifiedWindows").is_none() {
            return; // API-key turns don't carry plan windows
        }
        let mut info = info.clone();
        info["seen_at"] = json!(now());
        let mut st = self.store.lock().unwrap();
        st.plan_usage = Some(info);
        st.save();
        drop(st);
        self.touch();
    }

    /// Check the plan's usage right now with a tiny Haiku turn on the plan.
    pub async fn refresh_plan_usage(self: &Arc<Self>) -> Result<Value, String> {
        let s = self.settings();
        let spec = TurnSpec {
            prompt: "ok".into(),
            cwd: PathBuf::from(self.workspace()),
            resume: None,
            model: "claude-haiku-5-5".into(),
            effort: "low".into(),
            backend: "plan".into(),
            append_system: "Reply with the single word ok.".into(),
            tools: Some(String::new()),
            mcp_config: None,
            long_tool_calls: false,
            images: vec![],
            resume_at: None,
            fork_session: false,
            permission_mode: String::new(),
            inject: None,
            prior_cost: None,
        };
        let (_tx, mut rx) = watch::channel(false);
        let app = self.clone();
        let mut sink = move |v: &Value| app.observe_plan_usage(v);
        match run_turn(&s, &spec, &mut rx, &mut sink).await {
            Ok(_) => {}
            Err(TurnError::Failed(e)) => return Err(e),
            Err(TurnError::Stopped) => return Err("stopped".into()),
        }
        self.store.lock().unwrap().plan_usage.clone().ok_or("Claude Code didn't report plan usage (is the CLI logged in with a Claude plan?)".into())
    }

    /// This project's permission level.
    pub fn project_permission(&self) -> String {
        let default = self.settings().permission_default;
        self.store.lock().unwrap().permission(&default)
    }

    fn permission_for(&self, row: &ThreadRow) -> String {
        if row.permission.is_empty() { self.project_permission() } else { row.permission.clone() }
    }

    pub fn set_project_permission(&self, level: &str) -> Result<(), String> {
        check_permission(level)?;
        let mut st = self.store.lock().unwrap();
        st.set_permission(level);
        st.save();
        drop(st);
        self.touch();
        Ok(())
    }

    pub fn set_thread_permission(&self, name: &str, level: &str) -> Result<(), String> {
        if !level.is_empty() {
            check_permission(level)?;
        }
        let mut st = self.store.lock().unwrap();
        let row = st.get_mut(name).ok_or(format!("no thread named '{name}'"))?;
        row.permission = level.into();
        st.save();
        drop(st);
        self.touch();
        Ok(())
    }

    /// Claude Code asks before running a tool: show the user a card and wait.
    /// Returns the permission-prompt-tool answer ({"behavior": "allow" | "deny", ...}).
    pub async fn approve(self: &Arc<Self>, key: &str, tool: &str, input: Value) -> Value {
        let allow = |input: &Value| json!({"behavior": "allow", "updatedInput": input});
        // The app's own tools (task list, questions, thread management) never need a prompt.
        if tool.starts_with("mcp__app__") {
            return allow(&input);
        }
        if self.store.lock().unwrap().get(key).is_some_and(|t| t.always_allow.iter().any(|a| a == tool)) {
            return allow(&input);
        }
        let preview = crate::events::clip(&serde_json::to_string_pretty(&input).unwrap_or_default());
        let short = tool.trim_start_matches("mcp__");
        let questions = json!([{
            "header": "Permission",
            "question": format!("Allow {short}?"),
            "options": [
                {"label": "Allow", "description": "Just this once"},
                {"label": format!("Always allow {short}"), "description": "In this conversation, from now on"},
                {"label": "Deny", "description": "Type a reason below to tell the agent what to do instead"},
            ],
        }]);
        let extra = json!({"permission": {"tool": tool, "input": preview}});
        let Ok(answer) = self.ask_user_with(key, questions, extra).await else {
            return json!({"behavior": "deny", "message": "The permission request was cancelled."});
        };
        let selected: Vec<String> = answer.pointer("/0/selected").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let other = answer.pointer("/0/other").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if selected.iter().any(|s| s.starts_with("Always")) {
            let mut st = self.store.lock().unwrap();
            if let Some(t) = st.get_mut(key) {
                if !t.always_allow.iter().any(|a| a == tool) {
                    t.always_allow.push(tool.to_string());
                }
            }
            st.save();
            return allow(&input);
        }
        if selected.iter().any(|s| s == "Allow") {
            return allow(&input);
        }
        let message = if other.is_empty() { "The user denied this action.".to_string() } else { format!("The user denied this action: {other}") };
        json!({"behavior": "deny", "message": message})
    }

    /// This project's plan/API mix (0 = all plan, 10 = all API).
    pub fn mix(&self) -> u8 {
        let default = self.settings().api_mix;
        self.store.lock().unwrap().mix(default)
    }

    pub fn set_mix(&self, value: u8) {
        let mut st = self.store.lock().unwrap();
        st.set_mix(value);
        st.save();
        drop(st);
        self.touch();
    }

    /// Pick the backend for a turn from the project's plan/API mix. Every turn
    /// (threads and orchestrator) goes to whichever side is behind the target
    /// share, by API-rate cost over the last day. Returns plan, api, or auto
    /// (= plan, switching to api if the plan hits a usage limit).
    fn resolve_backend(&self) -> String {
        let s = self.settings();
        let mix = self.mix();
        if s.api_key.trim().is_empty() || mix == 0 {
            return "plan".into();
        }
        if mix >= 10 {
            return "api".into();
        }
        let target = mix as f64 / 10.0;
        let (plan, api) = self.store.lock().unwrap().usage_split();
        let share = if plan + api > 0.0 { api / (plan + api) } else { 0.0 };
        if share < target { "api".into() } else { "auto".into() }
    }

    fn usage_json(&self) -> Value {
        let (plan, api) = self.store.lock().unwrap().usage_split();
        let share = if plan + api > 0.0 { api / (plan + api) } else { 0.0 };
        json!({"plan_cost": plan, "api_cost": api, "api_share": share, "window_hours": 24, "mix": self.mix()})
    }

    fn orchestrator_prompt(&self) -> String {
        let mix = {
            let s = self.settings();
            let (plan, api) = self.store.lock().unwrap().usage_split();
            let share = if plan + api > 0.0 { api / (plan + api) * 100.0 } else { 0.0 };
            format!(
                "\n# Plan/API mix\nThe user has set this project to about {}/10 of the work on API credits (0 = all on \
their Claude plan, 10 = all API). Over the last 24h, {share:.0}% has run on API (${api:.2} API vs ${plan:.2} plan, at \
API rates). The app routes every turn (yours and every thread's) to meet this target, so you don't choose backends. \
Mention it only if the user asks about cost or usage.{}\n",
                self.mix(),
                if s.api_key.trim().is_empty() { " (No API key is saved, so everything runs on the plan.)" } else { "" }
            )
        };
        let cap = format!("\n# Effort cap\nAbove effort {} needs the user's approval (asked automatically when you request it).\n",
            { let c = self.settings().max_effort; if c.is_empty() { "high".to_string() } else { c } });
        let timing = self.timing_summary();
        let st = self.store.lock().unwrap();
        let mut p = ORCH_APPEND.to_string() + &mix + &cap + &timing;
        if let Some(n) = st.notes(&st.workspace) {
            if !n.brief.trim().is_empty() {
                p += &format!("\n# Project brief (saved with save_project_notes)\n{}\n", n.brief.trim());
            }
            if !n.rules.trim().is_empty() {
                p += &format!("\n# Project rules (every thread follows these)\n{}\n", n.rules.trim());
            }
            if !n.tuning.trim().is_empty() {
                p += &format!("\n# Your tuning notes (model/effort lessons for this project)\n{}\n", n.tuning.trim());
            }
        }
        p
    }

    fn worker_prompt(&self) -> String {
        let st = self.store.lock().unwrap();
        let mut p = WORKER_APPEND.to_string();
        if let Some(n) = st.notes(&st.workspace).filter(|n| !n.rules.trim().is_empty()) {
            p += &format!("\n\nRules for this project (never break these without asking the user via ask_user):\n{}", n.rules.trim());
        }
        p
    }

    pub fn save_tuning(&self, tuning: &str) {
        let mut st = self.store.lock().unwrap();
        let ws = st.workspace.clone();
        let n = st.notes_mut(&ws);
        n.tuning = tuning.trim().to_string();
        n.updated_at = now();
        st.save();
        drop(st);
        self.touch();
    }

    /// Attach the orchestrator's verdict to a thread's latest recorded turn.
    pub fn rate_thread(&self, name: &str, verdict: &str, note: &str) -> Result<(), String> {
        if !["right", "too_weak", "too_strong"].contains(&verdict) {
            return Err("verdict must be right, too_weak or too_strong".into());
        }
        let mut st = self.store.lock().unwrap();
        let rec = st.efficiency.iter_mut().rev().find(|r| r.thread == name).ok_or(format!("no recorded turns for {name}"))?;
        rec.verdict = verdict.into();
        rec.note = note.chars().take(300).collect();
        st.save();
        Ok(())
    }

    /// The orchestrator's estimate for the message it's about to send a thread. It applies
    /// to the turn that message starts, or to the running turn if it lands mid-turn.
    pub fn note_thread_estimate(&self, name: &str, est: Option<Estimate>) {
        if let Some(e) = est {
            self.pending_estimates.lock().unwrap().insert(name.into(), e);
        }
    }

    /// The message went into the turn already running: its estimate now covers that turn.
    pub fn estimate_landed_mid_turn(&self, name: &str) {
        if let Some(mut e) = self.pending_estimates.lock().unwrap().remove(name) {
            let prev = self.started.lock().unwrap().get(name).copied();
            e.set_at = prev.unwrap_or_else(now);
            self.estimates.lock().unwrap().insert(name.into(), e);
            self.touch();
        }
    }

    /// The orchestrator's estimate for the user's whole request. The clock runs until it
    /// and all of this project's threads are idle.
    pub fn set_job_estimate(&self, mut est: Estimate) {
        let key = self.orch_key();
        if let Some(prev) = self.estimates.lock().unwrap().get(&key) {
            est.set_at = prev.set_at; // a revised estimate for the same request
        } else if let Some(t) = self.started.lock().unwrap().get(&key) {
            est.set_at = *t; // the request started with this turn
        }
        self.estimates.lock().unwrap().insert(key, est);
        self.touch();
    }

    /// Record requests whose work is all done: the orchestrator and its threads are idle.
    fn finish_jobs(&self) {
        let keys: Vec<String> = self.estimates.lock().unwrap().keys().filter(|k| k.starts_with(ORCHESTRATOR)).cloned().collect();
        for key in keys {
            if self.busy(&key) {
                continue;
            }
            let threads: Vec<String> = {
                let st = self.store.lock().unwrap();
                st.threads.iter().filter(|t| orch_key(&t.workspace) == key).map(|t| t.name.clone()).collect()
            };
            if threads.iter().any(|t| self.busy(t)) {
                continue;
            }
            let Some(e) = self.estimates.lock().unwrap().remove(&key) else { continue };
            let wall = now() - e.set_at;
            let mut st = self.store.lock().unwrap();
            st.record_efficiency(EffRecord {
                thread: "orchestrator".into(),
                task: e.task.clone(),
                wall_s: wall,
                duration_ms: (wall * 1000.0) as u64,
                est_low_s: e.low_s,
                est_high_s: e.high_s,
                kind: e.kind.clone(),
                job: true,
                ok: true,
                ..Default::default()
            });
            st.save();
            drop(st);
            let range = if e.high_s > e.low_s { format!("{}–{}", fmt_minutes(e.low_s), fmt_minutes(e.high_s)) } else { fmt_minutes(e.low_s) };
            self.hub.emit(&key, json!({"kind": "notice", "text": format!("⏱ finished in {} (estimated {range})", fmt_minutes(wall))}));
        }
    }

    /// How long work has really taken in this project, and how good the orchestrator's
    /// estimates have been, so it can give realistic timelines.
    pub fn timing_json(&self, days: f64) -> Value {
        let st = self.store.lock().unwrap();
        let since = now() - days * 86400.0;
        let recs: Vec<&EffRecord> = st
            .efficiency
            .iter()
            .filter(|r| r.ts >= since && same_path(&r.workspace, &st.workspace) && r.ok && r.wall_s > 0.0)
            .collect();
        let stats = |rs: &[&EffRecord]| {
            let mut secs: Vec<f64> = rs.iter().map(|r| r.wall_s).collect();
            let mut ratios: Vec<f64> = rs.iter().filter(|r| r.est_high_s > 0.0).map(|r| r.wall_s / ((r.est_low_s + r.est_high_s) / 2.0)).collect();
            let est: Vec<&&EffRecord> = rs.iter().filter(|r| r.est_high_s > 0.0).collect();
            let within = est.iter().filter(|r| r.wall_s >= r.est_low_s * 0.9 && r.wall_s <= r.est_high_s * 1.1).count();
            let over = est.iter().filter(|r| r.wall_s > r.est_high_s * 1.1).count();
            json!({
                "count": rs.len(),
                "median_minutes": (median(&mut secs) / 6.0).round() / 10.0,
                "p80_minutes": (percentile(&mut secs, 0.8) / 6.0).round() / 10.0,
                "estimated": est.len(),
                "actual_over_estimate_median": if ratios.is_empty() { Value::Null } else { json!((median(&mut ratios) * 100.0).round() / 100.0) },
                "within_range": within, "took_longer": over, "took_shorter": est.len() - within - over,
            })
        };
        let mut groups: std::collections::BTreeMap<(bool, String), Vec<&EffRecord>> = Default::default();
        for r in &recs {
            let kind = if r.kind.is_empty() { "unlabeled".to_string() } else { r.kind.clone() };
            groups.entry((r.job, kind)).or_default().push(r);
        }
        let by_kind = |job: bool| -> Vec<Value> {
            groups.iter().filter(|((j, _), _)| *j == job).map(|((_, k), rs)| {
                let mut v = stats(rs);
                v["kind"] = json!(k);
                v
            }).collect()
        };
        let jobs: Vec<&EffRecord> = recs.iter().copied().filter(|r| r.job).collect();
        let turns: Vec<&EffRecord> = recs.iter().copied().filter(|r| !r.job).collect();
        let recent: Vec<Value> = recs.iter().rev().filter(|r| r.est_high_s > 0.0).take(15).map(|r| json!({
            "what": if r.job { "request" } else { "thread turn" }, "thread": r.thread, "kind": r.kind, "task": r.task,
            "estimated_minutes": format!("{}-{}", short_minutes(r.est_low_s), short_minutes(r.est_high_s)),
            "actual_minutes": (r.wall_s / 6.0).round() / 10.0, "model": r.model, "effort": r.effort,
        })).collect();
        json!({
            "days": days,
            "requests": {"overall": stats(&jobs), "by_kind": by_kind(true)},
            "thread_turns": {"overall": stats(&turns), "by_kind": by_kind(false)},
            "recent_estimates": recent,
        })
    }

    /// A few lines on timing history for the orchestrator's instructions.
    fn timing_summary(&self) -> String {
        let t = self.timing_json(60.0);
        let line = |label: &str, v: &Value| -> Option<String> {
            let n = v["count"].as_u64().unwrap_or(0);
            if n == 0 { return None }
            let mut s = format!("- {label}: {n} done, typically {} min (80% within {} min)", v["median_minutes"], v["p80_minutes"]);
            if let Some(r) = v["actual_over_estimate_median"].as_f64() {
                let e = v["estimated"].as_u64().unwrap_or(0);
                s += &format!("; of {e} estimated, {} in range, {} took longer, {} shorter; actual/estimate median {r:.2}x",
                    v["within_range"], v["took_longer"], v["took_shorter"]);
            }
            Some(s)
        };
        let mut out = vec![];
        for (section, label) in [("requests", "whole requests"), ("thread_turns", "thread tasks")] {
            if let Some(l) = line(label, &t[section]["overall"]) { out.push(l) }
            for k in t[section]["by_kind"].as_array().cloned().unwrap_or_default() {
                if let Some(l) = line(&format!("  {label}, {}", k["kind"].as_str().unwrap_or("")), &k) { out.push(l) }
            }
        }
        if out.is_empty() {
            "\n# Timing history\nNo timed work in this project yet. Estimate from the size of the work, and say the \
estimate is a first guess. Every estimate you give is checked against the real time, and the results show up here.\n".into()
        } else {
            format!("\n# Timing history (last 60 days, wall-clock)\n{}\nUse this to calibrate: if actual/estimate is above 1, \
your estimates have been too short, so scale them up by about that much; if below 1, scale them down.\n", out.join("\n"))
        }
    }

    /// Efficiency stats for this project: per model+effort, plus recent turns.
    pub fn efficiency_json(&self, days: f64) -> Value {
        let st = self.store.lock().unwrap();
        let since = now() - days * 86400.0;
        let recs: Vec<&EffRecord> = st.efficiency.iter().filter(|r| r.ts >= since && !r.job && same_path(&r.workspace, &st.workspace)).collect();
        let mut groups: std::collections::BTreeMap<(String, String), Vec<&EffRecord>> = Default::default();
        for r in &recs {
            groups.entry((r.model.clone(), r.effort.clone())).or_default().push(r);
        }
        let rows: Vec<Value> = groups.iter().map(|((m, e), rs)| {
            let n = rs.len() as f64;
            let count = |v: &str| rs.iter().filter(|r| r.verdict == v).count();
            json!({
                "model": m, "effort": e, "turns": rs.len(),
                "avg_cost": rs.iter().map(|r| r.cost).sum::<f64>() / n,
                "total_cost": rs.iter().map(|r| r.cost).sum::<f64>(),
                "avg_seconds": rs.iter().map(|r| r.duration_ms as f64).sum::<f64>() / n / 1000.0,
                "avg_steps": rs.iter().map(|r| r.steps as f64).sum::<f64>() / n,
                "errors": rs.iter().filter(|r| !r.ok).count(),
                "right": count("right"), "too_weak": count("too_weak"), "too_strong": count("too_strong"),
            })
        }).collect();
        let recent: Vec<Value> = recs.iter().rev().take(25).map(|r| json!({
            "ts": r.ts, "thread": r.thread, "task": r.task, "model": r.model, "effort": r.effort,
            "cost": r.cost, "seconds": r.duration_ms as f64 / 1000.0, "steps": r.steps, "ok": r.ok,
            "verdict": r.verdict, "note": r.note,
        })).collect();
        json!({"days": days, "groups": rows, "recent": recent, "total_cost": recs.iter().map(|r| r.cost).sum::<f64>(), "turns": recs.len()})
    }

    pub fn save_notes(&self, brief: Option<&str>, rules: Option<&str>) {
        let mut st = self.store.lock().unwrap();
        let ws = st.workspace.clone();
        let n = st.notes_mut(&ws);
        if let Some(b) = brief {
            n.brief = b.trim().to_string();
        }
        if let Some(r) = rules {
            n.rules = r.trim().to_string();
        }
        n.updated_at = now();
        st.save();
        drop(st);
        self.touch();
    }

    /// Claude Code conversations for the current workspace that didn't come from Orchestrator itself.
    pub fn past_sessions(&self) -> Vec<SessionInfo> {
        let (ws, known) = {
            let st = self.store.lock().unwrap();
            let mut known: HashSet<String> = st.threads.iter().filter_map(|t| t.session_id.clone()).collect();
            known.extend(st.orchestrators.values().filter_map(|o| o.session_id.clone()));
            (st.workspace.clone(), known)
        };
        self.sessions_index.sessions_for(&ws).into_iter().filter(|s| !known.contains(&s.id)).collect()
    }

    /// Continue a past Claude Code conversation as a thread (a forked copy).
    pub fn resume_past_session(
        self: &Arc<Self>, name: &str, session_id: &str, message: &str, backend: Option<&str>, model: Option<&str>, effort: Option<&str>,
    ) -> Result<ThreadRow, String> {
        let info = self.past_sessions().into_iter().find(|s| s.id == session_id).ok_or(format!("no past session {session_id}"))?;
        let cwd = if info.cwd.is_empty() { None } else { Some(info.cwd.as_str()) };
        let row = self.create_thread(name, cwd, backend, model, effort)?;
        let last = claude_projects::last_uuid(&info.path);
        {
            let mut st = self.store.lock().unwrap();
            if let Some(r) = st.get_mut(name) {
                r.session_id = Some(info.id.clone());
                r.fork_pending = true;
                r.last_uuid = last;
            }
            st.save();
        }
        for ev in claude_projects::to_events(&self.hub, name, &info.path, 400) {
            self.hub.emit(name, ev);
        }
        self.hub.emit(name, json!({"kind": "notice", "text": format!(
            "continued from Claude Code conversation “{}” — this thread works on a copy; the original is untouched", info.title)}));
        if !message.trim().is_empty() {
            self.send_to_thread(name, message, "orchestrator", vec![], false)?;
        }
        Ok(row)
    }

    /// Switch to a Claude Code project and have the orchestrator set it up.
    pub fn import_project(self: &Arc<Self>, path: &str) -> Result<String, String> {
        let ws = self.set_workspace(path, false)?;
        let count = self.past_sessions().len();
        {
            let mut st = self.store.lock().unwrap();
            st.notes_mut(&ws).imported_at = Some(now());
            st.save();
        }
        let key = self.orch_key();
        let memory = claude_projects::memory_dir(&ws);
        let memory_note = if memory.is_dir() {
            format!("Its Claude Code memory folder is {} (MEMORY.md is the index); it loads automatically for you and \
every thread working in this folder, and it holds many of the user's rules.", memory.display())
        } else {
            "It has no Claude Code memory folder.".to_string()
        };
        let prompt = format!(
            "[Project import] This project was just moved into Orchestrator from Claude Code: {ws}\n\
There are {count} earlier Claude Code conversations about it (list_past_sessions, read_past_session, past_user_messages). \
{memory_note}\n\n\
The user wants this to be hands-off, with two guarantees: every rule and instruction they gave carries over, and the \
work they were doing continues. Do this:\n\
1. Call update_plan with these steps and keep it current.\n\
2. Get to know the project: CLAUDE.md files, README, docs, the top-level layout, and every file in the memory folder. \
If the project has its own task tracker or coordination workflow (docs, scripts, a registry), learn it: it is the \
authority on what work is in progress, and every thread must follow it (claiming tasks, worktrees, review steps). \
For git status, recent commits, the tracker's state, or anything needing a shell, spawn one short-lived thread named \
'survey' (effort low) that only reports and changes nothing.\n\
3. Collect the rules: call past_user_messages (raise max_chars until you've covered every conversation) and note every \
standing instruction, preference, rule, prohibition and workflow the user gave (how to build, test, commit, hand off, \
what never to do, how they like replies). Lines marked [relayed from … session] or [agent] were written by other Claude \
sessions, not the user: use them to understand the process and the work in flight, but only the user's own words and \
the memory folder define rules. Combine them with the memory folder; where they conflict, the newer one wins.\n\
4. Find the work in progress: list_past_sessions, read the newest conversations (at least the 8 most recent) and any \
others that look unfinished, and work out what was being done, how far it got, and what was next.\n\
5. Call save_project_notes. rules: the complete list from step 3, written as direct instructions for a thread, grouped \
by topic, nothing dropped. brief: what the project is, how to build/test/run it, current state, decisions, and the \
unfinished work (with the conversation each came from).\n\
6. Continue the unfinished work without waiting: for each piece, prefer a handoff (spawn_thread with a complete brief \
written from what you read, including the relevant rules and where it left off); use resume_past_session only when the \
old conversation's detailed context really matters. Give independent work its own thread; never put two threads on \
the same files. Only use ask_user (once, multi_select) for items that are genuinely unclear or risky to restart.\n\
7. Finish with a short summary: what the project is, the rules you carried over (counts by topic), and links to every \
thread now working.\n\
Don't change project files yourself; threads do the work, following the rules."
        );
        let msg = Queued { id: String::new(), text: prompt, sender: "setup".into(), attach: vec![], fork: None };
        self.enqueue(&key, msg, false);
        Ok(ws)
    }

    /// This workspace's threads that aren't resolved.
    pub fn thread_names_here(&self) -> Vec<String> {
        let st = self.store.lock().unwrap();
        st.threads.iter().filter(|t| same_path(&t.workspace, &st.workspace) && !t.resolved).map(|t| t.name.clone()).collect()
    }

    /// Mark a thread resolved (done with) or reopen it. `by` is "you" or "orchestrator".
    pub fn set_resolved(&self, name: &str, resolved: bool, note: &str, by: &str) -> Result<(), String> {
        {
            let mut st = self.store.lock().unwrap();
            let row = st.get_mut(name).ok_or(format!("no thread named '{name}'"))?;
            if row.resolved == resolved {
                return Ok(());
            }
            row.resolved = resolved;
            row.resolved_at = if resolved { now() } else { 0.0 };
            row.resolved_note = if resolved { note.trim().chars().take(300).collect() } else { String::new() };
            st.save();
        }
        let who = if by == "orchestrator" { "the orchestrator" } else { "you" };
        let text = match (resolved, note.trim()) {
            (true, "") => format!("✓ resolved by {who}"),
            (true, n) => format!("✓ resolved by {who}: {n}"),
            (false, _) => format!("reopened by {who}"),
        };
        self.hub.emit(name, json!({"kind": "notice", "text": text}));
        self.touch();
        Ok(())
    }
}

pub struct PendingQuestion {
    pub key: String,
    tx: oneshot::Sender<Value>,
}

const IMAGE_TYPES: [(&str, &str); 5] =
    [("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp")];

/// Turn attached file paths into image blocks (images) plus a file list in the prompt.
fn with_attachments(text: &str, paths: &[String]) -> (String, Vec<(String, String)>) {
    use base64::Engine;
    let mut images = vec![];
    let mut files = vec![];
    for p in paths {
        let ext = Path::new(p).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let mime = IMAGE_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m);
        match (mime, std::fs::read(p)) {
            (Some(m), Ok(bytes)) if bytes.len() < 5_000_000 => {
                images.push((m.to_string(), base64::engine::general_purpose::STANDARD.encode(bytes)));
                files.push(format!("- {p} (image, included above)"));
            }
            _ => files.push(format!("- {p}")),
        }
    }
    if files.is_empty() {
        return (text.to_string(), images);
    }
    (format!("{text}\n\nAttached files:\n{}", files.join("\n")), images)
}

#[derive(Clone, Debug)]
pub struct Estimate {
    pub low_s: f64,
    pub high_s: f64,
    pub kind: String,
    pub task: String,
    pub set_at: f64,
}

impl Estimate {
    /// From minutes as given by the orchestrator; high defaults to low.
    pub fn from_minutes(low: f64, high: Option<f64>, kind: &str, task: &str) -> Option<Estimate> {
        if !(low > 0.0) {
            return None;
        }
        let high = high.filter(|h| *h >= low).unwrap_or(low);
        Some(Estimate {
            low_s: low * 60.0,
            high_s: high * 60.0,
            kind: kind_name(kind),
            task: task.chars().take(160).collect(),
            set_at: now(),
        })
    }
}

/// Normalize a kind of work to a short lowercase label.
fn kind_name(kind: &str) -> String {
    let k: String = kind.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).take(24).collect();
    if k.is_empty() { "other".into() } else { k }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 { 0.0 } else if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.is_empty() { return 0.0 }
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Minutes with one decimal under 10 (0.5, 2, 15).
fn short_minutes(s: f64) -> String {
    let m = s / 60.0;
    if m < 10.0 { format!("{:.1}", m).trim_end_matches(".0").to_string() } else { format!("{:.0}", m) }
}

pub fn fmt_minutes(s: f64) -> String {
    let m = s / 60.0;
    if m < 1.0 { "<1m".into() } else if m < 90.0 { format!("{:.0}m", m) } else { format!("{:.1}h", m / 60.0) }
}

#[derive(Clone)]
pub struct Queued {
    pub id: String,
    pub text: String,
    pub sender: String,
    pub attach: Vec<String>,
    /// Set for edits and retries: branch the conversation before this message.
    pub fork: Option<Fork>,
}

#[derive(Clone)]
pub struct Fork {
    /// Timestamp of the user message being replaced (the UI hides it and everything after).
    pub at_ts: f64,
    /// Transcript entry the replaced message started from; None = it was the first message.
    pub resume_at: Option<String>,
}

/// Where a turn starts: (session to resume, entry to branch at, start point to
/// record on the user message so it can be edited later).
fn branch_point(session: Option<String>, last: Option<String>, msg: &Queued) -> (Option<String>, Option<String>, Option<String>) {
    match &msg.fork {
        None => (session, None, last),
        Some(Fork { resume_at: None, .. }) => (None, None, None), // editing the first message: fresh session
        Some(Fork { resume_at: Some(at), .. }) => (session, Some(at.clone()), Some(at.clone())),
    }
}

/// UUID of a main-conversation transcript entry (not subagent traffic or stream events).
fn transcript_uuid(v: &Value) -> Option<String> {
    let t = v.get("type").and_then(Value::as_str)?;
    if (t == "assistant" || t == "user") && v.get("parent_tool_use_id").map_or(true, Value::is_null) {
        return v.get("uuid").and_then(Value::as_str).map(str::to_string);
    }
    None
}
