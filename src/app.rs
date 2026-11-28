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

mod needs_app;

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
When the user has to do, check or decide something now (a step in a hands-on test, a device to
plug in, an approval), call it with for_user=true and say exactly what they need to do: the
orchestrator is then made to pass it on to them straight away.
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
  chart you made) by its path.
- request_user_action: a task the user must do by hand, as exact steps (never only in chat text).
  Check list_needs_you before asking anything. If ask_user times out, go ahead with the
  recommended option and say so in your summary.";

/// Sender of a handoff request; when that turn ends, the session is replaced by a new
/// one that starts from the handoff the agent wrote.
/// Marks a thread message the user has to act on (message_orchestrator for_user).
const NEEDS_USER: &str = "[needs the user] ";
const HANDOFF: &str = "handoff";
const HANDOFF_PROMPT: &str = "[handoff] This conversation is about to continue in a fresh session that won't see anything above. Write the handoff it will start from. Cover: the goal and what the user asked for, in their words where it matters; what's done and how it was checked; what's in progress and the exact next steps; decisions made and why; files, commands, branches and links that matter; the user's preferences and constraints; open questions and anything risky. If you manage threads, list each one with its job and status. Be specific and complete, and leave out the story of how you got here. Don't take any other action: reply with the handoff only.";

const ORCH_APPEND: &str = r#"
You are the orchestrator. The user talks to you; you get work done through worker threads. Each thread
is a full Claude Code agent with its own working folder and conversation history. Your tools come from
the "app" MCP server.

# Working with threads
- Split the request into independent pieces, one thread each (spawn_thread); they run in parallel.
  Reuse a thread with send_to_thread when the work continues something it did.
- Never let two threads edit the same files at once: split by file or area, or give one its own cwd.
- Write each brief so a fresh agent can act on it alone: goal, files or area, constraints, and what
  "done" looks like.
- Lead them like a team: answer their questions, correct their approach early (send_to_thread reaches
  a busy thread mid-turn), pass on decisions and other threads' results as soon as you have them,
  check long-running ones with read_thread, and nudge stuck ones.
- Check results before reporting them as done, and send a thread back to fix gaps. Then resolve it
  with resolve_thread and a short note (new work reopens it).
- Do small lookups yourself with Read/Glob/Grep; delegate real work.
- Thread messages (message_orchestrator) and final reports reach you in a [messages from threads]
  block: mid-turn, or as a new turn if you're idle. Act on each one: reply, move the work on, or tell
  the user. If nothing needs doing, say so in one line and stop. The user can also talk to any thread
  directly; what happened since their last message comes to you in a [thread updates] block.

# Don't wait in your turn
Once threads are working and you have nothing else to do, tell the user what's running and roughly
how long it'll take, then end your turn. Thread reports and user messages each start a new turn, so
nothing is missed, and a turn that only waits looks stuck to the user. wait_for_threads is only for
short waits (at most 5 minutes) when you'll act on the result right away.

# Talking to the user
- The user sees only your text and tell_user, never your thinking, tool calls or tool results. A reply
  written only in your thinking never reaches them.
- When they message you (mid-turn, or in a wait_for_threads result), reply at once with tell_user,
  before any other tool call, from what you already know. If a real answer needs work, say in one line
  what you're doing and how long it'll take, do it, then tell them. "status" means a few lines: done,
  running, next, and anything they need to do.
- When a thread reports something the user must act on, or something you promised to pass on, tell
  them right away with exactly what to do. During long work, give a one-line status now and then.
- Link threads when you mention them: [thread-name](thread:thread-name) opens it beside the chat.
- Also: update_plan (a live task list they see), ask_user (multiple-choice, only for decisions that are
  genuinely theirs), show_media (images, video, audio, PDFs). Threads have these too.
- Anything that needs the user goes on their 📌 Needs you list, never only in chat text: ask_user,
  request_user_action (exact steps they do by hand), the queue, or tell_user with important=true. Give
  each a tldr, one recommendation and a why. Check list_needs_you first so you never ask twice. If
  ask_user times out, go ahead with the recommended option and say so.
- Usage limits: the app restarts you and any stopped threads a minute after the limit resets, with a
  note to carry on, so "carry on when my usage comes back" needs nothing from the user.
- Each message starts with the current time ([now: ...]); trust it over times in older messages.

# Model and effort (spawn_thread / configure_thread)
The user wants usage kept low: pick the cheapest model and effort that will do the job well.
- claude-haiku-5-5 for trivial or mechanical work (polling, small text edits, renames, lookups,
  summaries); claude-sonnet-5-5 for routine coding, tests and small reviews; claude-opus-5-5 (the
  default) for hard, ambiguous, cross-cutting or high-stakes work.
- Effort: medium almost always, low for simple work, high only when really needed. xhigh/max cost far
  more: request them (with effort_reason) only when clearly worth it; the user is asked, and high is
  used if they decline.
- If a cheaper thread does weak work, upgrade it with configure_thread and send it on rather than
  starting over.
- After reading a thread's result, rate it with rate_thread (right, too_weak or too_strong). After a
  batch of work, or about daily, check efficiency_report and timing_report and update
  save_tuning_notes with concrete rules for this project (e.g. "docs edits: haiku, low"; "bugfixes
  here take 2x my guess"). Follow your tuning notes.

# Timelines
- For a request that takes more than a couple of minutes, tell the user a range ("about 10-20
  minutes") and record it with set_estimate (low/high minutes until all its threads are done, and a
  kind). Give every spawn_thread / send_to_thread that starts real work estimate_minutes (and
  estimate_max_minutes) and a kind.
- Base estimates on "Timing history" below: how long similar work really took here and how far off
  past estimates were. Include reading, testing, fixing and review. Parallel threads overlap; work
  that waits on another thread doesn't.
- Kinds: small-fix, bugfix, feature, refactor, tests, research, docs, review, setup, ops, or another
  short word.
- If the work turns out bigger or smaller, tell the user and call set_estimate again.

# Learning from the user
Your lessons (below) are how you get better. save_lessons keeps these lists; each one you pass
replaces the saved one:
- lessons: about the user as a person, for every workspace: how they talk (shorthand, what phrases
  mean), how much detail they want, privacy, their setup, and mistakes not to repeat with them.
- dev_lessons: how they like development work done (models and reviews, testing, QA, deploys, git).
  Development workspaces only.
- business_lessons: how they like business work done. Business workspaces only.
- project_lessons: what only matters in this project.
Use the list for this workspace's type; the other type's list is refused here.
When the user corrects you, repeats themselves or is frustrated, fix the problem first, then save the
lesson: what went wrong and the rule that prevents it. Merge new lessons into the list, one or two
lines each; combine duplicates and drop ones that turned out wrong.
When guidance conflicts, the higher wins: 1. the user's newer words; 2. project rules; 3. project
lessons; 4. lessons for this workspace type; 5. lessons about the user; 6. your defaults here. The
app's safety checks always hold.

# Project memory
save_project_notes keeps this project's brief (shown to you every turn) and rules (added to every
thread's instructions). Keep both short and current: every line is read on every turn.
Earlier Claude Code conversations are available through list_past_sessions and read_past_session. To
continue old work, prefer a fresh thread with a full brief written from what you read; use
resume_past_session (a forked copy) only when the detailed context really matters.
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
    /// Orchestrators that got a message mid-wait: true if one was from the user.
    nudged: Mutex<HashMap<String, bool>>,
    /// Orchestrators blocked in wait_for_threads (thread messages go into its result).
    waiting: Mutex<HashSet<String>>,
    /// User messages that arrived while the orchestrator was in wait_for_threads: handed
    /// over in that call's result (so they can't be missed) instead of between steps.
    held: Mutex<HashMap<String, Vec<Queued>>>,
    /// Orchestrators that got a mid-turn message from the user and haven't written any
    /// text since: (the message, how many waits were refused for it). wait_for_threads
    /// won't block until they reply, since the user only sees what they write.
    owes_reply: Mutex<HashMap<String, (String, u8)>>,
    /// Orchestrators the user corrected that haven't saved a lesson yet.
    owes_lesson: Mutex<HashSet<String>>,
    /// Conversations stopped by a usage limit, and when they continue by themselves.
    resumes: Mutex<HashMap<String, f64>>,
    /// When each running orchestrator turn last wrote the user text (or started).
    last_text: Mutex<HashMap<String, f64>>,
    /// Finished-turn notices handed to the orchestrator on its next turn.
    notices: Mutex<HashMap<String, Vec<String>>>,
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
    /// Business secrets (DPAPI-encrypted, in their own file).
    pub vault: crate::vault::Vault,
}

impl App {
    pub fn new(paths: Paths, token: String) -> Arc<App> {
        let settings = Settings::load(&paths);
        let mut store = Store::load(paths.data_dir.join("state.json"));
        // Pending queue items move to the wider hash (nothing was approved under the old one
        // yet); approved ones keep theirs so their approvals stay valid.
        for i in store.queue.iter_mut().filter(|i| i.status == "pending" && i.hash_v < 2) {
            i.hash_v = 2;
        }
        if store.workspace.is_empty() || !Path::new(&store.workspace).is_dir() {
            store.workspace = paths.default_workspace().to_string_lossy().into_owned();
        }
        let ws = store.workspace.clone();
        store.remember_workspace(&ws);
        store.open_workspace(&ws);
        store.open_workspaces.retain(|p| Path::new(p).is_dir());
        let mut known: Vec<String> = store.recent_workspaces.clone();
        known.extend(store.open_workspaces.clone());
        known.extend(store.threads.iter().map(|t| t.workspace.clone()));
        for w in known.into_iter().filter(|w| !w.is_empty()) {
            let o = store.orchestrators.entry(orch_key(&w)).or_default();
            if o.workspace.is_empty() {
                o.workspace = w;
            }
        }
        store.save();
        let hub = Hub::new(paths.data_dir.join("transcripts"), paths.data_dir.join("media"));
        hub.mark_interrupted();
        let vault = crate::vault::Vault::load(paths.config_dir.join("vault.json"));
        let app = Arc::new(App {
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
            nudged: Mutex::new(HashMap::new()),
            waiting: Mutex::new(HashSet::new()),
            held: Mutex::new(HashMap::new()),
            owes_reply: Mutex::new(HashMap::new()),
            owes_lesson: Mutex::new(HashSet::new()),
            resumes: Mutex::new(HashMap::new()),
            last_text: Mutex::new(HashMap::new()),
            notices: Mutex::new(HashMap::new()),
            inbox: Mutex::new(HashMap::new()),
            questions: Mutex::new(HashMap::new()),
            answered_ids: Mutex::new(Default::default()),
            started: Mutex::new(HashMap::new()),
            estimates: Mutex::new(HashMap::new()),
            pending_estimates: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            sessions_index: claude_projects::Index::new(),
            update: Mutex::new(None),
            vault,
        });
        app.needs_after_restart();
        app.store.lock().unwrap().save();
        app
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    pub fn workspace(&self) -> String {
        self.store.lock().unwrap().workspace.clone()
    }

    /// The orchestrator of the workspace shown in the window.
    pub fn orch_key(&self) -> String {
        orch_key(&self.workspace())
    }

    /// The workspace a conversation belongs to: an orchestrator's folder, or a thread's
    /// workspace. Background work uses this, never the workspace shown in the window.
    pub fn ws_of(&self, key: &str) -> String {
        let st = self.store.lock().unwrap();
        let w = if key.starts_with(ORCHESTRATOR) {
            st.orchestrators.get(key).map(|o| o.workspace.clone())
        } else {
            st.get(key).map(|t| t.workspace.clone())
        };
        w.filter(|w| !w.is_empty()).unwrap_or_else(|| st.workspace.clone())
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
        if t.standing {
            // Standing thread: its next scheduled run and what it used this week.
            let st = self.store.lock().unwrap();
            let next = st.notes(&t.workspace).and_then(|n| {
                n.schedules.iter().filter(|s| s.thread == t.name).filter_map(|s| crate::schedule::next_run(s, now())).min_by(f64::total_cmp)
            });
            let (_, week_start) = crate::budget::period_starts(now());
            let week: f64 = st.efficiency.iter().filter(|e| e.thread == t.name && e.ts >= week_start).map(|e| e.cost).sum();
            v["next_run"] = json!(next);
            v["week_cost"] = json!(week);
        }
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
        v["permission_effective"] = json!(if thread_perm.is_empty() { self.permission_in(&self.ws_of(key)) } else { thread_perm });
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
        let project = {
            let st = self.store.lock().unwrap();
            let n = st.notes(&ws).cloned().unwrap_or_default();
            json!({
                "brief": n.brief, "rules": n.rules, "tuning": n.tuning, "imported_at": n.imported_at, "updated_at": n.updated_at,
                "lessons": st.lessons, "lessons_updated_at": st.lessons_updated_at, "project_lessons": n.lessons,
                "dev_lessons": st.dev_lessons, "business_lessons": st.business_lessons, "lesson_scope": n.lesson_scope,
                "lessons_scoped": st.lessons_scoped || st.lessons.trim().is_empty(),
                "kind": if n.kind == "business" { "business" } else { "development" },
                "outbound_paused": n.outbound_paused || st.outbound_paused_all, "outbound_paused_all": st.outbound_paused_all,
                "paused": n.kind == "business" && n.paused,
                "budget_day": n.budget_day, "budget_week": n.budget_week,
                "gates": n.gates, "paid_api_fallback": n.paid_api_fallback, "paid_api_cap_usd": n.paid_api_cap_usd,
                "notify_events": if n.notify_set { json!(n.notify_events) } else { json!(crate::notify::EVENTS.iter().map(|(e, _)| e).collect::<Vec<_>>()) },
                "notify_phone": n.notify_phone,
                "phone_feed": n.phone_feed, "feed_id": orch_key(&ws), "phone_decide": n.phone_decide,
            })
        };
        let queue = if self.is_business(&ws) { self.queue_json(&ws) } else { Value::Null };
        let (needs, needs_all, needs_counts) = self.needs_push(queue.get("pending"));
        let phones = {
            let st = self.store.lock().unwrap();
            json!({"enrolled": st.phones, "pending": st.pending_phone})
        };
        let secrets: Vec<Value> = if self.is_business(&ws) {
            self.vault.list(&ws).into_iter().map(|(n, scope, at)| json!({"name": n, "scope": scope, "added_at": at})).collect()
        } else {
            vec![]
        };
        json!({
            "workspace": ws, "recent_workspaces": recent, "open_workspaces": self.open_workspaces_json(&needs_counts), "orchestrator": self.orch_json(),
            "threads": threads, "running": running, "waiting": waiting,
            "project": project, "queue": queue, "secrets": secrets,
            "schedules": if self.is_business(&ws) { self.schedules_json(&ws) } else { json!([]) },
            "phones": phones,
            "needs": needs,
            "needs_all": needs_all,
            "spend": if self.is_business(&ws) { let (_, s) = self.budget(&ws); json!({"day": s.day, "week": s.week}) } else { Value::Null },
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

    /// The workspace tabs, with what's happening in each.
    fn open_workspaces_json(&self, needs: &[(String, usize, usize)]) -> Value {
        let (open, current, threads) = {
            let st = self.store.lock().unwrap();
            (st.open_workspaces.clone(), st.workspace.clone(), st.threads.clone())
        };
        let questions: Vec<String> = self.questions.lock().unwrap().values().map(|q| q.key.clone()).collect();
        let rows: Vec<Value> = open.iter().map(|w| {
            let key = orch_key(w);
            let mine: Vec<&ThreadRow> = threads.iter().filter(|t| same_path(&t.workspace, w)).collect();
            let running = mine.iter().filter(|t| self.busy(&t.name)).count();
            let needs_input = questions.iter().any(|q| *q == key || mine.iter().any(|t| t.name == *q));
            let (needs_count, needs_urgent) = needs.iter().find(|(p, ..)| same_path(p, w)).map(|(_, c, u)| (*c, *u)).unwrap_or((0, 0));
            json!({
                "needs_count": needs_count, "needs_urgent": needs_urgent,
                "path": w, "key": key, "current": same_path(w, &current),
                "busy": self.busy(&key), "running_threads": running, "needs_input": needs_input,
                "queue_pending": self.queue_pending(w),
            })
        }).collect();
        json!(rows)
    }

    pub fn all_threads_json(&self) -> Value {
        let rows = self.store.lock().unwrap().threads.clone();
        json!({"threads": rows.iter().map(|t| self.thread_json(t)).collect::<Vec<_>>(), "workspace": self.workspace()})
    }

    // ---- threads ------------------------------------------------------

    pub fn create_thread(
        &self, workspace: &str, name: &str, cwd: Option<&str>, backend: Option<&str>, model: Option<&str>, effort: Option<&str>,
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
        let ws = PathBuf::from(workspace);
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
            workspace: workspace.to_string(),
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

    /// A turn cut off by a usage limit: continue the conversation by itself once the
    /// limit resets. Returns a note for the turn's notice.
    fn resume_after_limit(self: &Arc<Self>, key: &str, r: &crate::runner::TurnResult) -> Option<String> {
        if !r.limited || !r.is_error {
            return None;
        }
        // A minute after the reset; if Claude Code didn't say when, try again in 30 minutes.
        let at = r.resets_at.map_or(now() + 1800.0, |t| t + 60.0).max(now() + 60.0);
        self.schedule_resume(key, at);
        // A clock time, not "in 4h": this note is read again long after it was written.
        Some(format!("usage limit: continues by itself at {}, when it resets", clock_time(at)))
    }

    fn resumes_file(&self) -> std::path::PathBuf {
        self.paths.data_dir.join("resumes-after-update.json")
    }

    /// Before restarting into an update: remember which conversations were waiting out a
    /// usage limit, so the new version continues them. A normal quit forgets them.
    pub fn save_resumes_for_update(&self) {
        let m: HashMap<String, f64> = self.resumes.lock().unwrap().clone();
        if !m.is_empty() {
            let _ = std::fs::write(self.resumes_file(), serde_json::to_string(&m).unwrap_or_default());
        }
    }

    pub fn restore_resumes_after_update(self: &Arc<Self>) {
        let f = self.resumes_file();
        // Only right after an update: a file left from an update that failed long ago is ignored.
        let fresh = std::fs::metadata(&f)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 600));
        let Ok(s) = std::fs::read_to_string(&f) else { return };
        let _ = std::fs::remove_file(&f);
        if !fresh {
            return;
        }
        let m: HashMap<String, f64> = serde_json::from_str(&s).unwrap_or_default();
        for (key, at) in m {
            self.schedule_resume(&key, at.max(now() + 15.0));
        }
    }

    fn schedule_resume(self: &Arc<Self>, key: &str, at: f64) {
        self.resumes.lock().unwrap().insert(key.to_string(), at);
        let (app, key) = (self.clone(), key.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs_f64((at - now()).max(0.0))).await;
            {
                // Something else ran (or the user stopped it) since: nothing to do.
                let mut m = app.resumes.lock().unwrap();
                if m.get(&key) != Some(&at) {
                    return;
                }
                m.remove(&key);
            }
            if !key.starts_with(ORCHESTRATOR) && app.store.lock().unwrap().get(&key).is_none() {
                return; // deleted
            }
            let text = if key.starts_with(ORCHESTRATOR) {
                "[Automatic message from the Orchestrator app. The user did NOT send this; don't treat it as their message or thank them for it.]
Your last turn was cut off by a usage limit, which has now reset. Pick up where you left off: check on your threads (some may have been stopped by the same limit and are continuing by themselves), then carry on. Tell the user in a line that you're back."
            } else {
                "[Automatic message from the Orchestrator app, not from the user or the orchestrator.]
Your last turn was cut off by a usage limit, which has now reset. Pick up where you left off and finish the task."
            };
            app.enqueue(&key, Queued { id: String::new(), text: text.into(), sender: "app".into(), attach: vec![], fork: None }, false);
        });
    }

    pub fn stop(&self, key: &str) -> bool {
        self.questions.lock().unwrap().retain(|_, q| q.key != key);
        if self.resumes.lock().unwrap().remove(key).is_some() {
            self.hub.emit(key, json!({"kind": "notice", "text": "won't continue by itself after the usage limit resets"}));
        }
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
                    // A paused business workspace runs only the user's own messages; the rest
                    // (thread news, resumes, automatic turns) waits until it's resumed.
                    let paused = app.is_paused(&app.ws_of(&key));
                    let next = {
                        let mut queues = app.queues.lock().unwrap();
                        let next = queues.get_mut(&key).and_then(|q| {
                            let pos = if paused { q.iter().position(|m| m.sender == "you") } else if q.is_empty() { None } else { Some(0) };
                            pos.and_then(|p| q.remove(p))
                        });
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
                    app.check_budget(&app.ws_of(&key));
                    app.usage_tick(&app.ws_of(&key));
                    app.line_caps_tick(&app.ws_of(&key));
                    app.touch();
                }
                app.finish_jobs();
                app.touch();
            });
        }
        self.touch();
        (id, waited)
    }

    /// Start the worker for messages that were held (e.g. while the workspace was paused).
    fn kick(self: &Arc<Self>, key: &str) {
        if self.workers.lock().unwrap().contains(key) {
            return;
        }
        let first = self.queues.lock().unwrap().get_mut(key).and_then(VecDeque::pop_front);
        if let Some(msg) = first {
            self.enqueue(key, msg, true);
        }
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
        if key.starts_with(ORCHESTRATOR) && msg.sender == "you" && !tx.is_closed() {
            let waiting = self.waiting.lock().unwrap();
            if waiting.contains(key) {
                self.held.lock().unwrap().entry(key.to_string()).or_default().push(msg.clone());
                drop(waiting);
                self.hub.emit(key, json!({"kind": "user", "text": msg.text, "sender": msg.sender, "attachments": msg.attach, "injected": true}));
                self.nudged.lock().unwrap().insert(key.to_string(), true);
                self.touch();
                return true;
            }
        }
        let (mut prompt, images) = with_attachments(&msg.text, &msg.attach);
        if key.starts_with(ORCHESTRATOR) && msg.sender == "you" {
            if let Some(n) = self.lesson_note(key, &msg.text) {
                prompt = format!("{prompt}\n\n{n}");
            }
        }
        if key.starts_with(ORCHESTRATOR) {
            prompt = format!("{}\n{prompt}", now_note());
        }
        if tx.send(user_line(&prompt, &images)).is_err() {
            return false;
        }
        self.hub.emit(key, json!({"kind": "user", "text": msg.text, "sender": msg.sender, "attachments": msg.attach, "injected": true}));
        if key.starts_with(ORCHESTRATOR) {
            if msg.sender == "you" {
                self.owe_reply(key, &msg.text);
            }
            let mut n = self.nudged.lock().unwrap();
            let from_user = msg.sender == "you" || n.get(key).copied().unwrap_or(false);
            n.insert(key.to_string(), from_user);
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

    /// A message from the user to an orchestrator (`key`, or the shown workspace's).
    pub fn submit_orchestrator(self: &Arc<Self>, key: &str, text: &str, attach: Vec<String>, now: bool) -> &'static str {
        let key = if key.starts_with(ORCHESTRATOR) && self.store.lock().unwrap().orchestrators.get(key).is_some_and(|o| !o.workspace.is_empty()) {
            key.to_string()
        } else {
            let ws = self.workspace();
            self.claim_orchestrator(&ws);
            orch_key(&ws)
        };
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
        self.resumes.lock().unwrap().remove(name);
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
            backend: self.thread_backend_for(&row),
            append_system: self.worker_prompt(&row.workspace),
            tools: (msg.sender == HANDOFF).then(|| "Read,Glob,Grep".into()),
            mcp_config: Some(self.mcp_config(name)),
            long_tool_calls: true,
            images,
            fork_session: row.fork_pending && msg.fork.is_none(),
            permission_mode: self.permission_for(&row),
            env: self.secret_env(&row.workspace),
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
                st.record_turn_usage(&row.workspace, r);
                let est_ref = est.as_ref();
                st.record_efficiency(&row.workspace, EffRecord {
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
                    if r.backend_used == "api" {
                        row.api_cost += (r.cost_usd - r.plan_cost).max(0.0);
                    }
                    row.updated_at = now();
                    let mut notes = r.notes.clone();
                    notes.extend(self.resume_after_limit(name, r));
                    let extra = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
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
        if msg.sender == "orchestrator" || msg.sender == "schedule" {
            // Its task (or a scheduled run of a standing thread): the orchestrator hears back right away, like a teammate reporting.
            let result = self.store.lock().unwrap().get(name).map(|r| r.last_result.clone()).unwrap_or_default();
            let report: String = result.chars().take(1500).collect();
            let more = if result.chars().count() > 1500 { "\n(cut short; read_thread for the rest)" } else { "" };
            let _ = self.post_to_orchestrator(name, &format!("{n}. Its final report:\n{report}{more}"));
            if msg.sender == "schedule" {
                self.notify(&row.workspace, "schedule", &format!("Scheduled run of {name} finished"), result.lines().find(|l| !l.trim().is_empty()).unwrap_or("done"));
            }
        } else {
            self.notices.lock().unwrap().entry(orch_key(&row.workspace)).or_default().push(n);
        }
    }

    /// A thread's mid-turn message to its orchestrator. Delivered right away: into a
    /// new orchestrator turn if it's idle, otherwise into its current turn (it wakes
    /// wait_for_threads) or the start of its next one.
    pub fn message_orchestrator(self: &Arc<Self>, from: &str, text: &str, for_user: bool) -> Result<(), String> {
        let label = if for_user { "→ messaged the orchestrator (for you)" } else { "→ messaged the orchestrator" };
        self.hub.emit(from, json!({"kind": "notice", "text": format!("{label}: {}", crate::events::clip(text))}));
        if for_user {
            self.post_to_orchestrator(from, &format!("{NEEDS_USER}{text}"))
        } else {
            self.post_to_orchestrator(from, text)
        }
    }

    /// The orchestrator's tell_user: show `text` in its chat as its own words.
    pub fn tell_user(&self, key: &str, text: &str) -> f64 {
        let ts = self.hub.emit(key, json!({"kind": "text", "text": text.trim(), "sub": false}));
        self.owes_reply.lock().unwrap().remove(key);
        self.last_text.lock().unwrap().insert(key.to_string(), now());
        self.touch();
        ts
    }

    /// Seconds since the orchestrator last wrote the user any text in this turn, the
    /// first time that passes `limit`; then the clock restarts.
    pub fn silent_for(&self, key: &str, limit: f64) -> Option<f64> {
        let mut m = self.last_text.lock().unwrap();
        let t = m.get_mut(key)?;
        let gap = now() - *t;
        if gap < limit {
            return None;
        }
        *t = now();
        Some(gap)
    }

    /// Hand a thread's message or report to its orchestrator: into its turn if it's
    /// working, as a new turn if it's idle, else at the start of its next turn.
    fn post_to_orchestrator(self: &Arc<Self>, from: &str, text: &str) -> Result<(), String> {
        let ws = self.store.lock().unwrap().get(from).map(|t| t.workspace.clone()).ok_or(format!("no thread named '{from}'"))?;
        self.post_news(&orch_key(&ws), from, text);
        Ok(())
    }

    /// Deliver news to an orchestrator: into its wait, mid-turn, or as a new turn. `from`
    /// is a thread name, or "@<label>" for the app itself (e.g. "@approval queue"), which
    /// the user already knows about.
    fn post_news(self: &Arc<Self>, key: &str, from: &str, text: &str) {
        let key = key.to_string();
        self.inbox.lock().unwrap().entry(key.clone()).or_default().push((from.to_string(), text.to_string()));
        self.touch();
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
    }

    /// Take the orchestrator's pending thread messages, formatted for a prompt.
    pub fn drain_inbox(&self, key: &str) -> Option<String> {
        let msgs = self.inbox.lock().unwrap().remove(key).filter(|m| !m.is_empty())?;
        if let Some((from, text)) = msgs.iter().rev().find(|(_, t)| t.starts_with(NEEDS_USER)) {
            let t: String = text[NEEDS_USER.len()..].chars().take(600).collect();
            self.owe(key, format!(
                "thread {from} says the user needs to act, and you haven't told them:\n\"{t}\"\n\
Pass on exactly what they need to do."
            ));
        } else if let Some((from, text)) = msgs.iter().rev().find(|(f, _)| !f.starts_with('@')) {
            // Any thread news: the user may be waiting on it ("wait for switched"), and they
            // can't see tool results, so the orchestrator says something before waiting again.
            // (News from the app itself, e.g. the user's own queue decisions, owes nothing.)
            let t: String = text.chars().take(600).collect();
            let who = if from.is_empty() { "a thread".to_string() } else { format!("thread {from}") };
            self.owe_news(key, format!(
                "{who} reported news the user hasn't heard:\n\"{t}\"\n\
Tell them in a line or two what changed, and anything they're waiting on or now need to do."
            ));
        }
        Some(
            msgs.iter()
                .map(|(from, text)| match from.strip_prefix('@') {
                    Some(label) => format!("[from the {label}]\n{text}"),
                    None if from.is_empty() => text.clone(),
                    None => format!("[from thread {from}]\n{text}"),
                })
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

    /// Whether a message reached this orchestrator mid-turn since it last checked:
    /// None, Some(true) if the user sent one, Some(false) if only threads or the app did.
    pub fn take_nudge(&self, key: &str) -> Option<bool> {
        self.nudged.lock().unwrap().remove(key)
    }

    /// The user sent this orchestrator `text` mid-turn: it must answer before waiting again.
    fn owe_reply(&self, key: &str, text: &str) {
        let t: String = text.chars().take(400).collect();
        self.owe(key, format!("the user sent you a message and you haven't replied:\n\"{t}\""));
    }

    fn owe(&self, key: &str, reason: String) {
        self.owes_reply.lock().unwrap().insert(key.to_string(), (reason, 0));
    }

    /// Like `owe`, but never replaces a pending reply to the user's own message.
    fn owe_news(&self, key: &str, reason: String) {
        self.owes_reply.lock().unwrap().entry(key.to_string()).or_insert((reason, 0));
    }

    /// The user message this orchestrator hasn't answered yet, if it should answer before
    /// waiting again. Gives up after two refusals so a stubborn model can't loop.
    pub fn unanswered(&self, key: &str) -> Option<String> {
        let mut o = self.owes_reply.lock().unwrap();
        let e = o.get_mut(key)?;
        if e.1 >= 2 {
            // It ignored two refusals: show the user what it wouldn't pass on.
            let (reason, _) = o.remove(key).unwrap_or_default();
            drop(o);
            self.hub.emit(key, json!({"kind": "notice", "text": format!(
                "The orchestrator went back to waiting without telling you this: {reason}")}));
            return None;
        }
        e.1 += 1;
        Some(e.0.clone())
    }

    /// User messages held for the wait_for_threads result, formatted for it.
    pub fn take_held(&self, key: &str) -> Option<String> {
        let msgs = self.held.lock().unwrap().remove(key).unwrap_or_default();
        if msgs.is_empty() {
            return None;
        }
        if let Some(m) = msgs.last() {
            self.owe_reply(key, &m.text);
        }
        let body = msgs
            .iter()
            .map(|m| {
                let mut t = m.text.clone();
                if !m.attach.is_empty() {
                    t += &format!("\n(attached: {})", m.attach.join(", "));
                }
                t
            })
            .collect::<Vec<_>>()
            .join("\n---\n");
        let notes: Vec<String> = msgs.iter().filter_map(|m| self.lesson_note(key, &m.text)).collect();
        let body = if notes.is_empty() { body } else { format!("{body}\n\n{}", notes.join("\n")) };
        Some(format!(
            "[message from the user, sent while you were waiting]\n{body}\n\n\
The user can't see your thinking or tool calls. Reply right now with tell_user (or as the message \
of your next wait_for_threads): answer from what you know, or say in one line what you'll check or \
wait for. Then act on it."
        ))
    }

    fn open_injector(&self, key: &str) -> tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.injectors.lock().unwrap().insert(key.to_string(), tx);
        tokio::sync::Mutex::new(rx)
    }

    /// Block until the named threads are idle, a thread messages the orchestrator,
    /// or the timeout passes.
    pub async fn wait_threads(&self, key: &str, names: &[String], timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        let key = key.to_string();
        self.waiting.lock().unwrap().insert(key.clone());
        loop {
            let notified = self.changed.notified();
            if !names.iter().any(|n| self.busy(n)) || self.has_inbox(&key) || self.nudged.lock().unwrap().contains_key(&key) {
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
        self.resumes.lock().unwrap().remove(key);
        let (tx, mut rx) = watch::channel(false);
        self.cancels.lock().unwrap().insert(key.into(), tx);
        self.started.lock().unwrap().insert(key.into(), now());
        let ws = self.ws_of(key);
        let (session, last) = {
            let st = self.store.lock().unwrap();
            let o = st.orchestrators.get(key);
            (o.and_then(|o| o.session_id.clone()), o.and_then(|o| o.last_uuid.clone()))
        };
        let (resume, resume_at, start_point) = branch_point(session, last, msg);
        if let Some(f) = &msg.fork {
            self.hub.emit(key, json!({"kind": "fork", "at": f.at_ts}));
        }
        self.hub.emit(key, json!({"kind": "user", "text": msg.text, "sender": msg.sender, "attachments": msg.attach, "resume_at": start_point}));
        self.touch();
        let (mut text, images) = with_attachments(&msg.text, &msg.attach);
        if msg.sender == "you" {
            if let Some(n) = self.lesson_note(key, &msg.text) {
                text = format!("{text}\n\n{n}");
            }
        }
        let text = text.as_str();

        let notices: Vec<String> = self.notices.lock().unwrap().remove(key).unwrap_or_default();
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
        // The model has no clock; without this it guesses the time from old messages
        // (e.g. "resets in about 4h" written hours earlier).
        let mut prompt = format!("{}\n{prompt}", now_note());
        if let Some(n) = self.housekeeping_note(&self.ws_of(key)) {
            prompt = format!("{prompt}\n\n{n}");
        }
        let s = self.settings();
        let prior_cost = self.store.lock().unwrap().session_cost(resume.as_deref());
        let spec = TurnSpec {
            prompt,
            cwd: PathBuf::from(&ws),
            resume,
            resume_at,
            model: s.orch_model.clone(),
            effort: s.orch_effort.clone(),
            // Business workspaces run on the plan; only paid-order threads may fall back.
            backend: if self.is_business(&ws) { "plan".into() } else { self.resolve_backend(&ws) },
            append_system: self.orchestrator_prompt(&ws),
            tools: Some(if msg.sender == HANDOFF { "Read,Glob,Grep" } else { "Read,Glob,Grep,WebSearch,WebFetch" }.into()),
            mcp_config: Some(self.mcp_config(key)),
            long_tool_calls: true,
            images,
            fork_session: false,
            permission_mode: self.permission_in(&ws),
            env: self.secret_env(&ws),
            inject: Some(self.open_injector(key)),
            prior_cost,
        };
        let app = self.clone();
        let k = key.to_string();
        let last_uuid = Arc::new(Mutex::new(start_point.clone()));
        let lu = last_uuid.clone();
        let mut on_line = move |v: &Value| {
            app.observe_plan_usage(v);
            if v.get("type").and_then(Value::as_str) == Some("assistant")
                && v.pointer("/message/content").and_then(Value::as_array).is_some_and(|c| {
                    c.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("text")
                        && b.get("text").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty()))
                })
            {
                app.owes_reply.lock().unwrap().remove(&k);
                app.last_text.lock().unwrap().insert(k.clone(), now());
            }
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
        self.last_text.lock().unwrap().insert(key.to_string(), now());
        let result = run_turn(&s, &spec, &mut rx, &mut on_line).await;
        self.injectors.lock().unwrap().remove(key);
        self.nudged.lock().unwrap().remove(key);
        self.owes_reply.lock().unwrap().remove(key);
        self.last_text.lock().unwrap().remove(key);
        let left = self.held.lock().unwrap().remove(key).unwrap_or_default();
        {
            // Held for a wait that ended without reading them: run them next, in order.
            let mut queues = self.queues.lock().unwrap();
            let q = queues.entry(key.to_string()).or_default();
            for mut m in left.into_iter().rev() {
                m.id = format!("m{}-held", (now() * 1000.0) as u64);
                q.push_front(m);
            }
        }
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
                st.record_turn_usage(&ws, &r);
                let o = st.orchestrators.entry(key.into()).or_default();
                o.cost_usd += r.cost_usd;
                o.turns += 1;
                st.save();
                drop(st);
                if msg.sender == "schedule" {
                    let first = r.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("done").to_string();
                    let what = msg.text.lines().nth(1).unwrap_or("routine").to_string();
                    self.notify(&ws, "schedule", &format!("Scheduled run finished: {what}"), &first);
                }
                let mut notes = r.notes.clone();
                notes.extend(self.resume_after_limit(key, &r));
                let extra = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
                self.hub.emit(key, json!({"kind": "notice", "text": format!("ran on {} via {}{extra}", r.backend_used, r.key_source)}));
                if msg.sender == HANDOFF && !r.is_error && !r.text.trim().is_empty() {
                    self.finish_handoff(key, &r.text);
                }
            }
            Err(TurnError::Stopped) => { self.hub.emit(key, json!({"kind": "notice", "text": "stopped"})); }
            Err(TurnError::Failed(e)) => {
                self.hub.emit(key, json!({"kind": "notice", "text": format!("failed: {e}"), "is_error": true}));
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
        let p = std::fs::canonicalize(&p).unwrap_or(p);
        let mut s = p.to_string_lossy().into_owned();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            s = stripped.to_string();
        }
        let mut st = self.store.lock().unwrap();
        st.workspace = s.clone();
        st.remember_workspace(&s);
        st.open_workspace(&s);
        st.orchestrators.entry(orch_key(&s)).or_default().workspace = s.clone();
        st.save();
        drop(st);
        self.touch();
        Ok(s)
    }

    /// Make sure `ws`'s orchestrator knows its folder.
    fn claim_orchestrator(&self, ws: &str) {
        let mut st = self.store.lock().unwrap();
        let o = st.orchestrators.entry(orch_key(ws)).or_default();
        if o.workspace.is_empty() {
            o.workspace = ws.to_string();
            st.save();
        }
    }

    /// Close a workspace tab. Its orchestrator and threads must be idle first; its
    /// conversations stay saved and come back when it's opened again.
    pub fn close_workspace(&self, path: &str) -> Result<String, String> {
        let (target, threads) = {
            let st = self.store.lock().unwrap();
            let t = st.open_workspaces.iter().find(|p| same_path(p, path)).cloned().ok_or("that workspace isn't open")?;
            (t.clone(), st.in_workspace(&t))
        };
        if self.busy(&orch_key(&target)) || threads.iter().any(|t| self.busy(&t.name)) {
            return Err("its orchestrator or threads are still working; stop them or wait before closing it".into());
        }
        let mut st = self.store.lock().unwrap();
        if st.open_workspaces.len() <= 1 {
            return Err("it's the only open workspace".into());
        }
        let i = st.open_workspaces.iter().position(|p| same_path(p, &target)).unwrap();
        st.open_workspaces.remove(i);
        if same_path(&st.workspace, &target) {
            st.workspace = st.open_workspaces[i.min(st.open_workspaces.len() - 1)].clone();
        }
        let shown = st.workspace.clone();
        st.save();
        drop(st);
        self.touch();
        Ok(shown)
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
        if let Some(v) = patch.get("start_at_login").and_then(Value::as_bool) {
            set_start_at_login(v)?;
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
            "start_at_login": start_at_login(),
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
                env: vec![],
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
        // Every question is a Needs you entry until it's answered (permission and effort
        // prompts close when they expire; real questions stay, with their default).
        let permission = extra.get("permission").is_some() || extra.get("effort").is_some();
        let need_id = self.need_question(key, &id, &questions, permission, 0.0);
        let mut ev = json!({"kind": "question", "id": id, "questions": questions, "need_id": need_id});
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                ev[k] = v.clone();
            }
        }
        let ts = self.hub.emit(key, ev);
        if let Some(n) = self.store.lock().unwrap().needs.iter_mut().find(|n| n.id == need_id) {
            n.msg_ts = ts;
        }
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
                    self.0.need_question_dropped(&self.1);
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
        self.need_question_answered(id, &answers);
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
        if let Some(s) = sample_of(&info) {
            if st.usage_samples.last().is_none_or(|l| s.ts - l.ts >= 60.0) {
                st.usage_samples.push(s);
                let cutoff = now() - 8.0 * 86400.0;
                st.usage_samples.retain(|x| x.ts >= cutoff);
            }
        }
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
            env: vec![],
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

    /// The shown project's permission level.
    pub fn project_permission(&self) -> String {
        self.permission_in(&self.workspace())
    }

    /// Workspace `ws`'s permission level.
    pub fn permission_in(&self, ws: &str) -> String {
        let default = self.settings().permission_default;
        self.store.lock().unwrap().permission(ws, &default)
    }

    fn permission_for(&self, row: &ThreadRow) -> String {
        if row.permission.is_empty() { self.permission_in(&row.workspace) } else { row.permission.clone() }
    }

    pub fn set_project_permission(&self, level: &str) -> Result<(), String> {
        check_permission(level)?;
        let mut st = self.store.lock().unwrap();
        let ws = st.workspace.clone();
        st.set_permission(&ws, level);
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

    /// The shown project's plan/API mix (0 = all plan, 10 = all API).
    pub fn mix(&self) -> u8 {
        self.mix_in(&self.workspace())
    }

    pub fn mix_in(&self, ws: &str) -> u8 {
        let default = self.settings().api_mix;
        self.store.lock().unwrap().mix(ws, default)
    }

    pub fn set_mix(&self, value: u8) {
        let mut st = self.store.lock().unwrap();
        let ws = st.workspace.clone();
        st.set_mix(&ws, value);
        st.save();
        drop(st);
        self.touch();
    }

    /// Pick the backend for a turn from the project's plan/API mix. Every turn
    /// (threads and orchestrator) goes to whichever side is behind the target
    /// share, by API-rate cost over the last day. Returns plan, api, or auto
    /// (= plan, switching to api if the plan hits a usage limit).
    fn resolve_backend(&self, ws: &str) -> String {
        let s = self.settings();
        let mix = self.mix_in(ws);
        if s.api_key.trim().is_empty() || mix == 0 {
            return "plan".into();
        }
        if mix >= 10 {
            return "api".into();
        }
        let target = mix as f64 / 10.0;
        let (plan, api) = self.store.lock().unwrap().usage_split(ws);
        let share = if plan + api > 0.0 { api / (plan + api) } else { 0.0 };
        if share < target { "api".into() } else { "auto".into() }
    }

    fn usage_json(&self) -> Value {
        let ws = self.workspace();
        let (plan, api) = self.store.lock().unwrap().usage_split(&ws);
        let share = if plan + api > 0.0 { api / (plan + api) } else { 0.0 };
        json!({"plan_cost": plan, "api_cost": api, "api_share": share, "window_hours": 24, "mix": self.mix()})
    }

    fn orchestrator_prompt(&self, ws: &str) -> String {
        let mix = {
            let s = self.settings();
            let (plan, api) = self.store.lock().unwrap().usage_split(ws);
            let share = if plan + api > 0.0 { api / (plan + api) * 100.0 } else { 0.0 };
            format!(
                "\n# Plan/API mix\nThe user has set this project to about {}/10 of the work on API credits (0 = all on \
their Claude plan, 10 = all API). Over the last 24h, {share:.0}% has run on API (${api:.2} API vs ${plan:.2} plan, at \
API rates). The app routes every turn (yours and every thread's) to meet this target, so you don't choose backends. \
Mention it only if the user asks about cost or usage.{}\n",
                self.mix_in(ws),
                if s.api_key.trim().is_empty() { " (No API key is saved, so everything runs on the plan.)" } else { "" }
            )
        };
        let cap = format!("\n# Effort cap\nAbove effort {} needs the user's approval (asked automatically when you request it).\n",
            { let c = self.settings().max_effort; if c.is_empty() { "high".to_string() } else { c } });
        let timing = self.timing_summary(ws);
        let others = self.other_workspaces_note(ws);
        let st = self.store.lock().unwrap();
        let business = st.notes(ws).is_some_and(|n| n.kind == "business");
        let base = if business { business_append() } else { ORCH_APPEND.to_string() };
        let mut p = base + &mix + &cap + &timing + &others;
        p += &format!("\n# Your workspace\nYou are the orchestrator for {ws}. Your threads work there unless you give them another folder.\n");
        if st.notes(ws).is_some_and(|n| n.kind == "business") {
            p += BUSINESS_QUEUE_NOTE;
        }
        if let Some(n) = st.notes(ws) {
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
        // Shared lessons: about the user everywhere, plus the list for this workspace's type,
        // unless the workspace overrides it.
        let scope = st.notes(ws).map(|n| n.lesson_scope.clone()).unwrap_or_default();
        if scope != "none" && !st.lessons.trim().is_empty() {
            let head = if business && !st.lessons_scoped {
                "Lessons about this user (every workspace; save_lessons lessons). Not sorted yet: some are about \
development work (models and reviews, QA, deploys, git, worktrees); those don't apply in this business workspace"
            } else {
                "Lessons about this user (every workspace; save_lessons lessons)"
            };
            p += &format!("\n# {head}\n{}\n", st.lessons.trim());
        }
        if scope.is_empty() {
            let (head, list) = if business {
                ("Lessons for business work (business workspaces only; save_lessons business_lessons)", &st.business_lessons)
            } else {
                ("Lessons for development work (development workspaces only; save_lessons dev_lessons)", &st.dev_lessons)
            };
            if !list.trim().is_empty() {
                p += &format!("\n# {head}\n{}\n", list.trim());
            }
        }
        if let Some(l) = st.notes(ws).map(|n| n.lessons.trim()).filter(|l| !l.is_empty()) {
            p += &format!("\n# Lessons for this project (save_lessons project_lessons)\n{l}\n");
        }
        p
    }

    /// A note for the orchestrator's next message while its notes need tidying: the
    /// one-time lesson split, or notes over their size limit. Everything they hold is read
    /// on every turn. Attached to the message (not the system prompt) so it gets acted on,
    /// and repeated until it's done.
    fn housekeeping_note(&self, ws: &str) -> Option<String> {
        let note = self.housekeeping_text(ws)?;
        // The orchestrator is about to rewrite notes the user may care about: keep a copy of
        // all notes and lessons as they were (one per day, and one before the lesson sort).
        let day = chrono::Local::now().format("%Y-%m-%d");
        let mut st = self.store.lock().unwrap();
        let copy = json!({"lessons": st.lessons, "dev_lessons": st.dev_lessons, "business_lessons": st.business_lessons, "projects": st.projects});
        let copy = serde_json::to_string_pretty(&copy).unwrap_or_default();
        let backup = self.paths.data_dir.join(format!("notes-backup-{day}.json"));
        if !backup.exists() {
            let _ = std::fs::write(&backup, &copy);
        }
        if Self::lesson_sort_due(&st, ws) {
            let before = self.paths.data_dir.join("notes-backup-before-lesson-sort.json");
            if !before.exists() {
                let _ = std::fs::write(&before, &copy);
            }
            if st.lesson_sort_before.is_empty() {
                st.lesson_sort_before = lesson_lines(&st.lessons);
                st.save();
            }
        }
        Some(note)
    }

    /// The one-time sort of the old shared lessons runs in development workspaces only.
    fn lesson_sort_due(st: &crate::store::Store, ws: &str) -> bool {
        st.lessons_split && !st.lessons_scoped && !st.lessons.trim().is_empty() && !st.notes(ws).is_some_and(|n| n.kind == "business")
    }

    fn housekeeping_text(&self, ws: &str) -> Option<String> {
        let st = self.store.lock().unwrap();
        let mut over = vec![];
        let n = st.notes(ws).cloned().unwrap_or_default();
        let (typed, typed_len, typed_max) = if n.kind == "business" {
            ("business lessons", st.business_lessons.chars().count(), LIMIT_BUSINESS_LESSONS)
        } else {
            ("dev lessons", st.dev_lessons.chars().count(), LIMIT_DEV_LESSONS)
        };
        for (what, len, max) in [
            ("project brief", n.brief.chars().count(), LIMIT_BRIEF),
            ("project rules", n.rules.chars().count(), LIMIT_RULES),
            ("tuning notes", n.tuning.chars().count(), LIMIT_TUNING),
            ("lessons about the user", st.lessons.chars().count(), LIMIT_LESSONS),
            ("project lessons", n.lessons.chars().count(), LIMIT_PROJECT_LESSONS),
            (typed, typed_len, typed_max),
        ] {
            if len > max {
                over.push(format!("- {what}: {len} characters (limit {max})"));
            }
        }
        if !st.lessons_split && !st.lessons.trim().is_empty() {
            let projects: Vec<String> = st.projects.keys().cloned().collect();
            return Some(format!(
                "[Automatic note from the Orchestrator app, not from the user. Do this in this turn, after you've handled the \
message above (reply first), without mentioning it to the user.]\n\
One-time task: your lessons used to be one list shared by every project, so they mix lessons about the user with \
ones about a single project. Call save_lessons with lessons set to only the ones about the user (true in any project) \
and project_lessons set to the ones about this project. For lessons about another project, call save_lessons again \
with project set to that project's folder and project_lessons set to its lessons (projects: {}). Don't drop any \
lesson in the move.",
                projects.join(", ")
            ));
        }
        if Self::lesson_sort_due(&st, ws) {
            let projects: Vec<String> = st.projects.keys().filter(|p| !same_path(p, ws)).cloned().collect();
            // A save replaces a project's whole list, so show what each one holds now.
            let current: String = st.projects.iter().filter(|(p, n)| !same_path(p, ws) && n.kind != "business")
                .map(|(p, n)| format!("\n\nCurrent project_lessons of {p}:\n{}", if n.lessons.trim().is_empty() { "(empty)" } else { n.lessons.trim() }))
                .collect();
            return Some(format!(
                "[Automatic note from the Orchestrator app, not from the user. Do this in this turn, after you've handled the \
message above (reply first), without mentioning it to the user; the app shows them the result.]\n\
One-time task: shared lessons are now split by workspace type, so business workspaces stop getting development \
lessons. Sort every line of \"Lessons about this user\" into:\n\
- lessons: about the user as a person, true in any workspace (how they talk, what they want reported and how, \
privacy, their hardware, money and sign-ups, not idling);\n\
- dev_lessons: how they like development work done (models and reviews, testing, QA, deploys, git, worktrees);\n\
- project_lessons of the one project a lesson is about (one app's server or box, a particular machine or session, a \
project's QA watcher or design bar). Save those first: save_lessons with project set to that project's folder and \
project_lessons set to its current list plus the moved lines (projects: {}; this one is {ws}).\n\
Then save lessons and dev_lessons together in one call. Copy each line unchanged: the app checks that every line \
is kept, word for word, and refuses the save otherwise. Nothing goes into business_lessons here.{current}",
                if projects.is_empty() { "none other".to_string() } else { projects.join(", ") }
            ));
        }
        if !over.is_empty() {
            return Some(format!(
                "[Automatic note from the Orchestrator app, not from the user. Do this in this turn, after you've handled the \
message above (reply first), without mentioning it to the user.]\n\
Housekeeping: these notes, read on every turn, are over their size limit:\n{}\n\
Rewrite each under its limit and save it again (save_project_notes, save_tuning_notes or save_lessons): merge \
duplicates, tighten wording, and drop only what's outdated or already done. Keep every rule and fact that still applies.",
                over.join("\n")
            ));
        }
        None
    }

    /// Other workspaces open at the same time, each with its own orchestrator.
    fn other_workspaces_note(&self, ws: &str) -> String {
        let others: Vec<String> = self.store.lock().unwrap().open_workspaces.iter().filter(|p| !same_path(p, ws)).cloned().collect();
        if others.is_empty() {
            return String::new();
        }
        format!("\n# Other workspaces\nThe user also has these workspaces open, each with its own orchestrator and \
threads: {}. They're not yours: don't work in those folders or message their threads unless the user asks. \
The user may be looking at another workspace while you work; carry on regardless.\n", others.join(", "))
    }

    fn worker_prompt(&self, ws: &str) -> String {
        let st = self.store.lock().unwrap();
        let mut p = WORKER_APPEND.to_string();
        if st.notes(ws).is_some_and(|n| n.kind == "business") {
            p += BUSINESS_QUEUE_NOTE;
        }
        if let Some(n) = st.notes(ws).filter(|n| !n.rules.trim().is_empty()) {
            p += &format!("\n\nRules for this project (never break these without asking the user via ask_user):\n{}", n.rules.trim());
        }
        p
    }

    /// Replace the orchestrator's lessons: about the user (every workspace), development or
    /// business work (that type's workspaces), and/or one project's. Each list is saved whole;
    /// the result says what changed, line by line, so the chat shows the new lesson rather
    /// than the list's first line. `from` is the calling orchestrator's workspace (None: the
    /// user, from the window), which may only write its own type's list.
    pub fn save_lessons(&self, key: &str, from: Option<&str>, l: Lessons) -> Result<String, String> {
        if l.general.is_none() && l.dev.is_none() && l.business.is_none() && l.project.is_none() {
            return Err("pass lessons, dev_lessons, business_lessons and/or project_lessons".into());
        }
        let from_business = from.map(|ws| self.store.lock().unwrap().notes(ws).is_some_and(|n| n.kind == "business"));
        if from_business == Some(true) && l.dev.is_some() {
            return Err("dev_lessons are for development workspaces, and this is a business workspace. Save how the user \
likes business work done in business_lessons, and lessons about the user as a person in lessons.".into());
        }
        if from_business == Some(false) && l.business.is_some() {
            return Err("business_lessons are for business workspaces, and this is a development workspace. Save how the \
user likes development work done in dev_lessons, and lessons about the user as a person in lessons.".into());
        }
        let too_long = |what: &str, s: &str, max: usize| {
            let n = s.trim().chars().count();
            (n > max).then(|| format!("{what} too long ({n} characters, limit {max}): merge duplicates, drop outdated ones, and save again"))
        };
        for (what, v, max) in [("lessons", l.general, LIMIT_LESSONS), ("dev_lessons", l.dev, LIMIT_DEV_LESSONS),
            ("business_lessons", l.business, LIMIT_BUSINESS_LESSONS), ("project_lessons", l.project.map(|p| p.1), LIMIT_PROJECT_LESSONS)] {
            if let Some(e) = v.and_then(|v| too_long(what, v, max)) {
                return Err(e);
            }
        }
        let lines = lesson_lines;
        let sort = {
            let st = self.store.lock().unwrap();
            // The one-time sort ends with an orchestrator in a development workspace saving
            // dev_lessons; every line of the old list must land in some list, unchanged.
            if from_business == Some(false) && l.dev.is_some() && from.is_some_and(|ws| Self::lesson_sort_due(&st, ws)) {
                let before = if st.lesson_sort_before.is_empty() { lines(&st.lessons) } else { st.lesson_sort_before.clone() };
                let general = l.general.map(lines).unwrap_or_else(|| lines(&st.lessons));
                let dev = l.dev.map(lines).unwrap_or_default();
                let business = l.business.map(lines).unwrap_or_else(|| lines(&st.business_lessons));
                let mut projects: Vec<(String, Vec<String>)> = st.projects.iter().map(|(p, n)| (p.clone(), lines(&n.lessons))).collect();
                if let Some((ws, pl)) = l.project {
                    match projects.iter_mut().find(|(p, _)| same_path(p, ws)) {
                        Some(e) => e.1 = lines(pl),
                        None => projects.push((ws.to_string(), lines(pl))),
                    }
                }
                let place = |x: &String| -> Option<String> {
                    if general.contains(x) {
                        Some("lessons".into())
                    } else if dev.contains(x) {
                        Some("dev_lessons".into())
                    } else if business.contains(x) {
                        Some("business_lessons".into())
                    } else {
                        projects.iter().find(|(_, v)| v.contains(x)).map(|(p, _)| format!("project_lessons:{}", folder_name(p)))
                    }
                };
                let placed: Vec<(String, Option<String>)> = before.iter().map(|x| (x.clone(), place(x))).collect();
                let missing: Vec<String> = placed.iter().filter(|(_, p)| p.is_none()).map(|(x, _)| format!("- {x}")).collect();
                if !missing.is_empty() {
                    return Err(format!("Not saved: the sort must keep every lesson word for word, and these lines aren't in \
any list yet:\n{}\nCopy each, unchanged, into lessons, dev_lessons or its project's project_lessons, and save again.", missing.join("\n")));
                }
                Some(placed.into_iter().map(|(x, p)| (x, p.unwrap_or_default())).collect::<Vec<_>>())
            } else {
                None
            }
        };
        let diff = |label: &str, old: &str, new: &str| {
            let (old, new) = (lines(old), lines(new));
            let added = new.iter().filter(|l| !old.contains(l)).map(|l| format!("+ {l}"));
            let removed = old.iter().filter(|l| !new.contains(l)).map(|l| format!("- {l}"));
            let changes: Vec<String> = added.chain(removed).collect();
            if changes.is_empty() {
                format!("{label}: saved, no changes ({} lessons)", new.len())
            } else {
                format!("{label}: saved ({} lessons)\n{}", new.len(), changes.join("\n"))
            }
        };
        let mut st = self.store.lock().unwrap();
        // Nothing to sort when there were no shared lessons to begin with.
        if !st.lessons_scoped && st.lessons.trim().is_empty() && st.lesson_sort_before.is_empty() {
            st.lessons_scoped = true;
        }
        let mut out = vec![];
        if let Some(g) = l.general {
            out.push(diff("lessons", &st.lessons, g.trim()));
            st.lessons = g.trim().to_string();
        }
        if let Some(d) = l.dev {
            out.push(diff("dev_lessons", &st.dev_lessons, d.trim()));
            st.dev_lessons = d.trim().to_string();
        }
        if let Some(b) = l.business {
            out.push(diff("business_lessons", &st.business_lessons, b.trim()));
            st.business_lessons = b.trim().to_string();
        }
        if let Some((ws, pl)) = l.project {
            let n = st.notes_mut(ws);
            out.push(diff(&format!("project_lessons ({ws})"), &n.lessons, pl.trim()));
            n.lessons = pl.trim().to_string();
            n.updated_at = now();
            st.lessons_split = true;
        }
        let mut report = None;
        if let Some(placed) = &sort {
            st.lessons_scoped = true;
            st.lesson_sort_before.clear();
            let count = |pred: &dyn Fn(&str) -> bool| placed.iter().filter(|(_, p)| pred(p)).count();
            let mut per_project: Vec<(String, usize)> = vec![];
            for (_, p) in placed {
                if let Some(name) = p.strip_prefix("project_lessons:") {
                    match per_project.iter_mut().find(|(n, _)| n == name) {
                        Some(e) => e.1 += 1,
                        None => per_project.push((name.to_string(), 1)),
                    }
                }
            }
            let in_projects = count(&|p| p.starts_with("project_lessons:"));
            let text = format!(
                "Before: {} shared lessons. After: {} about you, {} for development work, {} in project lessons{}.",
                placed.len(), count(&|p| p == "lessons"), count(&|p| p == "dev_lessons"), in_projects,
                if per_project.is_empty() { String::new() } else {
                    format!(" ({})", per_project.iter().map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>().join(", "))
                },
            );
            let moves: Vec<Value> = placed.iter().filter(|(_, p)| p != "lessons")
                .map(|(x, p)| json!({"lesson": x, "to": p.replace("project_lessons:", "project lessons: ")})).collect();
            out.push(format!("Sort done. {text}"));
            report = Some((text, moves));
        }
        let changes = out.join("\n");
        st.lessons_updated_at = now();
        st.since_lessons = 0;
        st.save();
        drop(st);
        if let Some((text, moves)) = report {
            if !key.is_empty() {
                self.hub.emit(key, json!({"kind": "lessons_sorted", "text": text, "moves": moves}));
            }
        }
        self.owes_lesson.lock().unwrap().remove(key);
        self.touch();
        Ok(changes)
    }

    /// Settings → Workspace → Shared lessons (window only).
    pub fn set_lesson_scope(&self, scope: &str) -> Result<(), String> {
        let scope = match scope {
            "" | "type" => "",
            "about_me" => "about_me",
            "none" => "none",
            _ => return Err("shared lessons: by type, about_me or none".into()),
        };
        let ws = self.workspace();
        let mut st = self.store.lock().unwrap();
        st.notes_mut(&ws).lesson_scope = scope.to_string();
        st.save();
        drop(st);
        self.touch();
        Ok(())
    }

    /// Notes the app adds to a user message for the orchestrator: a correction should
    /// become a lesson, and every so often it reviews how the user talks.
    fn lesson_note(&self, key: &str, text: &str) -> Option<String> {
        if looks_like_correction(text) {
            self.owes_lesson.lock().unwrap().insert(key.to_string());
            return Some("(App note: this reads like a correction. Fix it first; then, before you wait on threads \
again, call save_lessons with what went wrong and the rule that prevents it, merged into your existing lessons.)".into());
        }
        let mut st = self.store.lock().unwrap();
        st.since_lessons += 1;
        let due = st.since_lessons >= 25;
        if due {
            st.since_lessons = 0;
        }
        st.save();
        due.then(|| "(App note: at a quiet moment this turn, look back over your recent exchanges with the user and \
update save_lessons with anything you've learned about how they talk, what they mean, and what they want.)".into())
    }

    /// Whether the orchestrator still has to save a lesson from a correction (asked once).
    pub fn take_owed_lesson(&self, key: &str) -> bool {
        self.owes_lesson.lock().unwrap().remove(key)
    }

    pub fn save_tuning(&self, ws: &str, tuning: &str) {
        let mut st = self.store.lock().unwrap();
        let n = st.notes_mut(ws);
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
    pub fn set_job_estimate(&self, key: &str, mut est: Estimate) {
        let key = key.to_string();
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
            let ws = self.ws_of(&key);
            let mut st = self.store.lock().unwrap();
            st.record_efficiency(&ws, EffRecord {
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
    pub fn timing_json(&self, ws: &str, days: f64) -> Value {
        let st = self.store.lock().unwrap();
        let since = now() - days * 86400.0;
        let recs: Vec<&EffRecord> = st
            .efficiency
            .iter()
            .filter(|r| r.ts >= since && same_path(&r.workspace, ws) && r.ok && r.wall_s > 0.0)
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
    fn timing_summary(&self, ws: &str) -> String {
        let t = self.timing_json(ws, 60.0);
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
    pub fn efficiency_json(&self, ws: &str, days: f64) -> Value {
        let st = self.store.lock().unwrap();
        let since = now() - days * 86400.0;
        let recs: Vec<&EffRecord> = st.efficiency.iter().filter(|r| r.ts >= since && !r.job && same_path(&r.workspace, ws)).collect();
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

    pub fn save_notes(&self, ws: &str, brief: Option<&str>, rules: Option<&str>) {
        let mut st = self.store.lock().unwrap();
        let n = st.notes_mut(ws);
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
    pub fn past_sessions(&self, ws: &str) -> Vec<SessionInfo> {
        let known = {
            let st = self.store.lock().unwrap();
            let mut known: HashSet<String> = st.threads.iter().filter_map(|t| t.session_id.clone()).collect();
            known.extend(st.orchestrators.values().filter_map(|o| o.session_id.clone()));
            known
        };
        self.sessions_index.sessions_for(ws).into_iter().filter(|s| !known.contains(&s.id)).collect()
    }

    /// Continue a past Claude Code conversation as a thread (a forked copy).
    pub fn resume_past_session(
        self: &Arc<Self>, ws: &str, name: &str, session_id: &str, message: &str, backend: Option<&str>, model: Option<&str>, effort: Option<&str>,
    ) -> Result<ThreadRow, String> {
        let info = self.past_sessions(ws).into_iter().find(|s| s.id == session_id).ok_or(format!("no past session {session_id}"))?;
        let cwd = if info.cwd.is_empty() { None } else { Some(info.cwd.as_str()) };
        let row = self.create_thread(ws, name, cwd, backend, model, effort)?;
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
        let count = self.past_sessions(&ws).len();
        {
            let mut st = self.store.lock().unwrap();
            st.notes_mut(&ws).imported_at = Some(now());
            st.save();
        }
        let key = orch_key(&ws);
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

    /// Workspace `ws`'s threads that aren't resolved.
    pub fn thread_names_here(&self, ws: &str) -> Vec<String> {
        let st = self.store.lock().unwrap();
        st.threads.iter().filter(|t| same_path(&t.workspace, ws) && !t.resolved).map(|t| t.name.clone()).collect()
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
            // A resolved thread tagged with a business line books its usage in the ledger
            // (only what wasn't booked before, if it was reopened and resolved again).
            if resolved && !row.tag.is_empty() && row.cost_usd - row.ledgered > 0.0001 {
                let ledger = std::path::Path::new(&row.workspace).join("ledger").join("ledger.csv");
                if ledger.is_file() {
                    use std::io::Write;
                    let delta = row.cost_usd - row.ledgered;
                    let line = format!("{},{},tokens,{},{:.4},paid,\n", chrono::Local::now().format("%Y-%m-%d"), row.tag, row.name, -delta);
                    let needs_newline = std::fs::read(&ledger).ok().is_some_and(|b| !b.is_empty() && !b.ends_with(b"\n"));
                    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&ledger) {
                        let _ = f.write_all(format!("{}{line}", if needs_newline { "\n" } else { "" }).as_bytes());
                        row.ledgered = row.cost_usd;
                    }
                }
            }
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

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn reg(args: &[&str]) -> bool {
    let mut c = std::process::Command::new("reg");
    c.args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.status().is_ok_and(|s| s.success())
}

/// Whether Windows starts Orchestrator (minimized to the tray) at sign-in.
pub fn start_at_login() -> bool {
    reg(&["query", RUN_KEY, "/v", "Orchestrator"])
}

pub fn set_start_at_login(on: bool) -> Result<(), String> {
    if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let cmd = format!("\"{}\" --minimized", exe.display());
        if !reg(&["add", RUN_KEY, "/v", "Orchestrator", "/t", "REG_SZ", "/d", &cmd, "/f"]) {
            return Err("couldn't add Orchestrator to the sign-in apps".into());
        }
    } else if start_at_login() && !reg(&["delete", RUN_KEY, "/v", "Orchestrator", "/f"]) {
        return Err("couldn't remove Orchestrator from the sign-in apps".into());
    }
    Ok(())
}

/// A Unix time as a local clock time, e.g. "8:31 PM" (with the day if it isn't today).
pub fn clock_time(t: f64) -> String {
    use chrono::TimeZone;
    let Some(dt) = chrono::Local.timestamp_opt(t as i64, 0).single() else { return String::new() };
    if dt.date_naive() == chrono::Local::now().date_naive() {
        dt.format("%-I:%M %p").to_string()
    } else {
        dt.format("%a %b %-d, %-I:%M %p").to_string()
    }
}

/// The current local time, put in front of every message the orchestrator gets.
pub fn now_note() -> String {
    format!("[now: {}]", chrono::Local::now().format("%a %b %-d, %-I:%M %p"))
}

// ---------------------------------------------------------------- approval queue

/// Development-only lines of ORCH_APPEND and what a business workspace gets instead. The
/// rest of the instructions is shared word for word, so the two modes can't drift apart.
const BUSINESS_SWAPS: [(&str, &str); 3] = [
    (
        "- claude-haiku-5-5 for trivial or mechanical work (polling, small text edits, renames, lookups,
  summaries); claude-sonnet-5-5 for routine coding, tests and small reviews; claude-opus-5-5 (the
  default) for hard, ambiguous, cross-cutting or high-stakes work.",
        "- Pick a mode for every task as pipelines/models.md describes: a script in tools/ for repeat mechanical
  work; claude-haiku-5-5 (low) for short formulaic work (listing copy, tags, filing, sorting);
  claude-sonnet-5-5 (medium) as the default for clear work of a known kind; Opus plans and Sonnet builds
  for new, unclear or long work; claude-opus-5-5 alone for small but tricky tasks; Fable only to rescue
  a paid order Opus already failed. When unsure, Sonnet, then escalate one step if it fails.",
    ),
    (
        "(e.g. \"docs edits: haiku, low\"; \"bugfixes
  here take 2x my guess\")",
        "(e.g. \"listing copy: haiku, low\"; \"custom
  gigs take 2x my guess\")",
    ),
    (
        "- Kinds: small-fix, bugfix, feature, refactor, tests, research, docs, review, setup, ops, or another
  short word.",
        "- Kinds: gig, listing, design, copy, research, venture, outreach, bookkeeping, tool, review, or another
  short word. Keep them stable: usage per kind is learned from them.",
    ),
];

/// What a business orchestrator does differently (added after the shared instructions).
const BUSINESS_MODE: &str = "
# Running the business
This workspace runs a small business. Read its brief and rules (below) and follow them.
- Each business line has a playbook in pipelines/ (ventures being tested have theirs in ventures/<slug>/).
  Brief threads by pointing them at their playbook, and keep work in the folders it names.
- Judge work by expected profit: per hour of the user's time and per unit of plan usage. Do the most
  valuable work first, and drop work that doesn't pay.
- Paid orders come first: their deadlines, QA and capacity (see Capacity below) beat everything else.
- Internal work (research, drafts, designs, tools) needs no approval. Anything that leaves the computer
  does: it goes through queue_item (see Approval queue below).
- Weekly (or when the user says \"weekly review\"): review the ledger, decide what to stop, keep or scale,
  and propose up to two new ventures as new_venture items.
- When the user rejects an item, read the reason and save the pattern as a project lesson, so the next
  item already meets it.
";

/// The orchestrator's built-in instructions for a business workspace: the shared text with
/// the development-only lines swapped, plus how to run the business.
fn business_append() -> String {
    let mut s = ORCH_APPEND.to_string();
    for (dev, biz) in BUSINESS_SWAPS {
        s = s.replace(dev, biz);
    }
    s + BUSINESS_MODE
}

/// Added to the orchestrator's and threads' instructions in business workspaces.
const BUSINESS_QUEUE_NOTE: &str = "
# Approval queue (business workspace)
Anything that leaves this computer (publishing, posting, messaging, offers, spending, price changes, new ventures) \
needs an item the user approved. Propose it with queue_item, with the exact payload. Only the user can approve or \
reject, in the app's Queue tab; you'll get a message with each decision (rejections come with a reason). Execute only \
what list_approved returns, exactly as written, then record the result with complete_item. The files in queue/ are \
read-only copies the app writes: editing or moving them changes nothing. Every item needs tldr (one plain \
line), recommend (approve, reject or edit) and why; who_executes user items also need steps ({do, expect}) and \
done_when, like request_user_action.
Secrets (API keys, tokens) live in the app's vault. Never ask for, print, store or put a secret's value anywhere. When \
an action needs one, give the item a command (PowerShell) that reads it as $env:NAME and list NAME in secrets; once \
approved, run_approved runs exactly that command with the secret set. Some harmless keys are set as environment \
variables in every session; the user decides which.
Capacity: this workspace runs on the user's Claude plan (5-hour window and weekly limit), so never take paid work \
it can't finish. Check usage_status before quoting. send_offer, accept_order and publish_gig items need order_id, \
est_units (weekly plan points) and deadline; the app checks capacity and shows the user. After an order is accepted, \
reserve_capacity it (update remaining as work goes) and release_capacity on delivery. Spawn paid-order work with \
priority \"paid-order\": it always runs; other threads are refused when usage is high.
Routines: the orchestrator can propose a recurring message to itself (e.g. 08:00 daily \"morning run\") with \
propose_schedule; it runs once the user confirms it. Messages that start with [Scheduled run ...] come from one.
";

impl App {
    /// Append to a business workspace's audit log (queue/audit.log, hash-chained) and the
    /// app's own copy. Nothing turns this off. Don't call it with the store locked.
    pub fn audit(&self, ws: &str, line: &str) {
        let line = crate::vault::redact(line);
        let line = line.as_str();
        let mut st = self.store.lock().unwrap();
        let tip = st.notes(ws).map(|n| n.audit_tip.clone()).unwrap_or_default();
        let new_tip = crate::queue::audit_append(ws, &tip, line);
        st.notes_mut(ws).audit_tip = new_tip;
        st.audit.push((now(), ws.to_string(), line.to_string()));
        let over = st.audit.len().saturating_sub(5000);
        st.audit.drain(..over);
        st.save();
    }

    /// Secrets scoped to every session, as environment variables (business workspaces).
    pub fn secret_env(&self, ws: &str) -> Vec<(String, String)> {
        if self.is_business(ws) { self.vault.env_all(ws) } else { vec![] }
    }

    /// Add or replace a secret in the shown workspace (from the window only).
    pub fn vault_set(&self, name: &str, value: &str, scope: &str) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("secrets are only for business workspaces".into());
        }
        self.vault.set(&ws, name, value, scope)?;
        self.audit(&ws, &format!("SECRET SET {name} scope={} by=user", if scope == "all" { "all-sessions" } else { "approved-actions-only" }));
        self.touch();
        Ok(())
    }

    pub fn vault_remove(&self, name: &str) {
        let ws = self.workspace();
        if self.vault.remove(&ws, name) {
            self.audit(&ws, &format!("SECRET REMOVED {name} by=user"));
        }
        self.touch();
    }

    /// run_approved: run an approved item's exact command, with the secrets it names in
    /// its environment. Each item runs once; the agent then records the result with
    /// complete_item.
    pub async fn queue_run(&self, key: &str, id: &str) -> Result<String, String> {
        let ws = self.ws_of(key);
        if !self.is_business(&ws) {
            return Err("the approval queue is only on in business workspaces".into());
        }
        let item = {
            let st = self.store.lock().unwrap();
            let paused = st.outbound_paused_all || st.notes(&ws).is_some_and(|n| n.outbound_paused);
            if paused {
                return Err("outbound is paused by the user (kill switch): nothing may run until they resume it".into());
            }
            let listable = crate::queue::listable(st.queue.iter().filter(|i| same_path(&i.workspace, &ws)), false);
            listable.into_iter().find(|i| i.id == id).cloned().ok_or(format!("{id} isn't an approved, unchanged item in this workspace (see list_approved)"))?
        };
        if item.command.trim().is_empty() {
            return Err(format!("{id} has no command to run; execute its payload yourself, then complete_item"));
        }
        if item.ran_at > 0.0 {
            return Err(format!("{id} already ran; record its result with complete_item"));
        }
        let secrets = self.vault.get(&ws, &item.secrets)?;
        {
            let mut st = self.store.lock().unwrap();
            if let Some(i) = st.queue.iter_mut().find(|i| i.id == id && same_path(&i.workspace, &ws)) {
                i.ran_at = now();
            }
            st.save();
        }
        self.audit(&ws, &format!("RUN {id} by={key}{}", if item.secrets.is_empty() { String::new() } else { format!(" (secret access: {})", item.secrets.join(", ")) }));
        let mut cmd = tokio::process::Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &item.command])
            .current_dir(&ws)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        for (k, v) in &secrets {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let out = tokio::time::timeout(Duration::from_secs(600), cmd.output()).await;
        let (code, text) = match out {
            Err(_) => (None, "timed out after 10 minutes".to_string()),
            Ok(Err(e)) => (None, format!("couldn't start PowerShell: {e}")),
            Ok(Ok(o)) => (o.status.code(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        };
        let text = crate::vault::redact(&text);
        let text: String = if text.chars().count() > 12000 { text.chars().take(12000).collect::<String>() + "\n(cut short)" } else { text };
        self.audit(&ws, &format!("RAN {id} exit={}", code.map_or("none".to_string(), |c| c.to_string())));
        Ok(format!("{id} ran (exit code {}).\n{text}\n\nNow record what happened with complete_item.", code.map_or("none".to_string(), |c| c.to_string())))
    }

    /// The workspace's budget caps and what it has spent today and this week.
    pub fn budget(&self, ws: &str) -> (crate::budget::Caps, crate::budget::Spend) {
        let st = self.store.lock().unwrap();
        let n = st.notes(ws).cloned().unwrap_or_default();
        let caps = if n.kind == "business" { crate::budget::Caps { day: n.budget_day, week: n.budget_week } } else { Default::default() };
        let spend = crate::budget::spend(st.usage.iter().filter(|u| same_path(&u.3, ws)).map(|u| (u.0, u.2)), now());
        (caps, spend)
    }

    /// May a new thread start here? Refused when a cap is reached, unless it's a paid order.
    pub fn budget_gate(&self, ws: &str, priority: &str) -> Result<(), String> {
        let (caps, spend) = self.budget(ws);
        crate::budget::spawn_allowed(caps, spend, priority)
    }

    /// After a turn: send budget alerts that are due (80% and 100%, once per period).
    pub fn check_budget(self: &Arc<Self>, ws: &str) {
        let (caps, spend) = self.budget(ws);
        let due: Vec<(String, String)> = {
            let mut st = self.store.lock().unwrap();
            let n = st.notes_mut(ws);
            let due: Vec<_> = crate::budget::alerts(caps, spend, now()).into_iter().filter(|(k, _)| !n.budget_alerts.contains(k)).collect();
            if due.is_empty() {
                return;
            }
            n.budget_alerts.extend(due.iter().map(|(k, _)| k.clone()));
            let over = n.budget_alerts.len().saturating_sub(20);
            n.budget_alerts.drain(..over);
            st.save();
            due
        };
        for (_, msg) in &due {
            self.notify(ws, "budget", "Budget alert", msg);
            self.business_event(ws, "budget", Some("budget"), json!({"text": msg}));
            self.audit(ws, &format!("BUDGET {msg}"));
            self.hub.broadcast(json!({"kind": "toast", "text": format!("{}: {msg}", ws.rsplit(['\\', '/']).next().unwrap_or(ws))}));
        }
        crate::attention();
        let blocked = crate::budget::exceeded(caps, spend).map(|why| format!(
            "\n{why}: new threads are refused unless their task is a paid order (spawn_thread priority \"{}\"). Running work continues.",
            crate::budget::PAID_ORDER
        ));
        self.post_news(&orch_key(ws), "@budget", &format!("{}{}", due.iter().map(|(_, m)| m.clone()).collect::<Vec<_>>().join("\n"), blocked.unwrap_or_default()));
    }

    /// Budget caps for the shown workspace (from the window only).
    pub fn set_budget(&self, day: f64, week: f64) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("budget caps are for business workspaces".into());
        }
        if !(day >= 0.0 && week >= 0.0 && day.is_finite() && week.is_finite()) {
            return Err("caps must be 0 or more".into());
        }
        {
            let mut st = self.store.lock().unwrap();
            let n = st.notes_mut(&ws);
            n.budget_day = day;
            n.budget_week = week;
            st.save();
        }
        self.audit(&ws, &format!("BUDGET CAPS SET day=${day:.2} week=${week:.2} by=user (0 = none)"));
        self.touch();
        Ok(())
    }

    pub fn set_thread_priority(&self, name: &str, priority: &str) {
        let mut st = self.store.lock().unwrap();
        if let Some(r) = st.get_mut(name) {
            r.priority = priority.to_string();
        }
        st.save();
    }

    /// propose_schedule: the orchestrator suggests a routine; it runs once the user confirms it.
    pub fn schedule_propose(&self, key: &str, time: &str, days: &str, message: &str, thread: &str, reason: &str) -> Result<String, String> {
        let ws = self.ws_of(key);
        if !self.is_business(&ws) {
            return Err("schedules are for business workspaces".into());
        }
        let s = self.schedule_new(&ws, None, time, days, message, thread, true, false, "orchestrator", reason)?;
        self.audit(&ws, &format!("SCHEDULE PROPOSED {} {} ({}) by=orchestrator :: {}", s.id, s.time, crate::schedule::describe_days(&s.days), s.message));
        crate::attention();
        Ok(format!(
            "proposed schedule {}: {} {} -> {}\"{}\". It runs once the user confirms it in Settings → Schedules.",
            s.id, s.time, crate::schedule::describe_days(&s.days), if s.thread.is_empty() { String::new() } else { format!("thread {}: ", s.thread) }, s.message
        ))
    }

    /// Add (id None) or change a schedule. Returns it.
    #[allow(clippy::too_many_arguments)]
    fn schedule_new(&self, ws: &str, id: Option<&str>, time: &str, days: &str, message: &str, thread: &str, enabled: bool, confirmed: bool, by: &str, reason: &str) -> Result<crate::schedule::Schedule, String> {
        let thread = thread.trim();
        if !thread.is_empty() {
            let st = self.store.lock().unwrap();
            let row = st.get(thread).ok_or(format!("no thread named {thread}"))?;
            if !same_path(&row.workspace, ws) {
                return Err(format!("thread {thread} belongs to another workspace"));
            }
        }
        if crate::schedule::parse_time(time).is_none() {
            return Err("time must be HH:MM (24-hour, local time)".into());
        }
        let days = crate::schedule::parse_days(days)?;
        if message.trim().is_empty() {
            return Err("message is required".into());
        }
        let mut st = self.store.lock().unwrap();
        let n = st.notes_mut(ws);
        let sched = match id {
            Some(id) => {
                let s = n.schedules.iter_mut().find(|s| s.id == id).ok_or(format!("no schedule {id}"))?;
                s.time = time.trim().into();
                s.days = days;
                s.message = message.trim().into();
                s.thread = thread.into();
                s.enabled = enabled;
                s.confirmed |= confirmed;
                s.clone()
            }
            None => {
                let next = n.schedules.iter().filter_map(|s| s.id.strip_prefix('s').and_then(|x| x.parse::<u32>().ok())).max().unwrap_or(0) + 1;
                let s = crate::schedule::Schedule {
                    id: format!("s{next}"), time: time.trim().into(), days, message: message.trim().into(), thread: thread.into(), enabled, confirmed,
                    created_by: by.into(), reason: reason.trim().into(), ..Default::default()
                };
                n.schedules.push(s.clone());
                s
            }
        };
        if !thread.is_empty() {
            if let Some(r) = st.get_mut(thread) {
                r.standing = true;
                r.resolved = false;
            }
        }
        st.save();
        drop(st);
        self.touch();
        Ok(sched)
    }

    /// The user adds, changes, confirms or deletes a schedule (from the window only).
    pub fn schedule_user(&self, op: &str, id: &str, time: &str, days: &str, message: &str, thread: &str, enabled: bool) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("schedules are for business workspaces".into());
        }
        match op {
            "save" => {
                let s = self.schedule_new(&ws, (!id.is_empty()).then_some(id), time, days, message, thread, enabled, true, "user", "")?;
                self.audit(&ws, &format!("SCHEDULE SAVED {} {} ({}) enabled={} by=user :: {}", s.id, s.time, crate::schedule::describe_days(&s.days), s.enabled, s.message));
            }
            "confirm" => {
                {
                    let mut st = self.store.lock().unwrap();
                    let s = st.notes_mut(&ws).schedules.iter_mut().find(|s| s.id == id).ok_or(format!("no schedule {id}"))?;
                    s.confirmed = true;
                    s.enabled = true;
                    st.save();
                }
                self.audit(&ws, &format!("SCHEDULE CONFIRMED {id} by=user"));
            }
            "delete" => {
                {
                    let mut st = self.store.lock().unwrap();
                    st.notes_mut(&ws).schedules.retain(|s| s.id != id);
                    st.save();
                }
                self.audit(&ws, &format!("SCHEDULE DELETED {id} by=user"));
            }
            _ => return Err(format!("unknown schedule op {op}")),
        }
        self.touch();
        Ok(())
    }

    /// Every 30 s: run (or skip, and log why) the schedules that are due.
    pub fn schedule_tick(self: &Arc<Self>) {
        let items: Vec<(String, crate::schedule::Schedule)> = {
            let st = self.store.lock().unwrap();
            st.projects.iter().filter(|(_, n)| n.kind == "business").flat_map(|(ws, n)| n.schedules.iter().map(move |s| (ws.clone(), s.clone()))).collect()
        };
        for (ws, s) in items {
            let (caps, spend) = self.budget(&ws);
            let spent = crate::budget::exceeded(caps, spend);
            let key = if s.thread.is_empty() { orch_key(&ws) } else { s.thread.clone() };
            let missing = !s.thread.is_empty() && self.store.lock().unwrap().get(&s.thread).is_none();
            let spent = if missing { Some(format!("thread {} no longer exists", s.thread)) } else { spent };
            let due = crate::schedule::check(&s, now(), 120.0, self.is_paused(&ws), spent.as_deref(), self.busy(&key));
            let (period, result) = match due {
                crate::schedule::Due::NotDue => continue,
                crate::schedule::Due::Run(p) => (p, "ran".to_string()),
                crate::schedule::Due::Skip(p, why) => (p, format!("skipped: {why}")),
            };
            {
                let mut st = self.store.lock().unwrap();
                if let Some(x) = st.notes_mut(&ws).schedules.iter_mut().find(|x| x.id == s.id) {
                    x.last_key = period;
                    x.last_result = result.clone();
                    x.last_at = now();
                }
                st.save();
            }
            let when = format!("{} {}", s.time, crate::schedule::describe_days(&s.days));
            let target = if s.thread.is_empty() { String::new() } else { format!(" -> thread {}", s.thread) };
            if result == "ran" {
                self.audit(&ws, &format!("SCHEDULE RUN {} ({when}){target} :: {}", s.id, s.message));
                let text = format!("[Scheduled run {} ({when}), started by the app, not typed by the user just now]\n{}", s.id, s.message);
                if s.thread.is_empty() {
                    self.claim_orchestrator(&ws);
                    self.enqueue(&key, Queued { id: String::new(), text, sender: "schedule".into(), attach: vec![], fork: None }, false);
                } else if let Err(e) = self.send_to_thread(&s.thread, &text, "schedule", vec![], false) {
                    self.audit(&ws, &format!("SCHEDULE FAILED {} :: {e}", s.id));
                }
            } else {
                self.audit(&ws, &format!("SCHEDULE SKIPPED {} ({when}){target} :: {}", s.id, &result["skipped: ".len()..]));
                let notice_key = if missing { orch_key(&ws) } else { key.clone() };
                self.hub.emit(&notice_key, json!({"kind": "notice", "text": format!("⏰ scheduled run {} ({when}){target} {result}", s.id)}));
            }
            self.touch();
        }
    }

    pub fn schedules_json(&self, ws: &str) -> Value {
        let st = self.store.lock().unwrap();
        json!(st.notes(ws).map(|n| n.schedules.iter().map(|s| {
            let mut v = json!(s);
            v["days_text"] = json!(crate::schedule::describe_days(&s.days));
            v
        }).collect::<Vec<_>>()).unwrap_or_default())
    }

    // ---- usage-aware admission control (capacity.rs) ----

    /// Backend for a thread's turn. Business workspaces stay on the plan; only paid-order
    /// threads may fall back to the API (the user's setting, an API key, the order's cap).
    fn thread_backend_for(&self, row: &ThreadRow) -> String {
        if !self.is_business(&row.workspace) {
            return self.resolve_backend(&row.workspace);
        }
        let (on, cap) = {
            let st = self.store.lock().unwrap();
            st.notes(&row.workspace).map_or((false, 0.0), |n| (n.paid_api_fallback, n.paid_api_cap_usd))
        };
        let key_set = !self.settings().api_key.trim().is_empty();
        crate::capacity::thread_backend(&row.priority, on, key_set, row.api_cost, cap).to_string()
    }

    /// The latest plan usage reading, if it's at most `max_age_s` old.
    pub fn usage_sample(&self, max_age_s: f64) -> Option<crate::capacity::Sample> {
        let st = self.store.lock().unwrap();
        let s = st.plan_usage.as_ref().and_then(sample_of);
        crate::capacity::fresh(s, now(), max_age_s)
    }

    fn gates(&self, ws: &str) -> crate::capacity::Gates {
        self.store.lock().unwrap().notes(ws).map(|n| n.gates).unwrap_or_default()
    }

    /// Usage gate for spawn_thread in business workspaces (paid orders always pass).
    pub fn usage_gate(&self, ws: &str, priority: &str) -> Result<(), String> {
        if !self.is_business(ws) {
            return Ok(());
        }
        crate::capacity::spawn_allowed(self.usage_sample(3600.0), self.gates(ws), priority)
    }

    fn reservations_in(&self, ws: &str) -> Vec<crate::capacity::Reservation> {
        self.store.lock().unwrap().reservations.iter().filter(|r| same_path(&r.workspace, ws)).cloned().collect()
    }

    /// Weekly points per dollar of API-rate cost, learned from usage samples and the plan
    /// turns recorded between them (all workspaces: the plan is shared).
    fn points_per_dollar(&self) -> Option<f64> {
        let st = self.store.lock().unwrap();
        let usage = st.usage.clone();
        crate::capacity::points_per_dollar(&st.usage_samples, |a, b| {
            usage.iter().filter(|u| u.1 == "plan" && u.0 > a && u.0 <= b).map(|u| u.2).sum()
        })
    }

    /// The usage_status tool.
    pub async fn usage_status(self: &Arc<Self>, key: &str, before: Option<f64>) -> Result<String, String> {
        let ws = self.ws_of(key);
        let mut sample = self.usage_sample(15.0 * 60.0);
        let mut note = String::new();
        if sample.is_none() {
            match self.refresh_plan_usage().await {
                Ok(_) => sample = self.usage_sample(15.0 * 60.0),
                Err(e) => note = format!(" (refreshing failed: {e})"),
            }
        }
        let t = now();
        let mut out = vec![];
        match sample {
            None => out.push(format!("Plan usage: UNKNOWN right now{note}. Don't guess: treat capacity as unavailable until it's known.")),
            Some(s) => {
                let at = |r: f64| if r > 0.0 { clock_time(r) } else { "unknown".into() };
                let five_reset = self.store.lock().unwrap().plan_usage.as_ref().and_then(|p| p.pointer("/unifiedWindows/five_hour/resetsAt")).and_then(Value::as_f64).unwrap_or(0.0);
                out.push(format!("5-hour window: {:.0}% used, resets {}", s.five * 100.0, at(five_reset)));
                out.push(format!("Week: {:.0}% used, resets {} (reading from {} ago)", s.week * 100.0, at(s.week_resets_at), fmt_minutes(t - s.ts)));
                let samples = self.store.lock().unwrap().usage_samples.clone();
                match crate::capacity::burn_last_hour(&samples, t) {
                    Some(b) => out.push(format!("Burned in the last hour: {b:.1} weekly points")),
                    None => out.push("Burned in the last hour: not enough readings yet".into()),
                }
                if let Some(b) = before {
                    if let Some(a) = crate::capacity::available_before(Some(s), t, b) {
                        out.push(format!("Projected left before {}: {a:.1} weekly points (this week's remainder plus any reset before then; other work keeps using some)", clock_time(b)));
                    }
                }
            }
        }
        let res = self.reservations_in(&ws);
        let reserved: f64 = res.iter().map(|r| r.remaining_units).sum::<f64>() + 0.0;
        out.push(format!(
            "Reserved for open orders: {reserved:.1} weekly points across {} order(s){}",
            res.len(),
            res.iter().map(|r| format!("\n  - {}: {:.1} left of {:.1}, due {}", r.order_id, r.remaining_units, r.est_units, clock_time(r.deadline))).collect::<String>()
        ));
        let g = self.gates(&ws);
        out.push(format!(
            "Gates: non-paid threads stop starting at 5-hour {:.0}% or week {:.0}%; at week {:.0}% they checkpoint and stop. Offers may use {:.0}% of what's projected to be left.",
            g.five_hour * 100.0, g.week * 100.0, g.week_stop * 100.0, g.share * 100.0
        ));
        match self.points_per_dollar() {
            None => out.push("Usage per kind of work: not learned yet (needs more usage readings while work runs on the plan).".into()),
            Some(ppd) => {
                out.push(format!("Learned: about {ppd:.2} weekly points per $1 of API-rate cost."));
                // Typical cost of one thread task, by kind and model, over the last 30 days.
                let st = self.store.lock().unwrap();
                let since = t - 30.0 * 86400.0;
                let mut groups: std::collections::BTreeMap<(String, String), Vec<f64>> = Default::default();
                for e in st.efficiency.iter().filter(|e| e.ts >= since && !e.job && same_path(&e.workspace, &ws)) {
                    let kind = if e.kind.is_empty() { "other".to_string() } else { e.kind.clone() };
                    groups.entry((kind, e.model.clone())).or_default().push(e.cost);
                }
                if !groups.is_empty() {
                    out.push("Typical usage per thread turn (median, last 30 days):".into());
                    for ((kind, model), mut costs) in groups {
                        costs.sort_by(f64::total_cmp);
                        let med = costs[costs.len() / 2];
                        out.push(format!("  - {kind} on {model}: {:.2} weekly points (${med:.2}, {} turns)", med * ppd, costs.len()));
                    }
                }
            }
        }
        Ok(out.join("\n"))
    }

    /// reserve_capacity: hold weekly points for an accepted order (call again to update
    /// what's left).
    pub fn reserve_capacity(&self, key: &str, order_id: &str, est_units: f64, remaining: Option<f64>, deadline: f64) -> Result<String, String> {
        let ws = self.ws_of(key);
        if !self.is_business(&ws) {
            return Err("capacity reservations are for business workspaces".into());
        }
        if order_id.trim().is_empty() || !(est_units > 0.0) || !(deadline > now()) {
            return Err("order_id, a positive est_units (weekly points) and a future deadline are required".into());
        }
        let mut st = self.store.lock().unwrap();
        let r = match st.reservations.iter_mut().find(|r| r.order_id == order_id && same_path(&r.workspace, &ws)) {
            Some(r) => r,
            None => {
                st.reservations.push(crate::capacity::Reservation { order_id: order_id.into(), workspace: ws.clone(), created_at: now(), ..Default::default() });
                st.reservations.last_mut().unwrap()
            }
        };
        r.est_units = est_units;
        r.remaining_units = remaining.unwrap_or(est_units).max(0.0);
        r.deadline = deadline;
        let line = format!("RESERVED {order_id} {:.1}/{:.1} weekly points, due {}", r.remaining_units, r.est_units, clock_time(deadline));
        st.save();
        drop(st);
        self.audit(&ws, &line);
        self.touch();
        Ok(format!("{line}. Update remaining with reserve_capacity as work progresses; release_capacity when delivered."))
    }

    pub fn release_capacity(&self, key: &str, order_id: &str) -> Result<String, String> {
        let ws = self.ws_of(key);
        let removed = {
            let mut st = self.store.lock().unwrap();
            let before = st.reservations.len();
            st.reservations.retain(|r| !(r.order_id == order_id && same_path(&r.workspace, &ws)));
            st.save();
            before != st.reservations.len()
        };
        if !removed {
            return Err(format!("no reservation for {order_id}"));
        }
        self.audit(&ws, &format!("RELEASED {order_id}"));
        self.touch();
        Ok(format!("released {order_id}"))
    }

    /// The app's capacity check for an offer or order in the queue.
    fn capacity_check(&self, ws: &str, order_id: &str, est_units: f64, deadline: f64) -> crate::capacity::Check {
        let others: Vec<_> = self.reservations_in(ws).into_iter().filter(|r| r.order_id != order_id).collect();
        crate::capacity::check_offer(self.usage_sample(3600.0), &others, est_units, deadline, now(), self.gates(ws))
    }

    /// After each turn: at weekly usage >= the stop gate, ask running non-paid threads in
    /// business workspaces to checkpoint and stop (once per week each).
    pub fn usage_tick(self: &Arc<Self>, ws: &str) {
        if !self.is_business(ws) {
            return;
        }
        let Some(s) = self.usage_sample(3600.0) else { return };
        let g = self.gates(ws);
        if s.week < g.week_stop {
            return;
        }
        let week_key = format!("{:.0}", s.week_resets_at);
        let targets: Vec<String> = {
            let mut st = self.store.lock().unwrap();
            let names: Vec<String> = st.threads.iter()
                .filter(|t| same_path(&t.workspace, ws) && t.priority != crate::budget::PAID_ORDER && t.stop_asked != week_key)
                .map(|t| t.name.clone()).filter(|n| self.busy(n)).collect();
            for n in &names {
                if let Some(t) = st.get_mut(n) {
                    t.stop_asked = week_key.clone();
                }
            }
            st.save();
            names
        };
        if targets.is_empty() {
            return;
        }
        let msg = format!(
            "[Automatic message from the Orchestrator app, not from the user or the orchestrator.]\nWeekly plan usage is at {:.0}% (stop gate {:.0}%): paid orders need what's left. Finish your current step, write a checkpoint (progress.md: what's done, what's next, how to resume), then stop and report.",
            s.week * 100.0, g.week_stop * 100.0
        );
        for n in &targets {
            let _ = self.send_to_thread(n, &msg, "app", vec![], true);
        }
        self.audit(ws, &format!("USAGE GATE week {:.0}%: asked {} to checkpoint and stop", s.week * 100.0, targets.join(", ")));
        self.notify(ws, "usage", &format!("Weekly plan usage at {:.0}%", s.week * 100.0), &format!("Non-paid threads asked to checkpoint and stop: {}", targets.join(", ")));
        self.business_event(ws, "usage_gate", Some("usage_gate"), json!({"week": s.week, "text": format!("Weekly plan usage at {:.0}%: non-paid threads asked to stop", s.week * 100.0)}));
        self.post_news(&orch_key(ws), "@usage gate", &format!(
            "Weekly plan usage is at {:.0}%. The app asked these non-paid threads to checkpoint and stop: {}. Only paid-order threads may start now.",
            s.week * 100.0, targets.join(", ")
        ));
    }

    /// Every 30 minutes: warn about orders whose remaining work may not fit before 75% of
    /// their deadline (once a day per order).
    pub fn capacity_watchdog(self: &Arc<Self>) {
        let wss: Vec<String> = {
            let st = self.store.lock().unwrap();
            let mut v: Vec<String> = st.reservations.iter().map(|r| r.workspace.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        for ws in wss {
            if !self.is_business(&ws) {
                continue;
            }
            let res = self.reservations_in(&ws);
            let samples = self.store.lock().unwrap().usage_samples.clone();
            let burn = crate::capacity::burn_last_hour(&samples, now()).unwrap_or(0.0);
            let risky = crate::capacity::at_risk(self.usage_sample(3600.0), burn, &res, now());
            let fresh: Vec<(String, String)> = {
                let mut st = self.store.lock().unwrap();
                let mut out = vec![];
                for (id, why) in risky {
                    if let Some(r) = st.reservations.iter_mut().find(|r| r.order_id == id && same_path(&r.workspace, &ws)) {
                        if r.alerted != day {
                            r.alerted = day.clone();
                            out.push((id, why));
                        }
                    }
                }
                st.save();
                out
            };
            if fresh.is_empty() {
                continue;
            }
            for (id, why) in &fresh {
                self.notify(&ws, "deadline", &format!("Order {id} may miss its deadline"), why);
                self.business_event(&ws, "deadline_risk", Some("deadline_risk"), json!({"ref": id, "text": why, "level": "at_risk"}));
                self.audit(&ws, &format!("DEADLINE AT RISK {id} :: {why}"));
                self.hub.broadcast(json!({"kind": "toast", "text": format!("⚠ Order {id} may miss its deadline: {why}")}));
            }
            crate::attention();
            self.post_news(&orch_key(&ws), "@deadline watchdog", &format!(
                "URGENT: {} at risk of missing its deadline:\n{}\nTell the user now, with the options in pipelines/capacity.md (they step in, an honest early extension request, or other help).",
                if fresh.len() == 1 { "an order is" } else { "orders are" },
                fresh.iter().map(|(id, w)| format!("- {id}: {w}")).collect::<Vec<_>>().join("\n")
            ));
        }
    }

    /// Usage gates and API fallback for the shown workspace (from the window only).
    pub fn set_capacity_settings(&self, gates: crate::capacity::Gates, fallback: bool, cap: f64) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("usage gates are for business workspaces".into());
        }
        let ok = |x: f64| x > 0.0 && x <= 1.0;
        if !(ok(gates.five_hour) && ok(gates.week) && ok(gates.week_stop) && ok(gates.share) && cap >= 0.0) {
            return Err("gates are percentages between 1 and 100, and the cap 0 or more".into());
        }
        {
            let mut st = self.store.lock().unwrap();
            let n = st.notes_mut(&ws);
            n.gates = gates;
            n.paid_api_fallback = fallback;
            n.paid_api_cap_usd = cap;
            st.save();
        }
        self.audit(&ws, &format!(
            "USAGE GATES SET 5h={:.0}% week={:.0}% stop={:.0}% share={:.0}% paid-order API fallback={} cap=${cap:.2} by=user",
            gates.five_hour * 100.0, gates.week * 100.0, gates.week_stop * 100.0, gates.share * 100.0, if fallback { "on" } else { "off" }
        ));
        self.touch();
        Ok(())
    }

    // ---- business feed for a paired phone (business.rs) ----

    /// The business workspace behind a feed id (its orchestrator's key), if its phone feed
    /// is on.
    pub fn feed_ws(&self, id: &str) -> Option<String> {
        let st = self.store.lock().unwrap();
        st.projects.iter().find(|(ws, n)| n.kind == "business" && n.phone_feed && orch_key(ws) == id).map(|(ws, _)| ws.clone())
    }

    /// Business workspaces with the phone feed on: [{id, name, path}].
    pub fn feed_list(&self) -> Value {
        let st = self.store.lock().unwrap();
        json!(st.projects.iter().filter(|(_, n)| n.kind == "business" && n.phone_feed).map(|(ws, _)| json!({
            "id": orch_key(ws),
            "name": std::path::Path::new(ws).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            "path": ws,
        })).collect::<Vec<_>>())
    }

    /// Turn the phone feed on or off for the shown workspace (from the window only).
    pub fn set_phone_feed(&self, on: bool) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("the phone feed is for business workspaces".into());
        }
        {
            let mut st = self.store.lock().unwrap();
            st.notes_mut(&ws).phone_feed = on;
            st.save();
        }
        self.audit(&ws, if on { "PHONE FEED ON by=user (read-only business feed)" } else { "PHONE FEED OFF by=user" });
        self.touch();
        Ok(())
    }

    /// A change the phone should hear about (the feed's event stream). `push` marks the
    /// ones worth a notification: urgent_buyer, deadline_risk, usage_gate, budget.
    pub fn business_event(&self, ws: &str, event: &str, push: Option<&str>, extra: Value) {
        let mut ev = json!({"kind": "business", "ws": orch_key(ws), "event": event, "ts": now()});
        if let Some(p) = push {
            ev["push"] = json!(p);
        }
        if let Value::Object(m) = extra {
            for (k, v) in m {
                ev[k] = v;
            }
        }
        self.hub.broadcast(ev);
    }

    fn feed_item(i: &crate::queue::QueueItem) -> Value {
        json!({
            "id": i.id, "type": i.kind, "line": i.line, "summary": i.summary, "risk": i.risk, "urgent": i.urgent,
            "expected_value_usd": i.expected_value_usd, "who_executes": i.who_executes, "payload": i.payload,
            "payload_hash": crate::queue::content_hash(i), "status": i.status,
            "attachments": i.attachments.iter().map(|a| crate::business::attachment_info(a)).collect::<Vec<_>>(),
            "created_at": i.created_at, "order_id": if i.order_id.is_empty() { Value::Null } else { json!(i.order_id) },
            "est_units": i.capacity.as_ref().map(|c| c.est_units), "deadline": i.capacity.as_ref().map(|c| c.deadline),
            "capacity": i.capacity, "manual": crate::queue::is_manual(i), "command": if i.command.is_empty() { Value::Null } else { json!(i.command) },
            "secrets": i.secrets, "hash_v": i.hash_v, "tldr": i.tldr, "recommend": i.recommend, "why": i.why,
            "plan": i.plan, "minutes": i.minutes,
            "revisions": i.revisions.iter().map(|r| json!({"id": r.id, "payload": r.payload, "payload_hash": r.payload_hash, "created_at": r.created_at})).collect::<Vec<_>>(),
            "dropped": if crate::queue::is_manual(i) { json!(Self::queue_dropped(i)) } else { Value::Null },
        })
    }

    /// Everything the phone's Business tab shows, in one read-only snapshot.
    pub fn feed_snapshot(&self, ws: &str) -> Value {
        let t = now();
        let (items, n, reservations) = {
            let st = self.store.lock().unwrap();
            let items: Vec<crate::queue::QueueItem> = st.queue.iter().filter(|i| same_path(&i.workspace, ws) && i.status == "pending").cloned().collect();
            let res: Vec<crate::capacity::Reservation> = st.reservations.iter().filter(|r| same_path(&r.workspace, ws)).cloned().collect();
            (items, st.notes(ws).cloned().unwrap_or_default(), res)
        };
        let outbound_all = self.outbound_paused_all();
        let sample = self.usage_sample(3600.0);
        let samples = self.store.lock().unwrap().usage_samples.clone();
        let (caps, spend) = self.budget(ws);
        let waiting_4h: Vec<&String> = items.iter().filter(|i| t - i.created_at >= 4.0 * 3600.0).map(|i| &i.id).collect();
        json!({
            "v": 1,
            "ws": {"id": orch_key(ws), "name": std::path::Path::new(ws).file_name().map(|x| x.to_string_lossy().to_string()), "path": ws},
            "app_version": env!("CARGO_PKG_VERSION"),
            "generated_at": t,
            "paused": {"outbound": n.outbound_paused || outbound_all, "outbound_all": outbound_all, "workspace": n.paused},
            "pending": items.iter().map(Self::feed_item).collect::<Vec<_>>(),
            "needs_you": self.needs_feed(ws),
            "today": {
                "reservations": reservations.iter().map(|r| json!({
                    "order_id": r.order_id, "est_units": r.est_units, "remaining_units": r.remaining_units,
                    "deadline": r.deadline, "created_at": r.created_at,
                    "elapsed": if r.deadline > r.created_at { ((t - r.created_at) / (r.deadline - r.created_at)).clamp(0.0, 1.0) } else { 1.0 },
                })).collect::<Vec<_>>(),
                "waiting_over_4h": waiting_4h,
                "usage": match sample {
                    None => json!({"known": false}),
                    Some(s) => json!({
                        "known": true, "five_hour": s.five, "week": s.week, "week_resets_at": s.week_resets_at, "as_of": s.ts,
                        "burn_last_hour": crate::capacity::burn_last_hour(&samples, t),
                        "gates": n.gates,
                    }),
                },
            },
            "budget": {"day_cap": caps.day, "week_cap": caps.week, "spent_day": spend.day, "spent_week": spend.week},
            "lines": self.money_json(ws)["lines"],
            "ledger": crate::business::read_ledger(ws, t),
            "schedules": n.schedules.iter().map(|s| json!({
                "id": s.id, "time": s.time, "days": crate::schedule::describe_days(&s.days), "message": s.message,
                "thread": if s.thread.is_empty() { Value::Null } else { json!(s.thread) },
                "enabled": s.enabled, "confirmed": s.confirmed, "next_run": crate::schedule::next_run(s, t),
            })).collect::<Vec<_>>(),
        })
    }

    /// A pending item's attachment (by file name) for the feed.
    pub fn feed_attachment(&self, ws: &str, item: &str, name: &str) -> Option<std::path::PathBuf> {
        let st = self.store.lock().unwrap();
        let i = st.queue.iter().find(|i| i.id == item && same_path(&i.workspace, ws))?;
        let mut files: Vec<std::path::PathBuf> = i.attachments.iter().map(std::path::PathBuf::from).collect();
        if crate::queue::is_manual(i) {
            files.extend(std::fs::read_dir(crate::queue::drop_dir(i)).into_iter().flatten().filter_map(|e| e.ok()).map(|e| e.path()));
        }
        files.into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == name))
    }

    /// Every 30 s: push hints for buyers waiting over 4 h and orders at 50% / 75% of their
    /// time (each once).
    pub fn business_tick(&self) {
        let wss: Vec<String> = {
            let st = self.store.lock().unwrap();
            st.projects.iter().filter(|(_, n)| n.kind == "business").map(|(w, _)| w.clone()).collect()
        };
        for ws in wss {
            let (waiting, res) = {
                let st = self.store.lock().unwrap();
                let w: Vec<(String, f64, bool, String)> = st.queue.iter().filter(|i| same_path(&i.workspace, &ws) && i.status == "pending")
                    .map(|i| (i.id.clone(), i.created_at, i.urgent, i.kind.clone())).collect();
                let r: Vec<crate::capacity::Reservation> = st.reservations.iter().filter(|r| same_path(&r.workspace, &ws)).cloned().collect();
                (w, r)
            };
            let due: Vec<(String, &str, String)> = {
                let mut st = self.store.lock().unwrap();
                let fresh: Vec<_> = crate::business::push_hints(&waiting, &res, now()).into_iter()
                    .filter(|(k, ..)| !st.push_sent.contains(&format!("{}|{k}", orch_key(&ws)))).collect();
                for (k, ..) in &fresh {
                    st.push_sent.push(format!("{}|{k}", orch_key(&ws)));
                }
                let over = st.push_sent.len().saturating_sub(1000);
                st.push_sent.drain(..over);
                if !fresh.is_empty() {
                    st.save();
                }
                fresh
            };
            for (key, hint, text) in due {
                let item = key.split(':').nth(1).unwrap_or("");
                self.business_event(&ws, hint, Some(hint), json!({"text": text, "ref": item}));
            }
        }
    }

    // ---- business lines (lines.rs) ----

    pub fn lines_in(&self, ws: &str) -> Vec<crate::lines::Line> {
        self.store.lock().unwrap().notes(ws).map(|n| n.lines.clone()).unwrap_or_default()
    }

    /// API-rate cost this week of threads tagged with `line`.
    pub fn line_spend_week(&self, ws: &str, line: &str) -> f64 {
        let (_, week_start) = crate::budget::period_starts(now());
        let st = self.store.lock().unwrap();
        let tagged: Vec<&str> = st.threads.iter().filter(|t| t.tag == line && same_path(&t.workspace, ws)).map(|t| t.name.as_str()).collect();
        st.efficiency.iter().filter(|e| e.ts >= week_start && tagged.contains(&e.thread.as_str())).map(|e| e.cost).sum()
    }

    /// spawn_thread's tag: a registered line that may take new work.
    pub fn line_gate(&self, ws: &str, tag: &str, priority: &str) -> Result<(), String> {
        if tag.is_empty() {
            return Ok(());
        }
        if !self.is_business(ws) {
            return Err("tags are business lines: only for business workspaces".into());
        }
        let line = self.lines_in(ws).into_iter().find(|l| l.id == tag);
        crate::lines::spawn_allowed(line.as_ref(), tag, self.line_spend_week(ws, tag), priority)
    }

    pub fn set_thread_tag(&self, name: &str, tag: &str) {
        let mut st = self.store.lock().unwrap();
        if let Some(r) = st.get_mut(name) {
            r.tag = tag.to_string();
        }
        st.save();
    }

    /// Add or change a line (the user in the window, or an approved queue item).
    pub fn line_save(&self, ws: &str, line: crate::lines::Line, by: &str) -> Result<(), String> {
        let mut line = line;
        {
            let mut st = self.store.lock().unwrap();
            let lines = &mut st.notes_mut(ws).lines;
            if let Some(old) = lines.iter_mut().find(|l| l.id == line.id) {
                line.created_by = old.created_by.clone();
                if line.start.is_empty() {
                    line.start = old.start.clone();
                }
                *old = line.clone();
            } else {
                line.created_by = by.into();
                if line.start.is_empty() {
                    line.start = chrono::Local::now().format("%Y-%m-%d").to_string();
                }
                lines.push(line.clone());
            }
            st.save();
        }
        self.audit(ws, &format!("LINE SAVED {} \"{}\" status={} budget=${:.2}/week by={by}", line.id, line.name, line.status, line.weekly_budget_usd));
        self.business_event(ws, "line_changed", None, json!({"line": line}));
        self.touch();
        Ok(())
    }

    pub fn line_set_status(&self, ws: &str, id: &str, status: &str, by: &str, reason: &str) -> Result<(), String> {
        if !crate::lines::STATUSES.contains(&status) {
            return Err(format!("status must be one of {}", crate::lines::STATUSES.join(", ")));
        }
        let line = {
            let mut st = self.store.lock().unwrap();
            let l = st.notes_mut(ws).lines.iter_mut().find(|l| l.id == id).ok_or(format!("no line {id}"))?;
            l.status = status.into();
            l.auto_paused = by == "app";
            let c = l.clone();
            st.save();
            c
        };
        self.audit(ws, &format!("LINE {} {status} by={by}{}", line.id, if reason.is_empty() { String::new() } else { format!(" :: {reason}") }));
        self.business_event(ws, "line_changed", None, json!({"line": line}));
        self.touch();
        Ok(())
    }

    /// The user's line edits from the Money tab (window only).
    pub fn line_user(&self, op: &str, v: &Value) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("lines are for business workspaces".into());
        }
        match op {
            "save" => self.line_save(&ws, crate::lines::parse(v)?, "user"),
            "status" => self.line_set_status(
                &ws,
                v.get("id").and_then(Value::as_str).unwrap_or(""),
                v.get("status").and_then(Value::as_str).unwrap_or(""),
                "user",
                "",
            ),
            _ => Err(format!("unknown line op {op}")),
        }
    }

    /// propose_line: queue a new venture for the user's approval; it starts once approved.
    pub fn line_propose(&self, key: &str, args: &Value) -> Result<String, String> {
        let ws = self.ws_of(key);
        let line = crate::lines::parse(args)?;
        if self.lines_in(&ws).iter().any(|l| l.id == line.id) {
            return Err(format!("line {} already exists; use update_line_status to change its status", line.id));
        }
        let payload = serde_json::to_string_pretty(&json!({"action": "add_line", "line": {
            "id": line.id, "name": line.name, "status": line.status, "playbook": line.playbook,
            "weekly_budget_usd": line.weekly_budget_usd, "kill_criteria": line.kill_criteria, "start": line.start, "end": line.end,
        }})).unwrap();
        let why = args.get("why").and_then(Value::as_str).unwrap_or("");
        self.queue_add(key, &json!({
            "type": "new_venture", "line": line.id, "summary": format!("Start {} \"{}\"{}", line.status, line.name, if why.is_empty() { String::new() } else { format!(": {why}") }),
            "payload": payload, "risk": "medium", "expected_value_usd": args.get("expected_value_usd").cloned().unwrap_or(json!(0)),
            "tldr": format!("Start testing a new line: {}", line.name), "recommend": "approve",
            "why": if why.is_empty() { format!("kill criteria: {}", line.kill_criteria) } else { why.to_string() },
        }), vec![])
    }

    /// update_line_status: pausing, killing or reviving a line needs the user's approval.
    pub fn line_status_propose(&self, key: &str, id: &str, status: &str, reason: &str) -> Result<String, String> {
        let ws = self.ws_of(key);
        let line = self.lines_in(&ws).into_iter().find(|l| l.id == id).ok_or(format!("no line {id}"))?;
        if !crate::lines::STATUSES.contains(&status) {
            return Err(format!("status must be one of {}", crate::lines::STATUSES.join(", ")));
        }
        if reason.trim().is_empty() {
            return Err("give a reason (the data behind it)".into());
        }
        let payload = serde_json::to_string_pretty(&json!({"action": "set_status", "line": id, "status": status, "reason": reason})).unwrap();
        self.queue_add(key, &json!({
            "type": "line_status", "line": id, "summary": format!("Set line {} \"{}\" from {} to {status}", id, line.name, line.status),
            "payload": payload, "risk": if status == "killed" { "high" } else { "medium" },
            "tldr": format!("Set line {} to {status}", line.name), "recommend": "approve", "why": reason,
        }), vec![])
    }

    /// An approved new_venture or line_status item: apply exactly its payload, then mark it
    /// done.
    fn apply_line_item(self: &Arc<Self>, item: &crate::queue::QueueItem) {
        let Ok(v) = serde_json::from_str::<Value>(&item.payload) else { return };
        let ws = item.workspace.clone();
        let result = match v.get("action").and_then(Value::as_str) {
            Some("add_line") => v.get("line").ok_or("no line".to_string()).and_then(crate::lines::parse).and_then(|l| {
                let id = l.id.clone();
                self.line_save(&ws, l, "approved queue item").map(|_| format!("line {id} added"))
            }),
            Some("set_status") => {
                let id = v.get("line").and_then(Value::as_str).unwrap_or("");
                let status = v.get("status").and_then(Value::as_str).unwrap_or("");
                let reason = v.get("reason").and_then(Value::as_str).unwrap_or("");
                self.line_set_status(&ws, id, status, "approved queue item", reason).map(|_| format!("line {id} is now {status}"))
            }
            _ => return,
        };
        let text = result.unwrap_or_else(|e| format!("couldn't apply: {e}"));
        let _ = self.queue_complete(&orch_key(&ws), &item.id, &text);
    }

    /// After each turn: pause any line over its weekly budget (the per-line cap).
    pub fn line_caps_tick(self: &Arc<Self>, ws: &str) {
        if !self.is_business(ws) {
            return;
        }
        for l in self.lines_in(ws).into_iter().filter(|l| l.weekly_budget_usd > 0.0 && (l.status == "active" || l.status == "testing")) {
            let spent = self.line_spend_week(ws, &l.id);
            if spent >= l.weekly_budget_usd {
                let why = format!("spent ${spent:.2} of its ${:.2} weekly budget", l.weekly_budget_usd);
                let _ = self.line_set_status(ws, &l.id, "paused", "app", &why);
                self.notify(ws, "budget", &format!("Line {} paused", l.id), &why);
                self.business_event(ws, "budget", Some("budget"), json!({"text": format!("Line {} paused: {why}", l.id)}));
                self.post_news(&orch_key(ws), "@budget", &format!(
                    "Line {} (\"{}\") was paused automatically: it {why}. Only paid-order threads start for it now. To resume it, propose update_line_status with the data, or ask the user.",
                    l.id, l.name
                ));
            }
        }
    }

    /// list_lines and the Money tab: every line with its numbers.
    pub fn money_json(&self, ws: &str) -> Value {
        let t = now();
        let ledger = crate::business::read_ledger(ws, t);
        let today = chrono::Local::now().date_naive();
        let lines: Vec<Value> = self.lines_in(ws).into_iter().map(|l| {
            let spent = self.line_spend_week(ws, &l.id);
            let all = ledger.as_ref().and_then(|g| g.per_line.get(&l.id).cloned()).unwrap_or_default();
            json!({
                "line": l, "spent_week": spent, "days_left": crate::lines::days_left(&l, today),
                "this_week": ledger.as_ref().and_then(|g| g.per_line_this_week.get(&l.id).cloned()).unwrap_or_default(),
                "last_week": ledger.as_ref().and_then(|g| g.per_line_last_week.get(&l.id).cloned()).unwrap_or_default(),
                "all_time": all, "per_hour": all.per_hour(), "per_token_dollar": all.per_token_dollar(),
            })
        }).collect();
        json!({"lines": lines, "ledger": ledger, "chart_weeks": crate::business::CHART_WEEKS})
    }

    // ---- signed decisions from enrolled phones (phone.rs) ----

    /// POST revise: store a draft revision of a pending item's payload (unsigned; it changes
    /// nothing until a phone signs its hash). Returns the revision and its hash.
    pub fn phone_revise(&self, ws_id: &str, item_id: &str, payload: &str) -> Result<Value, crate::phone::Refusal> {
        let ws = self.feed_ws(ws_id).ok_or((404, "no_feed", "no business workspace with that id has the phone feed on".to_string()))?;
        if payload.trim().is_empty() {
            return Err((400, "bad_request", "payload is empty".into()));
        }
        if let Some(name) = crate::vault::contains_secret(payload) {
            return Err((400, "bad_request", format!("the payload contains the value of secret {name}")));
        }
        let (rev, original) = {
            let mut st = self.store.lock().unwrap();
            if !st.notes(&ws).is_some_and(|n| n.phone_decide) {
                return Err((403, "decisions_off", "phone decisions are off for this workspace (Settings → Phone)".into()));
            }
            let item = st.queue.iter_mut().find(|i| i.id == item_id && same_path(&i.workspace, &ws)).ok_or((404, "no_item", "no such item in this workspace".to_string()))?;
            if item.status != "pending" {
                return Err((409, "not_pending", format!("the item is already {}", item.status)));
            }
            let n = item.revisions.len() + 1;
            let rev = crate::queue::Revision {
                id: format!("r{n}"), payload: payload.to_string(),
                payload_hash: crate::queue::revision_hash(item, payload), created_at: now(),
            };
            item.revisions.push(rev.clone());
            let over = item.revisions.len().saturating_sub(20);
            item.revisions.drain(..over);
            let original = crate::queue::content_hash(item);
            st.save();
            (rev, original)
        };
        self.audit(&ws, &format!("REVISION {} of {item_id} drafted via phone api (original #{}, revision #{})", rev.id, &original[..12], &rev.payload_hash[..12]));
        self.touch();
        Ok(json!({"ok": true, "item_id": item_id, "revision": rev.id, "payload": rev.payload, "payload_hash": rev.payload_hash, "original_payload_hash": original}))
    }

    fn phone_nonce_check(&self, device: &str, nonce: &str) -> bool {
        let t = now();
        let mut st = self.store.lock().unwrap();
        st.phone_nonces.retain(|(_, _, at)| t - at < crate::phone::NONCE_KEEP_S);
        st.phone_nonces.iter().any(|(d, n, _)| d == device && n == nonce)
    }

    fn phone_nonce_use(&self, device: &str, nonce: &str) {
        let mut st = self.store.lock().unwrap();
        st.phone_nonces.push((device.to_string(), nonce.to_string(), now()));
        st.save();
    }

    /// POST pause: stopping is safe, so the token alone may (outbound or workspace).
    pub fn phone_pause(self: &Arc<Self>, ws_id: &str, target: &str) -> Result<Value, crate::phone::Refusal> {
        let ws = self.feed_ws(ws_id).ok_or((404, "no_feed", "no business workspace with that id has the phone feed on".to_string()))?;
        match target {
            "outbound" => {
                {
                    let mut st = self.store.lock().unwrap();
                    st.notes_mut(&ws).outbound_paused = true;
                    st.save();
                }
                self.audit(&ws, "OUTBOUND PAUSED via=phone api (kill switch on)");
                self.post_news(&orch_key(&ws), "@app", "The user PAUSED OUTBOUND from their phone (kill switch). Nothing may be executed, sent or published until they resume it.");
            }
            "workspace" => {
                {
                    let mut st = self.store.lock().unwrap();
                    st.notes_mut(&ws).paused = true;
                    st.save();
                }
                self.audit(&ws, "WORKSPACE PAUSED via=phone api (all turns stopped)");
                let mut keys = self.thread_names_here(&ws);
                keys.push(orch_key(&ws));
                for k in &keys {
                    self.resumes.lock().unwrap().remove(k);
                    self.stop(k);
                }
            }
            _ => return Err((400, "bad_request", "target must be outbound or workspace".into())),
        }
        self.business_event(&ws, "pause_changed", None, json!({"target": target, "paused": true, "via": "phone"}));
        self.touch();
        Ok(json!({"ok": true, "target": target, "paused": true}))
    }

    /// POST unpause: starting is signed.
    pub fn phone_unpause(self: &Arc<Self>, ws_id: &str, target: &str, s: &crate::phone::Signed) -> Result<Value, crate::phone::Refusal> {
        let ws = self.feed_ws(ws_id).ok_or((404, "no_feed", "no business workspace with that id has the phone feed on".to_string()))?;
        if !["outbound", "workspace"].contains(&target) {
            return Err((400, "bad_request", "target must be outbound or workspace".into()));
        }
        let phone = {
            let st = self.store.lock().unwrap();
            if !st.notes(&ws).is_some_and(|n| n.phone_decide) {
                return Err((403, "decisions_off", "phone decisions are off for this workspace (Settings → Phone)".into()));
            }
            let phones = st.phones.clone();
            drop(st);
            let used = self.phone_nonce_check(&s.device, &s.nonce);
            let msg = crate::phone::unpause_message(&s.device, ws_id, target, s.ts, &s.nonce);
            crate::phone::check_signed(&phones, s, &msg, now(), used)?.clone()
        };
        self.phone_nonce_use(&s.device, &s.nonce);
        let via = format!("phone:{}", phone.fingerprint);
        if target == "outbound" {
            {
                let mut st = self.store.lock().unwrap();
                st.notes_mut(&ws).outbound_paused = false;
                st.save();
            }
            self.audit(&ws, &format!("OUTBOUND RESUMED by=user via={via}"));
            self.post_news(&orch_key(&ws), "@app", "The user resumed outbound from their phone: approved items are available in list_approved again.");
        } else {
            {
                let mut st = self.store.lock().unwrap();
                st.notes_mut(&ws).paused = false;
                st.save();
            }
            self.audit(&ws, &format!("WORKSPACE RESUMED by=user via={via}"));
            let mut keys = self.thread_names_here(&ws);
            keys.push(orch_key(&ws));
            for k in &keys {
                self.kick(k);
            }
            self.post_news(&orch_key(&ws), "@app", "The user resumed this workspace from their phone after a pause. Check where things stood and carry on.");
        }
        self.business_event(&ws, "pause_changed", None, json!({"target": target, "paused": false, "via": via}));
        self.touch();
        Ok(json!({"ok": true, "target": target, "paused": false, "via": via}))
    }

    /// POST manual/{item}/file: a file from the phone for a manual task (images and PDF, 25 MB).
    pub fn phone_manual_file(&self, ws_id: &str, item_id: &str, name: &str, b64: &str) -> Result<Value, crate::phone::Refusal> {
        use base64::Engine;
        let ws = self.feed_ws(ws_id).ok_or((404, "no_feed", "no business workspace with that id has the phone feed on".to_string()))?;
        let owned = self.store.lock().unwrap().queue.iter().any(|i| i.id == item_id && same_path(&i.workspace, &ws));
        if !owned {
            return Err((404, "no_item", "no such item in this workspace".into()));
        }
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        if !["png", "jpg", "jpeg", "webp", "gif", "pdf"].contains(&ext.as_str()) {
            return Err((415, "bad_type", "only images (png, jpg, webp, gif) and PDF".into()));
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| (400, "bad_request", format!("bad file data: {e}")))?;
        if bytes.len() > 25 * 1024 * 1024 {
            return Err((413, "too_large", "files over 25 MB aren't taken".into()));
        }
        let path = self.queue_drop_file(item_id, name, &base64::engine::general_purpose::STANDARD.encode(&bytes)).map_err(|e| (409, "not_manual", e))?;
        self.business_event(&ws, "manual_file", None, json!({"item_id": item_id, "name": crate::queue::safe_file_name(name)}));
        Ok(json!({"ok": true, "item_id": item_id, "name": crate::queue::safe_file_name(name), "path": path}))
    }

    /// The file list of a manual task: (name, sha256 hex).
    fn manual_files(&self, item: &crate::queue::QueueItem) -> Vec<(String, String)> {
        use sha2::{Digest, Sha256};
        let dir = crate::queue::drop_dir(item);
        Self::queue_dropped(item).into_iter().map(|n| {
            let h = std::fs::read(dir.join(&n)).map(|b| format!("{:x}", Sha256::digest(&b))).unwrap_or_default();
            (n, h)
        }).collect()
    }

    /// POST manual/{item}/done: the phone's signed Done over the exact file list.
    pub fn phone_manual_done(self: &Arc<Self>, ws_id: &str, item_id: &str, s: &crate::phone::Signed) -> Result<Value, crate::phone::Refusal> {
        let ws = self.feed_ws(ws_id).ok_or((404, "no_feed", "no business workspace with that id has the phone feed on".to_string()))?;
        let (item, phones, on) = {
            let st = self.store.lock().unwrap();
            let item = st.queue.iter().find(|i| i.id == item_id && same_path(&i.workspace, &ws)).cloned();
            (item, st.phones.clone(), st.notes(&ws).is_some_and(|n| n.phone_decide))
        };
        if !on {
            return Err((403, "decisions_off", "phone decisions are off for this workspace (Settings → Phone)".into()));
        }
        let item = item.ok_or((404, "no_item", "no such item in this workspace".to_string()))?;
        let files = self.manual_files(&item);
        let used = self.phone_nonce_check(&s.device, &s.nonce);
        let msg = crate::phone::done_message(&s.device, ws_id, item_id, &crate::phone::files_hash(&files), s.ts, &s.nonce);
        let phone = crate::phone::check_signed(&phones, s, &msg, now(), used)?.clone();
        self.phone_nonce_use(&s.device, &s.nonce);
        self.queue_user_done(item_id).map_err(|e| (409, "not_manual", e))?;
        self.audit(&ws, &format!("DONE confirmed via=phone:{} for {item_id} ({} file(s))", phone.fingerprint, files.len()));
        Ok(json!({"ok": true, "item_id": item_id, "files": files.iter().map(|(n, h)| json!({"name": n, "sha256": h})).collect::<Vec<_>>(), "via": format!("phone:{}", phone.fingerprint)}))
    }

    /// The user pasted a phone's bundle (window only): parse it and show its fingerprint
    /// for confirmation. Nothing is enrolled yet.
    pub fn phone_enroll_begin(&self, bundle: &str) -> Result<(), String> {
        let p = crate::phone::parse_bundle(bundle)?;
        self.store.lock().unwrap().pending_phone = Some(p);
        self.touch();
        Ok(())
    }

    /// The user confirmed the fingerprint matches the phone's (window only): enrol it.
    pub fn phone_enroll_confirm(&self, device: &str, fingerprint: &str) -> Result<(), String> {
        let (phone, wss) = {
            let mut st = self.store.lock().unwrap();
            // The window sends back the fingerprint it showed: only that exact key is enrolled.
            let mut p = st.pending_phone.take().filter(|p| p.device == device && p.fingerprint == fingerprint).ok_or("nothing to confirm for that device and fingerprint")?;
            if st.phones.iter().any(|x| x.key == p.key && x.revoked_at > 0.0) {
                return Err("that key was revoked; pair the phone again so it makes a new key".into());
            }
            p.enrolled_at = now();
            st.phones.retain(|x| x.device != p.device);
            st.phones.push(p.clone());
            st.save();
            let wss: Vec<String> = st.projects.iter().filter(|(_, n)| n.kind == "business").map(|(w, _)| w.clone()).collect();
            (p, wss)
        };
        for ws in wss {
            self.audit(&ws, &format!("PHONE ENROLLED {} \"{}\" fingerprint {} by=user", phone.device, phone.name, phone.fingerprint));
        }
        self.touch();
        Ok(())
    }

    pub fn phone_enroll_cancel(&self) {
        self.store.lock().unwrap().pending_phone = None;
        self.touch();
    }

    /// Revoke a phone (window only): it can never decide again.
    pub fn phone_revoke(&self, device: &str) -> Result<(), String> {
        let (phone, wss) = {
            let mut st = self.store.lock().unwrap();
            let p = st.phones.iter_mut().find(|p| p.device == device && p.revoked_at == 0.0).ok_or("no such active phone")?;
            p.revoked_at = now();
            let p = p.clone();
            st.save();
            let wss: Vec<String> = st.projects.iter().filter(|(_, n)| n.kind == "business").map(|(w, _)| w.clone()).collect();
            (p, wss)
        };
        for ws in wss {
            self.audit(&ws, &format!("PHONE REVOKED {} fingerprint {} by=user", phone.device, phone.fingerprint));
        }
        self.touch();
        Ok(())
    }

    /// Allow signed decisions from enrolled phones in the shown workspace (window only).
    pub fn set_phone_decide(&self, on: bool) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("phone decisions are for business workspaces".into());
        }
        {
            let mut st = self.store.lock().unwrap();
            st.notes_mut(&ws).phone_decide = on;
            st.save();
        }
        self.audit(&ws, if on { "PHONE DECISIONS ON by=user (signed approve/reject from enrolled phones)" } else { "PHONE DECISIONS OFF by=user" });
        self.touch();
        Ok(())
    }

    /// POST /api/business/{ws}/decide: a phone's signed approve or reject. Every check is in
    /// phone::check_decide; then the same decide function the window uses runs.
    pub fn phone_decide(self: &Arc<Self>, ws_id: &str, req: &crate::phone::DecideRequest) -> Result<Value, crate::phone::Refusal> {
        let ws = self.feed_ws(ws_id).ok_or((404, "no_feed", "no business workspace with that id has the phone feed on".to_string()))?;
        let t = now();
        let (phone, capacity_failed, signed_hash, revision) = {
            let mut st = self.store.lock().unwrap();
            if !st.notes(&ws).is_some_and(|n| n.phone_decide) {
                return Err((403, "decisions_off", "phone decisions are off for this workspace (Settings → Phone)".into()));
            }
            st.phone_nonces.retain(|(_, _, at)| t - at < crate::phone::NONCE_KEEP_S);
            let used = st.phone_nonces.iter().any(|(d, n, _)| *d == req.device && *n == req.nonce);
            let found = st.queue.iter().find(|i| i.id == req.item_id && same_path(&i.workspace, &ws)).cloned();
            // A signed draft revision: the signature must cover that draft's hash, recomputed
            // now from the item as it is (so a changed attachment still fails).
            if !req.revision.is_empty() && !found.as_ref().is_some_and(|i| i.revisions.iter().any(|r| r.id == req.revision)) {
                return Err((404, "no_revision", "no such revision of this item".into()));
            }
            let revision = found.as_ref().and_then(|i| i.revisions.iter().find(|r| r.id == req.revision).cloned());
            let item = found.as_ref().map(|i| crate::phone::ItemNow {
                status: &i.status,
                payload_hash: match &revision {
                    Some(r) => crate::queue::revision_hash(i, &r.payload),
                    None => crate::queue::content_hash(i),
                },
            });
            let signed_hash = item.as_ref().map(|i| i.payload_hash.clone());
            let phone = crate::phone::check_decide(&st.phones, req, ws_id, item, t, used)?.clone();
            // Single use: remembered before anything is decided.
            st.phone_nonces.push((req.device.clone(), req.nonce.clone(), t));
            st.save();
            (phone, found.is_some_and(|i| i.capacity.as_ref().is_some_and(|c| !c.ok)), signed_hash, revision)
        };
        if capacity_failed && req.decision == "approve" {
            return Err((409, "capacity_failed", "this item failed the capacity check: approve it in the Orchestrator window (it needs a second, explicit click)".into()));
        }
        let mut via = format!("phone:{}", phone.fingerprint);
        // The hash the signature covers is what gets approved (an attachment swapped since
        // then voids the item instead). A revision approves its edited payload.
        let (action, payload) = match (&revision, req.decision.as_str()) {
            (Some(r), "approve") => {
                via = format!("{via} revision={}", r.id);
                ("edit_approve", Some(r.payload.as_str()))
            }
            _ => (req.decision.as_str(), None),
        };
        self.queue_decide(&req.item_id, action, &req.reason, payload, false, &via, signed_hash.as_deref()).map_err(|e| (409, "decide_failed", e))?;
        let status = self.store.lock().unwrap().queue.iter().find(|i| i.id == req.item_id).map(|i| i.status.clone()).unwrap_or_default();
        Ok(json!({"ok": true, "item_id": req.item_id, "status": status, "via": via}))
    }

    // ---- manual tasks (queue items the user does by hand) ----

    /// Save a file the user dropped on a manual task's card to inbox/images/<id>/.
    pub fn queue_drop_file(&self, id: &str, name: &str, b64: &str) -> Result<String, String> {
        use base64::Engine;
        let item = self.store.lock().unwrap().queue.iter().find(|i| i.id == id).cloned().ok_or(format!("no queue item {id}"))?;
        if !crate::queue::is_manual(&item) || !["pending", "approved"].contains(&item.status.as_str()) {
            return Err(format!("{id} isn't an open manual task"));
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| format!("bad file data: {e}"))?;
        if bytes.len() > 50 * 1024 * 1024 {
            return Err("files over 50 MB aren't taken".into());
        }
        let dir = crate::queue::drop_dir(&item);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(crate::queue::safe_file_name(name));
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        self.touch();
        Ok(path.display().to_string())
    }

    /// Files dropped on a manual task so far (names).
    pub fn queue_dropped(item: &crate::queue::QueueItem) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(crate::queue::drop_dir(item))
            .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// The user's Done on a manual task (from the window only): it's done, and the
    /// orchestrator is told which files are in.
    pub fn queue_user_done(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let item = {
            let mut st = self.store.lock().unwrap();
            let item = st.queue.iter_mut().find(|i| i.id == id).ok_or(format!("no queue item {id}"))?;
            let files: Vec<String> = Self::queue_dropped(item);
            crate::queue::done_by_user(item, &files, now())?;
            crate::queue::mirror(item);
            let c = item.clone();
            st.save();
            c
        };
        let files = Self::queue_dropped(&item);
        let rel = format!("inbox/images/{}/", item.id);
        self.business_event(&item.workspace, "item_completed", None, json!({"item_id": item.id, "by": "user"}));
        self.audit(&item.workspace, &format!("DONE BY USER {}{}", item.id, if files.is_empty() { String::new() } else { format!(" ({} file(s) in {rel})", files.len()) }));
        self.post_news(&orch_key(&item.workspace), "@approval queue", &format!(
            "The user did {} ({}, line {}) by hand: {}{}",
            item.id, item.kind, if item.line.is_empty() { "-" } else { &item.line }, item.summary,
            if files.is_empty() { "\nNo files were dropped in.".to_string() } else { format!("\nTheir files are in {rel}: {}. Pick them up from there.", files.join(", ")) }
        ));
        self.queue_changed(&item.workspace);
        Ok(())
    }

    // ---- inbox capture (inbox.rs) ----

    /// Where captures go: the shown workspace if it's a business one, else the first open
    /// business workspace.
    pub fn inbox_target(&self) -> Option<String> {
        let ws = self.workspace();
        if self.is_business(&ws) {
            return Some(ws);
        }
        let open = self.store.lock().unwrap().open_workspaces.clone();
        open.into_iter().find(|w| self.is_business(w))
    }

    /// After a capture: tell the user and the workspace's orchestrator.
    pub fn inbox_saved(self: &Arc<Self>, ws: &str, path: &std::path::Path, what: &str) {
        let rel = path.strip_prefix(ws).map(|p| p.display().to_string()).unwrap_or_else(|_| path.display().to_string());
        let rel = rel.trim_start_matches(['\\', '/']).replace('\\', "/");
        self.hub.broadcast(json!({"kind": "toast", "text": format!("saved to {rel}")}));
        self.post_news(&orch_key(ws), "@inbox", &format!(
            "The user put {what} in the inbox: {rel}. Read it and triage it (reply drafts and offers go through the queue)."
        ));
    }

    /// Ctrl+Alt+I or the Inbox box's clipboard button: save what's on the clipboard.
    pub fn inbox_from_clipboard(self: &Arc<Self>) -> Result<(), String> {
        let ws = self.inbox_target().ok_or("no business workspace is open to capture into")?;
        let (path, what) = match crate::inbox::read_clipboard()? {
            crate::inbox::Clip::Text(t) => (crate::inbox::save_text(&ws, &t, "the clipboard")?, "clipboard text"),
            crate::inbox::Clip::Png(bytes) => (crate::inbox::save_file(&ws, "clipboard image.png", &bytes, "the clipboard")?, "a clipboard image"),
        };
        self.inbox_saved(&ws, &path, what);
        Ok(())
    }

    /// The Inbox drop zone: text, or a file (base64).
    pub fn inbox_drop(self: &Arc<Self>, text: Option<&str>, file: Option<(&str, &str)>) -> Result<String, String> {
        use base64::Engine;
        let ws = self.inbox_target().ok_or("no business workspace is open")?;
        let (path, what) = match (text, file) {
            (Some(t), _) => (crate::inbox::save_text(&ws, t, "the inbox box")?, "pasted text".to_string()),
            (None, Some((name, b64))) => {
                let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| format!("bad file data: {e}"))?;
                if bytes.len() > 50 * 1024 * 1024 {
                    return Err("files over 50 MB aren't taken".into());
                }
                (crate::inbox::save_file(&ws, name, &bytes, "the inbox box")?, format!("a file ({name})"))
            }
            _ => return Err("nothing to save".into()),
        };
        self.inbox_saved(&ws, &path, &what);
        Ok(path.display().to_string())
    }

    // ---- notifications (notify.rs) ----

    /// Notify the user about a business event: a Windows toast, and a phone push if they set
    /// one up. Only events they chose; only a redacted summary line.
    pub fn notify(&self, ws: &str, event: &str, title: &str, body: &str) {
        let (on, phone) = {
            let st = self.store.lock().unwrap();
            let Some(n) = st.notes(ws).filter(|n| n.kind == "business") else { return };
            (!n.notify_set || n.notify_events.iter().any(|e| e == event), n.notify_phone.clone())
        };
        if !on {
            return;
        }
        let name = std::path::Path::new(ws).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let title = format!("{name}: {title}");
        crate::notify::toast(&title, body);
        if !phone.is_empty() {
            let names: Vec<String> = crate::notify::phone_secrets(&phone).iter().map(|s| s.to_string()).collect();
            match self.vault.get(ws, &names) {
                Ok(secrets) => crate::notify::push(&phone, &secrets, &title, body),
                Err(e) => self.hub.broadcast(json!({"kind": "toast", "text": format!("phone notification not sent: {e}")})),
            }
        }
    }

    /// Notification settings for the shown workspace (from the window only).
    pub fn set_notify(&self, events: Vec<String>, phone: &str) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("notifications are for business workspaces".into());
        }
        if !["", "ntfy", "telegram"].contains(&phone) {
            return Err("phone must be none, ntfy or telegram".into());
        }
        let known: Vec<&str> = crate::notify::EVENTS.iter().map(|(e, _)| *e).collect();
        let events: Vec<String> = events.into_iter().filter(|e| known.contains(&e.as_str())).collect();
        {
            let mut st = self.store.lock().unwrap();
            let n = st.notes_mut(&ws);
            n.notify_set = true;
            n.notify_events = events.clone();
            n.notify_phone = phone.to_string();
            st.save();
        }
        self.audit(&ws, &format!("NOTIFICATIONS SET events=[{}] phone={} by=user", events.join(","), if phone.is_empty() { "none" } else { phone }));
        self.touch();
        Ok(())
    }

    pub fn is_paused(&self, ws: &str) -> bool {
        self.store.lock().unwrap().notes(ws).is_some_and(|n| n.kind == "business" && n.paused)
    }

    /// Kill switch for the shown workspace (from the window only).
    pub fn set_outbound_paused(self: &Arc<Self>, paused: bool) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("only business workspaces have a kill switch".into());
        }
        {
            let mut st = self.store.lock().unwrap();
            st.notes_mut(&ws).outbound_paused = paused;
            st.save();
        }
        self.audit(&ws, if paused { "OUTBOUND PAUSED by=user (kill switch on)" } else { "OUTBOUND RESUMED by=user" });
        self.business_event(&ws, "pause_changed", None, json!({"target": "outbound", "paused": paused}));
        self.post_news(&orch_key(&ws), "@app", if paused {
            "The user PAUSED OUTBOUND (kill switch). Nothing may be executed, sent or published until they resume it; list_approved returns nothing meanwhile. Internal work may continue."
        } else {
            "The user resumed outbound: approved items are available in list_approved again."
        });
        self.touch();
        Ok(())
    }

    /// Kill switch for every business workspace (tray menu).
    pub fn set_outbound_paused_all(&self, paused: bool) {
        let business: Vec<String> = {
            let mut st = self.store.lock().unwrap();
            st.outbound_paused_all = paused;
            st.save();
            st.projects.iter().filter(|(_, n)| n.kind == "business").map(|(k, _)| k.clone()).collect()
        };
        for ws in business {
            self.audit(&ws, if paused { "OUTBOUND PAUSED (all workspaces) by=user from the tray" } else { "OUTBOUND RESUMED (all workspaces) by=user from the tray" });
        }
        self.touch();
    }

    pub fn outbound_paused_all(&self) -> bool {
        self.store.lock().unwrap().outbound_paused_all
    }

    /// Pause or resume the shown business workspace (from the window only). Pausing stops
    /// every running turn in it; agent-started work waits until it's resumed.
    pub fn set_workspace_paused(self: &Arc<Self>, paused: bool) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("only business workspaces can be paused".into());
        }
        {
            let mut st = self.store.lock().unwrap();
            st.notes_mut(&ws).paused = paused;
            st.save();
        }
        self.audit(&ws, if paused { "WORKSPACE PAUSED by=user (all turns stopped)" } else { "WORKSPACE RESUMED by=user" });
        self.business_event(&ws, "pause_changed", None, json!({"target": "workspace", "paused": paused}));
        let mut keys = self.thread_names_here(&ws);
        keys.push(orch_key(&ws));
        if paused {
            for k in &keys {
                self.resumes.lock().unwrap().remove(k);
                self.stop(k);
            }
        } else {
            // Messages that arrived while paused were kept; run them now.
            for k in &keys {
                self.kick(k);
            }
            self.post_news(&orch_key(&ws), "@app", "The user resumed this workspace after a pause. Check where things stood and carry on.");
        }
        self.touch();
        Ok(())
    }

    /// Business workspaces get the approval queue and the other business features.
    pub fn is_business(&self, ws: &str) -> bool {
        self.store.lock().unwrap().notes(ws).is_some_and(|n| n.kind == "business")
    }

    /// Set the shown workspace's type: "business" or "development".
    pub fn set_workspace_kind(&self, kind: &str) -> Result<(), String> {
        if !["business", "development"].contains(&kind) {
            return Err("kind must be business or development".into());
        }
        let ws = self.workspace();
        let mut st = self.store.lock().unwrap();
        st.notes_mut(&ws).kind = if kind == "business" { "business".into() } else { String::new() };
        st.save();
        drop(st);
        self.touch();
        Ok(())
    }

    /// queue_item: an agent proposes an outbound action. `attachments` are absolute paths.
    pub fn queue_add(&self, key: &str, args: &Value, attachments: Vec<String>) -> Result<String, String> {
        let ws = self.ws_of(key);
        if !self.is_business(&ws) {
            return Err("the approval queue is only on in business workspaces".into());
        }
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let kind = s("type");
        let summary = s("summary");
        let payload = match args.get("payload") {
            Some(Value::String(p)) => p.clone(),
            Some(v) if !v.is_null() => serde_json::to_string_pretty(v).unwrap_or_default(),
            _ => String::new(),
        };
        if kind.is_empty() || summary.is_empty() || payload.trim().is_empty() {
            return Err("type, summary and payload are required".into());
        }
        let risk = if s("risk").is_empty() { "medium".to_string() } else { s("risk").to_lowercase() };
        if !crate::queue::RISKS.contains(&risk.as_str()) {
            return Err("risk must be low, medium or high".into());
        }
        let who = if s("who_executes").is_empty() { "agent".to_string() } else { s("who_executes").to_lowercase() };
        if !["agent", "user"].contains(&who.as_str()) {
            return Err("who_executes must be agent or user".into());
        }
        // What the user decides from: one plain line, one suggestion, and why.
        let (tldr, recommend, why) = (s("tldr"), s("recommend").to_lowercase(), s("why"));
        if tldr.is_empty() || recommend.is_empty() || why.is_empty() {
            return Err("tldr (one plain line), recommend (approve, reject or edit) and why (one line) are required".into());
        }
        if !["approve", "reject", "edit"].contains(&recommend.as_str()) {
            return Err("recommend must be approve, reject or edit".into());
        }
        // The user does it by hand: precise steps, the same as request_user_action.
        let plan = if who == "user" {
            let mut a = args.clone();
            if a.get("goal").and_then(Value::as_str).is_none_or(|g| g.trim().is_empty()) {
                a["goal"] = json!(summary);
            }
            Some(crate::needs::parse_plan(&a).map_err(|e| format!("who_executes user needs a step-by-step plan: {e}"))?)
        } else {
            None
        };
        let minutes = args.get("minutes").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 600.0) as u32;
        let command = s("command");
        let secrets: Vec<String> = args.get("secrets").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        if !secrets.is_empty() && command.is_empty() {
            return Err("secrets are only passed to a command: add command (what run_approved will run)".into());
        }
        let known: Vec<String> = self.vault.list(&ws).into_iter().map(|(n, ..)| n).collect();
        if let Some(missing) = secrets.iter().find(|n| !known.contains(n)) {
            return Err(format!("no secret named {missing} in this workspace; the user adds secrets in Settings → Secrets"));
        }
        // Offers and orders commit usage: the app checks capacity itself.
        let (order_id, capacity) = if crate::capacity::COMMITTING.contains(&kind.as_str()) {
            let order_id = s("order_id");
            let est = args.get("est_units").and_then(Value::as_f64).unwrap_or(0.0);
            let deadline = args.get("deadline").and_then(parse_when).unwrap_or(0.0);
            if order_id.is_empty() || est <= 0.0 || deadline <= 0.0 {
                return Err(format!(
                    "a {kind} item needs order_id, est_units (weekly plan points the order will use; see usage_status) and deadline (\"YYYY-MM-DD HH:MM\" local time), so the app can check capacity"
                ));
            }
            (order_id.clone(), Some(self.capacity_check(&ws, &order_id, est, deadline)))
        } else {
            (s("order_id"), None)
        };
        for text in [&summary, &payload, &command, &tldr, &why] {
            if let Some(name) = crate::vault::contains_secret(text) {
                return Err(format!("the item contains the value of secret {name}: never put secret values in an item; name the secret in secrets and use $env:{name} in command"));
            }
        }
        let now_t = now();
        let mut st = self.store.lock().unwrap();
        let n = st.queue.iter().filter_map(|i| i.id.strip_prefix('q').and_then(|n| n.parse::<u64>().ok())).max().unwrap_or(0) + 1;
        let id = format!("q{n}");
        let line = s("line");
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M");
        let file = format!(
            "{stamp}-{}{}-{}-{id}.md",
            if line.is_empty() { String::new() } else { format!("{}-", crate::queue::slug(&line)) },
            crate::queue::slug(&kind),
            crate::queue::slug(&summary).chars().take(30).collect::<String>().trim_end_matches('-')
        );
        let item = crate::queue::QueueItem {
            id: id.clone(),
            workspace: ws.clone(),
            kind,
            line,
            summary,
            payload,
            expected_value_usd: args.get("expected_value_usd").and_then(Value::as_f64).unwrap_or(0.0),
            risk,
            who_executes: who,
            urgent: args.get("urgent").and_then(Value::as_bool).unwrap_or(false),
            attachments,
            command,
            secrets,
            order_id,
            capacity: capacity.clone(),
            hash_v: 3,
            tldr,
            recommend,
            why,
            minutes,
            plan,
            created_by: if key.starts_with(ORCHESTRATOR) { "orchestrator".into() } else { key.to_string() },
            created_at: now_t,
            status: "pending".into(),
            file,
            ..Default::default()
        };
        crate::queue::mirror(&item);
        let audit_line = format!("QUEUED {id} {} line={} by={} :: {}", item.kind, item.line, item.created_by, item.summary);
        let summary_for_notice = format!("{id} ({}, line {}): {}", item.kind, if item.line.is_empty() { "-" } else { &item.line }, item.summary);
        let urgent = item.urgent;
        let created = Self::feed_item(&item);
        st.queue.push(item);
        st.save();
        drop(st);
        self.business_event(&ws, "item_created", None, json!({"item": created}));
        self.audit(&ws, &audit_line);
        self.queue_changed(&ws);
        if urgent {
            crate::attention();
            self.notify(&ws, "urgent", "Urgent: waiting for your approval", &summary_for_notice);
        }
        let cap_note = match &capacity {
            Some(c) if c.ok => format!(" Capacity check passed: {}.", c.note),
            Some(c) => format!(" Capacity check FAILED: {}. The user sees this in red; the default is a polite decline or a later delivery date.", c.note),
            None => String::new(),
        };
        Ok(format!("queued {id} for the user's approval. It can't be executed until they approve it in the app; list_approved shows approved items.{cap_note}"))
    }

    /// list_approved: items the user approved that still match what they approved.
    pub fn queue_list_approved(&self, key: &str) -> Result<String, String> {
        let ws = self.ws_of(key);
        if !self.is_business(&ws) {
            return Err("the approval queue is only on in business workspaces".into());
        }
        let mut st = self.store.lock().unwrap();
        // An approved item that changed since is void for good (logged once).
        let changed: Vec<String> = crate::queue::still_approved(st.queue.iter().filter(|i| same_path(&i.workspace, &ws))).1.iter().map(|i| i.id.clone()).collect();
        let mut audit_lines = vec![];
        if !changed.is_empty() {
            for item in st.queue.iter_mut().filter(|i| changed.contains(&i.id) && same_path(&i.workspace, &ws)) {
                item.status = "void".into();
                crate::queue::mirror(item);
                audit_lines.push(format!("VOID {} (payload or an attachment changed after approval; it won't be executed)", item.id));
            }
            st.save();
        }
        let paused = st.outbound_paused_all || st.notes(&ws).is_some_and(|n| n.outbound_paused);
        let waiting = crate::queue::still_approved(st.queue.iter().filter(|i| same_path(&i.workspace, &ws))).0.len();
        let ok = crate::queue::listable(st.queue.iter().filter(|i| same_path(&i.workspace, &ws)), paused);
        let out = if paused {
            format!(
                "Outbound is PAUSED by the user (kill switch). Nothing may be executed, sent or published until they resume it; don't work around this.{}",
                if waiting > 0 { format!(" {waiting} approved item(s) are on hold.") } else { String::new() }
            )
        } else if ok.is_empty() {
            "no approved items waiting".to_string()
        } else {
            ok
            .iter()
            .map(|i| {
                format!(
                    "## {} ({}, line {}, executed by {}){}\nWhat: {}\nPayload (execute exactly this):\n{}{}{}",
                    i.id,
                    i.kind,
                    if i.line.is_empty() { "-" } else { &i.line },
                    i.who_executes,
                    if i.edited { " [the user edited the payload before approving]" } else { "" },
                    i.summary,
                    i.payload,
                    if i.attachments.is_empty() { String::new() } else { format!("\nAttachments: {}", i.attachments.join(", ")) },
                    if i.command.is_empty() { String::new() } else { format!("\nCommand (run it with run_approved; secrets: {}):\n{}", if i.secrets.is_empty() { "none".to_string() } else { i.secrets.join(", ") }, i.command) }
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
        };
        drop(st);
        for l in audit_lines {
            self.audit(&ws, &l);
        }
        Ok(out)
    }

    /// complete_item: record what happened when an approved item was executed.
    pub fn queue_complete(&self, key: &str, id: &str, result: &str) -> Result<String, String> {
        let ws = self.ws_of(key);
        let mut st = self.store.lock().unwrap();
        let item = st.queue.iter_mut().find(|i| i.id == id && same_path(&i.workspace, &ws)).ok_or(format!("no queue item {id} in this workspace"))?;
        crate::queue::complete(item, &crate::vault::redact(result), now())?;
        crate::queue::mirror(item);
        let audit_line = format!("EXECUTED {id} by={key} :: {}", item.result);
        st.save();
        drop(st);
        self.audit(&ws, &audit_line);
        self.business_event(&ws, "item_completed", None, json!({"item_id": id}));
        self.queue_changed(&ws);
        Ok(format!("{id} marked done"))
    }

    /// The user's decision from the Queue tab. Only called from the desktop window's IPC
    /// channel (see main.rs), never from HTTP or an agent tool.
    pub fn queue_decide(self: &Arc<Self>, id: &str, action: &str, reason: &str, payload: Option<&str>, force: bool, via: &str, seen_hash: Option<&str>) -> Result<(), String> {
        let decision = match action {
            "approve" => crate::queue::Decision::Approve(seen_hash.filter(|h| !h.is_empty()).map(str::to_string)),
            "edit_approve" => crate::queue::Decision::EditApprove(payload.unwrap_or("").to_string(), seen_hash.filter(|h| !h.is_empty()).map(str::to_string)),
            "reject" => crate::queue::Decision::Reject(reason.to_string()),
            _ => return Err(format!("unknown action {action}")),
        };
        let mut st = self.store.lock().unwrap();
        let item = st.queue.iter_mut().find(|i| i.id == id).ok_or(format!("no queue item {id}"))?;
        let over_capacity = item.capacity.as_ref().is_some_and(|c| !c.ok);
        if over_capacity && action != "reject" && !force {
            return Err(format!("{id} failed the capacity check: approving it anyway needs a second click"));
        }
        crate::queue::decide(item, decision, reason, now())?;
        crate::queue::mirror(item);
        let item = item.clone();
        st.save();
        drop(st);
        let what = match item.status.as_str() {
            "rejected" => "REJECTED",
            _ if over_capacity => "APPROVED DESPITE FAILED CAPACITY CHECK",
            _ if item.edited => "APPROVED after editing",
            _ => "APPROVED",
        };
        let via_note = if via == "window" { String::new() } else { format!(" via={via}") };
        self.audit(&item.workspace, &format!("{what} {} by=user{via_note}{}", item.id, if item.note.is_empty() { String::new() } else { format!(" :: {}", item.note) }));
        let ws = item.workspace.clone();
        self.business_event(&ws, "item_decided", None, json!({"item_id": item.id, "status": item.status, "via": via}));
        if item.status == "approved" && (item.kind == "new_venture" || item.kind == "line_status") {
            self.apply_line_item(&item);
        }
        self.queue_news(&ws, &[item]);
        self.queue_changed(&ws);
        Ok(())
    }

    /// "Approve all low-risk" in the shown workspace.
    /// `shown`: the (id, hash) of each low-risk card the user saw; only those are approved,
    /// and only if they still match.
    pub fn queue_approve_low_risk(self: &Arc<Self>, shown: &[(String, String)]) -> Result<usize, String> {
        let ws = self.workspace();
        let mut st = self.store.lock().unwrap();
        let mut done = vec![];
        for item in st.queue.iter_mut().filter(|i| same_path(&i.workspace, &ws) && i.status == "pending" && i.risk == "low" && !i.capacity.as_ref().is_some_and(|c| !c.ok) && !crate::queue::is_manual(i)) {
            let Some((_, h)) = shown.iter().find(|(i, _)| *i == item.id) else { continue };
            if crate::queue::decide(item, crate::queue::Decision::Approve(Some(h.clone())), "", now()).is_err() {
                continue; // changed since the user saw it: left pending
            }
            crate::queue::mirror(item);
            done.push(item.clone());
        }
        st.save();
        drop(st);
        for i in &done {
            self.audit(&ws, &format!("APPROVED {} by=user (approve all low-risk)", i.id));
            self.business_event(&ws, "item_decided", None, json!({"item_id": i.id, "status": i.status, "via": "window"}));
        }
        if !done.is_empty() {
            self.queue_news(&ws, &done);
            self.queue_changed(&ws);
        }
        Ok(done.len())
    }

    /// Tell the workspace's orchestrator what the user decided, so it can act on approvals
    /// and learn from rejections.
    fn queue_news(self: &Arc<Self>, ws: &str, items: &[crate::queue::QueueItem]) {
        let lines: Vec<String> = items
            .iter()
            .map(|i| {
                let head = format!("{} ({}, line {}): {}", i.id, i.kind, if i.line.is_empty() { "-" } else { &i.line }, i.summary);
                match i.status.as_str() {
                    "rejected" => format!("The user REJECTED {head}\nReason: {}\nRead the reason, and save a lesson if it shows a pattern.", i.note),
                    _ if i.who_executes == "user" => format!(
                        "The user APPROVED {head}{}\nThey'll do this one by hand; record the result with complete_item once they tell you.",
                        if i.edited { " (after editing the payload)" } else { "" }
                    ),
                    _ => format!(
                        "The user APPROVED {head}{}{}\nIt's now in list_approved; execute exactly that payload, then complete_item.",
                        if i.edited { " (after editing the payload: use the edited version)" } else { "" },
                        if i.note.is_empty() { String::new() } else { format!("\nTheir note: {}", i.note) }
                    ),
                }
            })
            .collect();
        self.post_news(&orch_key(ws), "@approval queue", &lines.join("\n\n"));
    }

    /// Queue items for the Queue tab: pending first, then the latest decided ones.
    pub fn queue_json(&self, ws: &str) -> Value {
        let st = self.store.lock().unwrap();
        let mut items: Vec<&crate::queue::QueueItem> = st.queue.iter().filter(|i| same_path(&i.workspace, ws)).collect();
        items.sort_by(|a, b| b.created_at.total_cmp(&a.created_at));
        let (pending, decided): (Vec<_>, Vec<_>) = items.into_iter().partition(|i| i.status == "pending");
        let pending: Vec<Value> = pending
            .into_iter()
            .map(|i| {
                let mut v = json!(i);
                // The hash the card shows: approving sends it back, so the approval covers
                // exactly what the user saw.
                v["payload_hash"] = json!(crate::queue::content_hash(i));
                if crate::queue::is_manual(i) {
                    v["manual"] = json!(true);
                    v["dropped"] = json!(Self::queue_dropped(i));
                    v["drop_dir"] = json!(crate::queue::drop_dir(i));
                }
                v
            })
            .collect();
        let mut decided = decided;
        decided.sort_by(|a, b| b.decided_at.max(b.done_at).total_cmp(&a.decided_at.max(a.done_at)));
        decided.truncate(40);
        // An approved item that no longer matches its approval can't be executed.
        let decided: Vec<Value> = decided
            .into_iter()
            .map(|i| {
                let mut v = json!(i);
                if i.status == "approved" && crate::queue::content_hash(i) != i.approved_hash {
                    v["void"] = json!(true);
                }
                v
            })
            .collect();
        json!({"pending": pending, "decided": decided})
    }

    pub fn queue_pending(&self, ws: &str) -> usize {
        self.store.lock().unwrap().queue.iter().filter(|i| i.status == "pending" && same_path(&i.workspace, ws)).count()
    }

    fn queue_changed(&self, ws: &str) {
        let total = self.store.lock().unwrap().queue.iter().filter(|i| i.status == "pending").count();
        crate::set_badge(total);
        // Decisions and manual tasks in Needs you are read from the queue.
        self.needs_changed(ws);
    }
}

/// A time given as Unix seconds, "YYYY-MM-DD HH:MM" or "YYYY-MM-DD" (local, end of day).
pub fn parse_when(v: &Value) -> Option<f64> {
    use chrono::TimeZone;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    let s = v.as_str()?.trim();
    let dt = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|d| d.and_hms_opt(23, 59, 0).unwrap()))
        .ok()?;
    chrono::Local.from_local_datetime(&dt).earliest().map(|d| d.timestamp() as f64)
}

/// A usage reading from Claude Code's rate-limit info.
fn sample_of(info: &Value) -> Option<crate::capacity::Sample> {
    let w = info.get("unifiedWindows")?;
    let util = |k: &str| w.pointer(&format!("/{k}/utilization")).and_then(Value::as_f64);
    Some(crate::capacity::Sample {
        ts: info.get("seen_at").and_then(Value::as_f64).unwrap_or_else(now),
        five: util("five_hour").unwrap_or(0.0),
        week: util("seven_day")?,
        week_resets_at: w.pointer("/seven_day/resetsAt").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

/// Size limits (characters) for what the orchestrator reads on every turn. Saves over the
/// limit are refused, and existing ones over it get a housekeeping note.
pub const LIMIT_BRIEF: usize = 5000;
pub const LIMIT_RULES: usize = 5000;
pub const LIMIT_TUNING: usize = 3000;
pub const LIMIT_LESSONS: usize = 5000;
pub const LIMIT_PROJECT_LESSONS: usize = 4000;
pub const LIMIT_DEV_LESSONS: usize = 4000;
pub const LIMIT_BUSINESS_LESSONS: usize = 4000;

/// The lists save_lessons may write (None leaves a list as it is).
#[derive(Default)]
pub struct Lessons<'a> {
    pub general: Option<&'a str>,
    pub dev: Option<&'a str>,
    pub business: Option<&'a str>,
    /// (project folder, its lessons)
    pub project: Option<(&'a str, &'a str)>,
}

/// A lesson list's lines, trimmed, without blank ones.
pub fn lesson_lines(s: &str) -> Vec<String> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

fn folder_name(p: &str) -> String {
    std::path::Path::new(p.trim_end_matches(['\\', '/'])).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string())
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

/// Whether a user message reads like a correction of the orchestrator.
fn looks_like_correction(text: &str) -> bool {
    let t = text.to_lowercase().replace('\u{2019}', "").replace('\'', "");
    let t = t.trim();
    const PHRASES: &[&str] = &[
        "i told you", "i said", "like i said", "as i said", "i already", "i meant", "not what i",
        "thats not", "that is not", "thats what i", "you didnt", "you did not", "why didnt you",
        "why did you", "why are you", "you forgot", "you were supposed", "you keep", "you still",
        "you never", "i asked you", "i asked for", "didnt i", "you misunderstood", "stop doing",
        "dont do that", "wrong", "not like that",
    ];
    t.starts_with("no ") || t.starts_with("no,") || t.starts_with("nope") || t == "no"
        || PHRASES.iter().any(|p| t.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every development-only line must exist in the shared text (or the swap would silently
    /// do nothing), and a business workspace must get none of them.
    #[test]
    fn business_instructions_swap_every_dev_line() {
        let biz = business_append();
        for (dev, replacement) in BUSINESS_SWAPS {
            assert_eq!(ORCH_APPEND.matches(dev).count(), 1, "not found in ORCH_APPEND: {dev}");
            assert!(!biz.contains(dev), "business text still has: {dev}");
            assert!(biz.contains(replacement));
        }
        assert!(biz.contains("# Running the business"));
        // The shared parts are identical: the business text is the dev text with only the swaps.
        let mut back = biz.trim_end_matches(BUSINESS_MODE).to_string();
        for (dev, replacement) in BUSINESS_SWAPS {
            back = back.replace(replacement, dev);
        }
        assert_eq!(back, ORCH_APPEND);
    }
}

#[cfg(test)]
mod phone_decide_tests {
    use super::*;
    use crate::phone::tests::{sign, test_phone};

    /// A real App on a temp business workspace with the phone feed and decisions on. The
    /// workspace is paused so the orchestrator news a decision sends never starts a turn.
    fn app(name: &str) -> (Arc<App>, String, String) {
        let root = std::env::temp_dir().join(format!("orch-phone-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let ws = ws.to_string_lossy().to_string();
        let state = json!({
            "workspace": ws, "open_workspaces": [ws],
            "projects": {ws.clone(): {"kind": "business", "phone_feed": true, "phone_decide": true, "paused": true}},
        });
        std::fs::write(root.join("state.json"), state.to_string()).unwrap();
        let app = App::new(Paths { config_dir: root.clone(), data_dir: root.clone() }, "t".repeat(40));
        let id = orch_key(&ws);
        (app, ws, id)
    }

    fn queue(app: &Arc<App>, ws: &str, summary: &str) -> String {
        let out = app.queue_add(&orch_key(ws), &json!({"type": "post_pin", "line": "C", "summary": summary, "payload": "board: tests", "risk": "low",
            "tldr": summary, "recommend": "approve", "why": "tests"}), vec![]).unwrap();
        out.split_whitespace().nth(1).unwrap().to_string()
    }

    fn signed(sk: &p256::ecdsa::SigningKey, ws_id: &str, item: &str, hash: &str, nonce: &str) -> crate::phone::DecideRequest {
        let ts = now() as i64;
        crate::phone::DecideRequest {
            item_id: item.into(), decision: "approve".into(), reason: String::new(), device: "iphone-1".into(), ts, nonce: nonce.into(),
            sig: sign(sk, &crate::phone::decide_message("iphone-1", ws_id, item, "approve", hash, "", ts, nonce)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_valid_signature_decides_like_a_window_click() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, ws, id) = app("valid");
        let (sk, phone) = test_phone("iphone-1", 7);
        let fp = phone.fingerprint.clone();
        app.store.lock().unwrap().phones.push(phone);
        let by_phone = queue(&app, &ws, "pin one");
        let by_click = queue(&app, &ws, "pin two");
        let hash = |i: &str| app.store.lock().unwrap().queue.iter().find(|x| x.id == i).map(crate::queue::content_hash).unwrap();
        let req = signed(&sk, &id, &by_phone, &hash(&by_phone), "nonce-valid-0123456789");
        let out = app.phone_decide(&id, &req).unwrap();
        assert_eq!(out["status"], "approved");
        let h = hash(&by_click);
        app.queue_decide(&by_click, "approve", "", None, false, "window", Some(&h)).unwrap();
        let st = app.store.lock().unwrap();
        for i in [&by_phone, &by_click] {
            let item = st.queue.iter().find(|x| x.id == *i).unwrap();
            assert_eq!(item.status, "approved");
            assert_eq!(item.approved_hash, crate::queue::content_hash(item), "same approval as a click");
        }
        drop(st);
        let audit = std::fs::read_to_string(std::path::Path::new(&ws).join("queue/audit.log")).unwrap();
        assert!(audit.contains(&format!("APPROVED {by_phone} by=user via=phone:{fp}")), "{audit}");
        assert!(audit.contains(&format!("APPROVED {by_click} by=user ")) || audit.contains(&format!("APPROVED {by_click} by=user  #")), "{audit}");
        // The same signed request again is a replay.
        assert_eq!(app.phone_decide(&id, &req).unwrap_err().1, "replayed_nonce");
        // The instance token alone (no signature) never decides.
        let mut unsigned = signed(&sk, &id, &by_phone, &hash(&by_phone), "nonce-unsigned-012345678");
        unsigned.sig.clear();
        assert_eq!(app.phone_decide(&id, &unsigned).unwrap_err().1, "signature_required");
    }

    #[tokio::test]
    async fn kill_switch_on_approval_records_but_nothing_is_listed() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, ws, id) = app("killswitch");
        let (sk, phone) = test_phone("iphone-1", 7);
        app.store.lock().unwrap().phones.push(phone);
        app.store.lock().unwrap().notes_mut(&ws).outbound_paused = true;
        let item = queue(&app, &ws, "pin while paused");
        let hash = app.store.lock().unwrap().queue.iter().find(|x| x.id == item).map(crate::queue::content_hash).unwrap();
        let out = app.phone_decide(&id, &signed(&sk, &id, &item, &hash, "nonce-paused-0123456789")).unwrap();
        assert_eq!(out["status"], "approved", "the decision is recorded");
        let listed = app.queue_list_approved(&orch_key(&ws)).unwrap();
        assert!(listed.contains("PAUSED"), "{listed}");
        assert!(!listed.contains(&item), "nothing is handed out while outbound is paused");
    }

    #[tokio::test]
    async fn decisions_need_the_workspace_setting() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, ws, id) = app("off");
        let (sk, phone) = test_phone("iphone-1", 7);
        app.store.lock().unwrap().phones.push(phone);
        app.store.lock().unwrap().notes_mut(&ws).phone_decide = false;
        let item = queue(&app, &ws, "pin");
        let hash = app.store.lock().unwrap().queue.iter().find(|x| x.id == item).map(crate::queue::content_hash).unwrap();
        assert_eq!(app.phone_decide(&id, &signed(&sk, &id, &item, &hash, "nonce-off-0123456789ab")).unwrap_err().1, "decisions_off");
        app.store.lock().unwrap().notes_mut(&ws).phone_feed = false;
        assert_eq!(app.phone_decide(&id, &signed(&sk, &id, &item, &hash, "nonce-off2-0123456789a")).unwrap_err().1, "no_feed");
    }

    fn signed_with(sk: &p256::ecdsa::SigningKey, msg: impl Fn(i64) -> String, nonce: &str) -> crate::phone::Signed {
        let ts = now() as i64;
        crate::phone::Signed { device: "iphone-1".into(), ts, nonce: nonce.into(), sig: sign(sk, &msg(ts)) }
    }

    #[tokio::test]
    async fn a_signed_revision_approves_the_revised_payload() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, ws, id) = app("revise");
        let (sk, phone) = test_phone("iphone-1", 7);
        app.store.lock().unwrap().phones.push(phone);
        let item = queue(&app, &ws, "pin to revise");
        let rev = app.phone_revise(&id, &item, "board: revised").unwrap();
        // A draft changes nothing.
        let original = app.store.lock().unwrap().queue.iter().find(|x| x.id == item).cloned().unwrap();
        assert_eq!(original.payload, "board: tests");
        assert_eq!(original.status, "pending");
        // Signing the original hash with the revision named fails; the revision's hash passes.
        let rid = rev["revision"].as_str().unwrap().to_string();
        let mut wrong = signed(&sk, &id, &item, &crate::queue::content_hash(&original), "nonce-rev-wrong-01234567");
        wrong.revision = rid.clone();
        assert!(app.phone_decide(&id, &wrong).is_err());
        let mut req = signed(&sk, &id, &item, rev["payload_hash"].as_str().unwrap(), "nonce-rev-good-012345678");
        req.revision = rid.clone();
        app.phone_decide(&id, &req).unwrap();
        let now_item = app.store.lock().unwrap().queue.iter().find(|x| x.id == item).cloned().unwrap();
        assert_eq!(now_item.status, "approved");
        assert_eq!(now_item.payload, "board: revised");
        assert!(now_item.edited);
        assert_eq!(now_item.approved_hash, rev["payload_hash"].as_str().unwrap());
        let audit = std::fs::read_to_string(std::path::Path::new(&ws).join("queue/audit.log")).unwrap();
        assert!(audit.contains(&format!("revision={rid}")), "{audit}");
        // No revising once decided, and no unknown revisions.
        assert_eq!(app.phone_revise(&id, &item, "board: again").unwrap_err().1, "not_pending");
        let other = queue(&app, &ws, "another");
        let mut ghost = signed(&sk, &id, &other, "x", "nonce-rev-ghost-01234567");
        ghost.revision = "r9".into();
        assert_eq!(app.phone_decide(&id, &ghost).unwrap_err().1, "no_revision");
    }

    #[tokio::test]
    async fn pause_needs_no_signature_but_unpause_does() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, ws, id) = app("pause");
        let (sk, phone) = test_phone("iphone-1", 7);
        app.store.lock().unwrap().phones.push(phone);
        app.phone_pause(&id, "outbound").unwrap();
        assert!(app.store.lock().unwrap().notes(&ws).unwrap().outbound_paused);
        assert_eq!(app.phone_pause(&id, "everything").unwrap_err().1, "bad_request");
        let unsigned = crate::phone::Signed { device: "iphone-1".into(), ts: now() as i64, nonce: "nonce-unp-unsigned-0123".into(), sig: String::new() };
        assert_eq!(app.phone_unpause(&id, "outbound", &unsigned).unwrap_err().1, "signature_required");
        // A signature for the other target doesn't carry over.
        let other = signed_with(&sk, |ts| crate::phone::unpause_message("iphone-1", &id, "workspace", ts, "nonce-unp-other-0123456"), "nonce-unp-other-0123456");
        assert!(app.phone_unpause(&id, "outbound", &other).is_err());
        assert!(app.store.lock().unwrap().notes(&ws).unwrap().outbound_paused);
        let good = signed_with(&sk, |ts| crate::phone::unpause_message("iphone-1", &id, "outbound", ts, "nonce-unp-good-01234567"), "nonce-unp-good-01234567");
        app.phone_unpause(&id, "outbound", &good).unwrap();
        assert!(!app.store.lock().unwrap().notes(&ws).unwrap().outbound_paused);
        assert_eq!(app.phone_unpause(&id, "outbound", &good).unwrap_err().1, "replayed_nonce");
        let audit = std::fs::read_to_string(std::path::Path::new(&ws).join("queue/audit.log")).unwrap();
        assert!(audit.contains("OUTBOUND PAUSED via=phone api") && audit.contains("OUTBOUND RESUMED by=user via=phone:"), "{audit}");
    }

    #[tokio::test]
    async fn done_is_bound_to_the_exact_files() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, ws, id) = app("done");
        let (sk, phone) = test_phone("iphone-1", 7);
        app.store.lock().unwrap().phones.push(phone);
        let out = app.queue_add(&orch_key(&ws), &json!({"type": "post_pin", "line": "C", "summary": "upload pins by hand", "payload": "board: tests", "risk": "low", "who_executes": "user",
            "tldr": "Upload the pins", "recommend": "approve", "why": "tests", "done_when": "The pins show on the board",
            "steps": [{"do": "Open the board", "expect": "The board page"}]}), vec![]).unwrap();
        let item = out.split_whitespace().nth(1).unwrap().to_string();
        use base64::Engine;
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        assert_eq!(app.phone_manual_file(&id, &item, "notes.exe", &b64(b"x")).unwrap_err().1, "bad_type");
        app.phone_manual_file(&id, &item, "shot.png", &b64(b"first")).unwrap();
        let files = vec![("shot.png".to_string(), format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"first")))];
        let fh = crate::phone::files_hash(&files);
        let s = signed_with(&sk, |ts| crate::phone::done_message("iphone-1", &id, &item, &fh, ts, "nonce-done-a-0123456789"), "nonce-done-a-0123456789");
        // A second file arrives after the phone signed: the signed list no longer matches.
        app.phone_manual_file(&id, &item, "more.jpg", &b64(b"second")).unwrap();
        assert!(app.phone_manual_done(&id, &item, &s).is_err());
        assert_eq!(app.store.lock().unwrap().queue.iter().find(|x| x.id == item).unwrap().status, "pending");
        std::fs::remove_file(std::path::Path::new(&ws).join("inbox/images").join(&item).join("more.jpg")).unwrap();
        let s2 = signed_with(&sk, |ts| crate::phone::done_message("iphone-1", &id, &item, &fh, ts, "nonce-done-b-0123456789"), "nonce-done-b-0123456789");
        app.phone_manual_done(&id, &item, &s2).unwrap();
        assert_eq!(app.store.lock().unwrap().queue.iter().find(|x| x.id == item).unwrap().status, "done");
    }
}

#[cfg(test)]
mod lesson_scope_tests {
    use super::*;
    use crate::config::Paths;

    /// A development workspace, a business one and a third project, with old unsorted lessons.
    fn app(name: &str) -> (Arc<App>, String, String, String, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("orch-lessons-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |n: &str| { let p = root.join(n); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() };
        let (dev, biz, other) = (mk("dev"), mk("biz"), mk("atlas"));
        let state = json!({
            "workspace": dev, "open_workspaces": [dev, biz],
            "lessons": "- ABOUT-ME talks in shorthand\n- DEV-PRACTICE opus reviews every slice\n- ATLAS-BOX deploy to the box",
            "lessons_split": true,
            "dev_lessons": "", "business_lessons": "BIZ-PRACTICE keep costs down",
            "projects": {dev.clone(): {}, biz.clone(): {"kind": "business"}, other.clone(): {}},
        });
        std::fs::write(root.join("state.json"), state.to_string()).unwrap();
        let app = App::new(Paths { config_dir: root.clone(), data_dir: root.clone() }, "t".repeat(40));
        (app, dev, biz, other, root)
    }

    #[tokio::test]
    async fn each_type_gets_only_its_own_list() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, dev, biz, _, _) = app("types");
        {
            let mut st = app.store.lock().unwrap();
            st.lessons_scoped = true;
            st.dev_lessons = "DEV-ONLY run the tests".into();
        }
        let d = app.orchestrator_prompt(&dev);
        let b = app.orchestrator_prompt(&biz);
        assert!(d.contains("ABOUT-ME") && b.contains("ABOUT-ME"), "lessons about the user reach both");
        assert!(d.contains("DEV-ONLY") && !d.contains("BIZ-PRACTICE"), "a development prompt has no business_lessons");
        assert!(b.contains("BIZ-PRACTICE") && !b.contains("DEV-ONLY"), "a business prompt has no dev_lessons");
        assert!(d.contains("1. the user's newer words; 2. project rules"), "the order of precedence is stated");
    }

    #[tokio::test]
    async fn the_override_narrows_what_reaches_a_workspace() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, _, biz, _, _) = app("override");
        app.store.lock().unwrap().lessons_scoped = true;
        app.store.lock().unwrap().notes_mut(&biz).lessons = "PROJECT-OWN".into();
        app.store.lock().unwrap().notes_mut(&biz).lesson_scope = "about_me".into();
        let p = app.orchestrator_prompt(&biz);
        assert!(p.contains("ABOUT-ME") && !p.contains("BIZ-PRACTICE") && p.contains("PROJECT-OWN"));
        app.store.lock().unwrap().notes_mut(&biz).lesson_scope = "none".into();
        let p = app.orchestrator_prompt(&biz);
        assert!(!p.contains("ABOUT-ME") && !p.contains("BIZ-PRACTICE") && p.contains("PROJECT-OWN"), "project lessons still apply");
        assert!(app.set_lesson_scope("everything").is_err());
    }

    #[tokio::test]
    async fn a_business_workspace_cant_write_dev_lessons_or_run_the_sort() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, dev, biz, _, _) = app("biz");
        let k = orch_key(&biz);
        assert!(app.save_lessons(&k, Some(&biz), Lessons { dev: Some("x"), ..Default::default() }).unwrap_err().contains("development workspaces"));
        assert!(app.save_lessons(&orch_key(&dev), Some(&dev), Lessons { business: Some("x"), ..Default::default() }).is_err());
        // The unsorted lessons reach the business workspace marked as such, and it's never asked to sort.
        assert!(app.orchestrator_prompt(&biz).contains("Not sorted yet"));
        assert!(app.housekeeping_note(&biz).is_none());
        app.save_lessons(&k, Some(&biz), Lessons { business: Some("BIZ-PRACTICE keep costs down\nBIZ-2"), ..Default::default() }).unwrap();
        assert!(!app.store.lock().unwrap().lessons_scoped);
    }

    #[tokio::test]
    async fn the_sort_keeps_every_lesson_and_backs_up_first() {
        let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (app, dev, _, atlas, root) = app("sort");
        let note = app.housekeeping_note(&dev).expect("a development workspace is asked to sort");
        assert!(note.contains("dev_lessons") && note.contains(&atlas));
        assert!(note.contains(&format!("Current project_lessons of {atlas}:\n(empty)")), "{note}");
        let backup = std::fs::read_to_string(root.join("notes-backup-before-lesson-sort.json")).expect("backup written first");
        assert!(backup.contains("ATLAS-BOX"));
        let k = orch_key(&dev);
        // A project's lesson moves first.
        app.save_lessons(&k, Some(&dev), Lessons { project: Some((atlas.as_str(), "- ATLAS-BOX deploy to the box")), ..Default::default() }).unwrap();
        // Dropping (or rewording) a line is refused, and nothing is saved.
        let e = app.save_lessons(&k, Some(&dev), Lessons { general: Some("- ABOUT-ME talks in shorthand"), dev: Some("- DEV-PRACTICE opus reviews"), ..Default::default() }).unwrap_err();
        assert!(e.contains("DEV-PRACTICE opus reviews every slice"), "{e}");
        assert!(app.store.lock().unwrap().dev_lessons.is_empty());
        let out = app.save_lessons(&k, Some(&dev), Lessons {
            general: Some("- ABOUT-ME talks in shorthand"), dev: Some("- DEV-PRACTICE opus reviews every slice"), ..Default::default()
        }).unwrap();
        assert!(out.contains("Before: 3 shared lessons. After: 1 about you, 1 for development work, 1 in project lessons (atlas 1)."), "{out}");
        let st = app.store.lock().unwrap();
        assert!(st.lessons_scoped && st.lesson_sort_before.is_empty());
        let after = lesson_lines(&st.lessons).len() + lesson_lines(&st.dev_lessons).len() + lesson_lines(&st.notes(&atlas).unwrap().lessons).len();
        assert_eq!(after, 3, "count before = sum of counts after");
        drop(st);
        assert!(app.housekeeping_note(&dev).is_none(), "asked once");
        let log = std::fs::read_to_string(app.hub.path(&k)).unwrap();
        assert!(log.contains("lessons_sorted") && log.contains("ATLAS-BOX"), "the user sees the moves");
    }
}
