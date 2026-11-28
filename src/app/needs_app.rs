//! "Needs you", wired into the app: stored entries (questions, action steps, heads-ups),
//! entries read from the queue (decisions, manual tasks), the user's marks, the mirror,
//! the feed and notifications. The data model is in needs.rs.

use super::{orch_key, App, ORCHESTRATOR};
use crate::needs::{self, Mark, Need, View};
use crate::store::{now, same_path};
use serde_json::{json, Value};
use std::sync::Arc;

/// Closed entries are kept this long (Done today, and what agents dismissed).
const KEEP_CLOSED_S: f64 = 14.0 * 86400.0;

/// One workspace's list, as the window, the feed and the mirror see it.
#[derive(Default)]
pub struct WsNeeds {
    pub open: Vec<View>,
    pub today: Vec<String>,
    pub snoozed: Vec<View>,
    pub done_today: Vec<View>,
}

impl WsNeeds {
    pub fn minutes(&self) -> u32 {
        self.open.iter().map(|v| v.minutes).sum()
    }
    pub fn urgent(&self) -> usize {
        self.open.iter().filter(|v| v.priority == "urgent").count()
    }
}

fn first_line(s: &str, max: usize) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if l.chars().count() > max { l.chars().take(max - 1).collect::<String>() + "…" } else { l.to_string() }
}

fn view_of_need(n: &Need, mark: &Mark, waiting: bool, t: f64) -> View {
    let snoozed = n.status == "open" && mark.snooze_until > t;
    View {
        id: n.id.clone(),
        ws: n.workspace.clone(),
        kind: n.kind.clone(),
        title: n.title.clone(),
        tldr: n.tldr.clone(),
        details: n.details.clone(),
        why: n.why.clone(),
        recommend: n.recommend.clone(),
        minutes: n.minutes,
        priority: n.priority.clone(),
        source: n.source.clone(),
        created_at: n.created_at,
        msg_ts: n.msg_ts,
        status: if snoozed { "snoozed".into() } else { n.status.clone() },
        snoozed_until: if snoozed { mark.snooze_until } else { 0.0 },
        waiting: n.kind == "question" && n.status == "open" && waiting,
        // A question nobody waits for any more (it timed out, or the app restarted) went
        // ahead with its default.
        timed_out: n.kind == "question" && n.status == "open" && (n.timed_out || !waiting),
        default_answer: n.default_answer.clone(),
        questions: n.questions.clone(),
        steps: n.steps.clone(),
        link: n.link.clone(),
        plan: n.plan.clone(),
        step: mark.step,
        queue_item: String::new(),
        queue: Value::Null,
        careful: false,
        pin: mark.pin,
        agent_today: mark.today,
        note: n.note.clone(),
        closed_by: n.closed_by.clone(),
        closed_at: n.closed_at,
        answer: n.answer.clone(),
    }
}

/// High risk, spending or a failed capacity check: never a one-key approve.
fn careful(i: &crate::queue::QueueItem) -> bool {
    i.risk == "high" || i.kind.contains("spend") || i.capacity.as_ref().is_some_and(|c| !c.ok)
}

fn view_of_item(i: &crate::queue::QueueItem, item_json: Value, mark: &Mark, t: f64) -> View {
    let manual = i.status == "approved" && crate::queue::is_manual(i);
    let kind = if manual { "manual" } else { "decision" };
    let open = i.status == "pending" || manual;
    let snoozed = open && mark.snooze_until > t;
    let status = if snoozed { "snoozed" } else if open { "open" } else { "done" };
    View {
        id: format!("{}:{}", if manual { "manual" } else { "queue" }, i.id),
        ws: i.workspace.clone(),
        kind: kind.into(),
        title: i.summary.clone(),
        tldr: if i.tldr.is_empty() { first_line(&i.summary, 140) } else { i.tldr.clone() },
        details: i.payload.clone(),
        why: i.why.clone(),
        recommend: if manual { "do".into() } else { i.recommend.clone() },
        minutes: if i.minutes > 0 { i.minutes } else if manual { i.plan.as_ref().map_or(5, |p| p.steps.len().max(1) as u32) } else { 1 },
        priority: if i.urgent { "urgent".into() } else { "today".into() },
        source: if i.created_by == "orchestrator" || i.created_by.is_empty() { orch_key(&i.workspace) } else { i.created_by.clone() },
        created_at: if manual && i.decided_at > 0.0 { i.decided_at } else { i.created_at },
        status: status.into(),
        snoozed_until: if snoozed { mark.snooze_until } else { 0.0 },
        plan: i.plan.clone(),
        step: mark.step,
        queue_item: i.id.clone(),
        queue: item_json,
        careful: careful(i),
        pin: mark.pin,
        agent_today: mark.today,
        note: if open { String::new() } else { i.status.clone() },
        closed_by: if open { String::new() } else { "user".into() },
        closed_at: if open { 0.0 } else { i.done_at.max(i.decided_at) },
        ..Default::default()
    }
}

/// The ask_user arguments' questions as one line for the list.
fn question_title(questions: &Value) -> String {
    questions.pointer("/0/question").and_then(Value::as_str).map(|q| first_line(q, 140)).unwrap_or_else(|| "A question".into())
}

impl App {
    /// The workspace's entries, computed fresh (decisions and manual tasks from the queue).
    pub fn needs_of(&self, ws: &str) -> WsNeeds {
        self.needs_of_with(ws, None)
    }

    /// `pending`: the Queue tab's pending items for `ws`, if already computed (they hash
    /// every attachment, so the state push computes them once).
    fn needs_of_with(&self, ws: &str, pending: Option<&Value>) -> WsNeeds {
        let t = now();
        let today = chrono::Local::now();
        let waiting: Vec<String> = self.questions.lock().unwrap().keys().cloned().collect();
        let (stored, items, marks) = {
            let st = self.store.lock().unwrap();
            let stored: Vec<Need> = st.needs.iter().filter(|n| same_path(&n.workspace, ws)).cloned().collect();
            let items: Vec<crate::queue::QueueItem> = st.queue.iter().filter(|i| same_path(&i.workspace, ws)).cloned().collect();
            (stored, items, st.need_marks.clone())
        };
        let mark = |id: &str| marks.get(id).cloned().unwrap_or_default();
        let mut all: Vec<View> = stored.iter().map(|n| view_of_need(n, &mark(&n.id), waiting.contains(&n.ref_id), t)).collect();
        let has_items = items.iter().any(|i| i.status == "pending");
        let pending_json = match pending {
            Some(p) => p.as_array().cloned().unwrap_or_default(),
            None if has_items => self.queue_json(ws)["pending"].as_array().cloned().unwrap_or_default(),
            None => vec![],
        };
        for i in &items {
            let manual = i.status == "approved" && crate::queue::is_manual(i);
            let decided_today = i.status != "pending" && needs::same_day(i.done_at.max(i.decided_at), today);
            if i.status != "pending" && !manual && !decided_today {
                continue;
            }
            let item_json = match pending_json.iter().find(|v| v["id"] == json!(i.id)) {
                Some(v) => v.clone(),
                None => {
                    let mut v = json!(i);
                    v["payload_hash"] = json!(crate::queue::content_hash(i));
                    if crate::queue::is_manual(i) {
                        v["manual"] = json!(true);
                        v["dropped"] = json!(Self::queue_dropped(i));
                        v["drop_dir"] = json!(crate::queue::drop_dir(i));
                    }
                    v
                }
            };
            let id = format!("{}:{}", if manual { "manual" } else { "queue" }, i.id);
            all.push(view_of_item(i, item_json, &mark(&id), t));
        }
        let mut out = WsNeeds::default();
        for v in all {
            match v.status.as_str() {
                "open" => out.open.push(v),
                "snoozed" => out.snoozed.push(v),
                _ if needs::same_day(v.closed_at, today) => out.done_today.push(v),
                _ => {}
            }
        }
        needs::sort(&mut out.open);
        needs::sort(&mut out.snoozed);
        out.done_today.sort_by(|a, b| b.closed_at.total_cmp(&a.closed_at));
        out.today = needs::today(&out.open);
        out
    }

    pub fn needs_json(&self, ws: &str) -> Value {
        Self::ws_needs_json(ws, &self.needs_of(ws))
    }

    fn ws_needs_json(ws: &str, n: &WsNeeds) -> Value {
        json!({
            "ws": ws, "count": n.open.len(), "urgent": n.urgent(), "minutes": n.minutes(),
            "open": n.open, "today": n.today, "snoozed": n.snoozed, "done_today": n.done_today,
        })
    }

    /// Every open workspace tab's list, for the all-workspaces view.
    pub fn needs_all_json(&self) -> Value {
        self.needs_push(None).1
    }

    /// For the state push: (the shown workspace's list, every open tab's, (path, open,
    /// urgent) per tab). `pending`: the shown workspace's pending queue items, if known.
    pub fn needs_push(&self, pending: Option<&Value>) -> (Value, Value, Vec<(String, usize, usize)>) {
        let (open, current) = {
            let st = self.store.lock().unwrap();
            (st.open_workspaces.clone(), st.workspace.clone())
        };
        let (mut count, mut minutes) = (0, 0);
        let mut shown = Value::Null;
        let mut counts = vec![];
        let rows: Vec<Value> = open.iter().map(|w| {
            let n = self.needs_of_with(w, if same_path(w, &current) { pending } else { None });
            if same_path(w, &current) {
                shown = Self::ws_needs_json(w, &n);
            }
            counts.push((w.clone(), n.open.len(), n.urgent()));
            count += n.open.len();
            minutes += n.minutes();
            json!({
                "path": w, "name": super::folder_name(w), "key": orch_key(w), "current": same_path(w, &current),
                "count": n.open.len(), "urgent": n.urgent(), "minutes": n.minutes(), "today": n.today, "open": n.open,
            })
        }).collect();
        if shown.is_null() {
            shown = Self::ws_needs_json(&current, &self.needs_of_with(&current, pending));
        }
        (shown, json!({"count": count, "minutes": minutes, "workspaces": rows}), counts)
    }

    /// The open entries for the phone feed (no payload bytes beyond what the Queue shows).
    pub fn needs_feed(&self, ws: &str) -> Value {
        let n = self.needs_of(ws);
        let entry = |v: &View| json!({
            "id": v.id, "kind": v.kind, "title": v.title, "tldr": v.tldr, "why": v.why, "recommend": v.recommend,
            "minutes": v.minutes, "priority": v.priority, "priority_label": needs::label(&v.priority), "created_at": v.created_at, "status": v.status,
            "snoozed_until": v.snoozed_until, "waiting": v.waiting, "timed_out": v.timed_out, "default_answer": v.default_answer,
            "questions": v.questions, "plan": v.plan,
            "progress": {"step": v.step, "of": v.plan.as_ref().map_or(0, |p| p.steps.len())},
            "queue_item": if v.queue_item.is_empty() { Value::Null } else { json!(v.queue_item) },
            "careful": v.careful, "today": n.today.contains(&v.id), "source": v.source,
        });
        json!({
            "count": n.open.len(), "urgent": n.urgent(), "minutes": n.minutes(), "snoozed": n.snoozed.len(),
            "done_today": n.done_today.len(), "today": n.today,
            "open": n.open.iter().map(entry).collect::<Vec<_>>(),
        })
    }

    /// (open, urgent) for a workspace tab's badge.
    #[cfg(test)]
    pub fn needs_counts(&self, ws: &str) -> (usize, usize) {
        let n = self.needs_of(ws);
        (n.open.len(), n.urgent())
    }

    /// Store a new entry; returns its id.
    pub fn need_add(&self, mut n: Need) -> String {
        let ws = n.workspace.clone();
        let id = {
            let mut st = self.store.lock().unwrap();
            st.need_seq += 1;
            n.id = format!("n{}", st.need_seq);
            n.status = "open".into();
            n.created_at = now();
            n.priority = needs::priority_or_default(&n.priority);
            let id = n.id.clone();
            st.needs.push(n);
            st.save();
            id
        };
        self.needs_changed(&ws);
        id
    }

    fn need_get(&self, id: &str) -> Option<Need> {
        self.store.lock().unwrap().needs.iter().find(|n| n.id == id).cloned()
    }

    fn need_update(&self, id: &str, f: impl FnOnce(&mut Need)) -> Option<Need> {
        let mut st = self.store.lock().unwrap();
        let n = st.needs.iter_mut().find(|n| n.id == id)?;
        f(n);
        let c = n.clone();
        st.save();
        Some(c)
    }

    fn close_need(&self, id: &str, status: &str, by: &str, note: &str) -> Option<Need> {
        let n = self.need_update(id, |n| {
            n.status = status.into();
            n.closed_by = by.into();
            n.closed_at = now();
            if !note.trim().is_empty() {
                n.note = note.trim().to_string();
            }
        })?;
        self.store.lock().unwrap().need_marks.remove(id);
        self.needs_changed(&n.workspace);
        Some(n)
    }

    /// ask_user posted a question: it becomes an entry until it's answered.
    pub(super) fn need_question(&self, key: &str, ask_id: &str, questions: &Value, permission: bool, msg_ts: f64) -> String {
        let qtext = needs::questions_text(questions);
        self.need_add(Need {
            workspace: self.ws_of(key),
            kind: "question".into(),
            title: question_title(questions),
            tldr: question_title(questions),
            details: qtext,
            recommend: String::new(),
            minutes: 1,
            priority: "today".into(),
            source: key.into(),
            msg_ts,
            ref_id: ask_id.into(),
            questions: questions.clone(),
            permission,
            default_answer: if permission { String::new() } else { needs::default_answer(questions) },
            ..Default::default()
        })
    }

    /// The question was answered (window or phone, while the agent waited): close it.
    pub(super) fn need_question_answered(&self, ask_id: &str, answers: &Value) {
        let found = self.store.lock().unwrap().needs.iter().find(|n| n.ref_id == ask_id && n.status == "open").map(|n| (n.id.clone(), n.questions.clone()));
        if let Some((id, qs)) = found {
            let text = crate::mcp::answer_text(&qs, answers);
            let _ = self.need_update(&id, |n| n.answer = text);
            self.close_need(&id, "done", "user", "");
        }
    }

    /// The agent stopped waiting (timeout, or its turn stopped). A question stays open as
    /// "went ahead with the default"; a permission or effort prompt just closes.
    pub(super) fn need_question_dropped(&self, ask_id: &str) {
        let found = self.store.lock().unwrap().needs.iter().find(|n| n.ref_id == ask_id && n.status == "open").map(|n| (n.id.clone(), n.permission));
        match found {
            Some((id, true)) => {
                self.close_need(&id, "dismissed", "app", "expired: the agent stopped waiting");
            }
            Some((id, false)) => {
                if let Some(n) = self.need_update(&id, |n| n.timed_out = true) {
                    self.needs_changed(&n.workspace);
                }
            }
            None => {}
        }
    }

    /// After a restart nothing waits for old questions: they count as timed out, and
    /// permission prompts close.
    pub(super) fn needs_after_restart(&self) {
        let mut st = self.store.lock().unwrap();
        let t = now();
        for n in st.needs.iter_mut().filter(|n| n.kind == "question" && n.status == "open") {
            if n.permission {
                n.status = "dismissed".into();
                n.closed_by = "app".into();
                n.closed_at = t;
                n.note = "expired: the app restarted".into();
            } else {
                n.timed_out = true;
            }
        }
        st.needs.retain(|n| n.status == "open" || t - n.closed_at < KEEP_CLOSED_S);
    }

    /// Tell the agent behind an entry: the orchestrator as app news, a thread as a message.
    /// A thread that's gone is reported to its orchestrator instead.
    fn tell_agent(self: &Arc<Self>, source: &str, ws: &str, text: &str) {
        if !source.starts_with(ORCHESTRATOR) && self.store.lock().unwrap().get(source).is_some() {
            if self.send_to_thread(source, text, "you", vec![], true).is_ok() {
                return;
            }
        }
        let key = if source.starts_with(ORCHESTRATOR) { source.to_string() } else { orch_key(ws) };
        let text = if source.starts_with(ORCHESTRATOR) { text.to_string() } else { format!("(for thread {source}, which is gone) {text}") };
        self.post_news(&key, "@app", &text);
    }

    /// The user's answer from Needs you: straight to the agent if it still waits, else as
    /// a change sent to the agent.
    pub fn need_answer(self: &Arc<Self>, id: &str, answers: Value) -> Result<(), String> {
        let n = self.need_get(id).ok_or(format!("no entry {id}"))?;
        if n.kind != "question" {
            return Err(format!("{id} isn't a question"));
        }
        if n.status != "open" {
            return Err(format!("{id} is already {}", n.status));
        }
        if self.questions.lock().unwrap().contains_key(&n.ref_id) {
            return self.answer(&n.ref_id, answers);
        }
        let text = crate::mcp::answer_text(&n.questions, &answers);
        self.need_update(id, |x| {
            x.answer = text.clone();
            x.timed_out = true;
        });
        self.close_need(id, "done", "user", "answered after the agent went ahead");
        let went = if n.default_answer.is_empty() { "your default".to_string() } else { format!("\"{}\"", n.default_answer) };
        self.tell_agent(&n.source, &n.workspace, &format!(
            "[late answer from the user] Your earlier ask_user question timed out and you went ahead with {went}. They have now answered:\n{text}\n\
If that changes what you did, change it now and tell them what you changed (in one or two lines)."
        ));
        Ok(())
    }

    /// The window's Needs you buttons (IPC only, never HTTP): done, snooze, unsnooze,
    /// dismiss, pin, step, stuck. `ws`, if given, must be an open workspace the entry is in.
    pub fn need_user(self: &Arc<Self>, action: &str, id: &str, ws: Option<&str>, v: &Value) -> Result<(), String> {
        let entry_ws = self.need_ws(id).ok_or(format!("no open entry {id}"))?;
        if let Some(w) = ws.filter(|w| !w.is_empty()) {
            let open = self.store.lock().unwrap().open_workspaces.iter().any(|o| same_path(o, w));
            if !open || !same_path(w, &entry_ws) {
                return Err(format!("{id} isn't in an open workspace {w}"));
            }
        }
        let note = v.get("note").and_then(Value::as_str).unwrap_or("").trim().to_string();
        match action {
            "answer" => self.need_answer(id, v.get("answers").cloned().unwrap_or(json!([]))),
            "snooze" => {
                let until = match v.get("until") {
                    Some(Value::Number(n)) => n.to_string(),
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                let t = needs::snooze_until(&until, chrono::Local::now()).ok_or("snooze until 1h, tonight, tomorrow or a later time")?;
                self.mark(id, &entry_ws, |m| m.snooze_until = t);
                Ok(())
            }
            "unsnooze" => {
                self.mark(id, &entry_ws, |m| m.snooze_until = 0.0);
                Ok(())
            }
            "pin" => {
                let p = v.get("pin").and_then(Value::as_i64).unwrap_or(1).clamp(-1, 1) as i8;
                self.mark(id, &entry_ws, |m| m.pin = p);
                Ok(())
            }
            "step" | "stuck" => {
                let all = self.needs_of(&entry_ws);
                let view = all.open.into_iter().chain(all.snoozed).find(|x| x.id == id).ok_or(format!("{id} isn't open"))?;
                let plan = view.plan.clone().ok_or(format!("{id} has no step-by-step plan"))?;
                let step = (v.get("step").and_then(Value::as_u64).unwrap_or(0) as usize).min(plan.steps.len().saturating_sub(1));
                self.mark(id, &entry_ws, |m| m.step = step);
                if action == "stuck" {
                    let s = &plan.steps[step];
                    self.tell_agent(&view.source, &entry_ws, &format!(
                        "[the user is stuck] On \"{}\" ({id}), step {} of {}: \"{}\" (they should have seen: {}).\nTheir note: {}\n\
Help them: tell them what to try (tell_user), or send clearer steps with request_user_action and dismiss_need this one. They saw if_stuck: {}",
                        view.title, step + 1, plan.steps.len(), s.action, s.expect,
                        if note.is_empty() { "(none)" } else { &note },
                        if plan.if_stuck.is_empty() { "(none given)" } else { &plan.if_stuck }
                    ));
                }
                Ok(())
            }
            "done" => {
                if let Some(q) = id.strip_prefix("manual:") {
                    return self.queue_user_done(q);
                }
                if id.starts_with("queue:") {
                    return Err("decide it in the card: approve or reject".into());
                }
                let n = self.need_get(id).ok_or(format!("no entry {id}"))?;
                if n.kind == "question" {
                    return Err("answer the question instead".into());
                }
                self.close_need(id, "done", "user", &note).ok_or(format!("no entry {id}"))?;
                if n.kind == "action" {
                    self.tell_agent(&n.source, &n.workspace, &format!(
                        "[done by the user] The user marked \"{}\" ({id}) done in Needs you{}{}. Carry on with what it was for.",
                        n.title,
                        n.plan.as_ref().map(|p| format!(" and confirmed: {}", p.done_when)).unwrap_or_default(),
                        if note.is_empty() { String::new() } else { format!(". Their note: {note}") }
                    ));
                }
                Ok(())
            }
            "dismiss" => {
                if id.starts_with("queue:") || id.starts_with("manual:") {
                    return Err("queue items are rejected in the card (with a reason), not dismissed".into());
                }
                let n = self.need_get(id).ok_or(format!("no entry {id}"))?;
                if n.kind == "question" && self.questions.lock().unwrap().contains_key(&n.ref_id) {
                    // The agent is still waiting: it gets the dismissal as its answer.
                    let msg = format!("The user dismissed this question{}", if note.is_empty() { String::new() } else { format!(": {note}") });
                    let answers: Vec<Value> = n.questions.as_array().map(|q| q.iter().map(|_| json!({"selected": [], "other": msg})).collect()).unwrap_or_default();
                    let _ = self.answer(&n.ref_id, json!(answers));
                }
                self.close_need(id, "dismissed", "user", &note);
                if n.kind != "heads_up" && !(n.kind == "question" && n.permission) {
                    self.tell_agent(&n.source, &n.workspace, &format!(
                        "[dismissed by the user] The user dismissed \"{}\" ({id}) in Needs you{}. Don't ask again unless something changes.",
                        n.title, if note.is_empty() { String::new() } else { format!(": {note}") }
                    ));
                }
                Ok(())
            }
            other => Err(format!("unknown need action {other}")),
        }
    }

    /// The workspace an entry (stored or from the queue) belongs to.
    fn need_ws(&self, id: &str) -> Option<String> {
        let st = self.store.lock().unwrap();
        if let Some(q) = id.strip_prefix("queue:").or(id.strip_prefix("manual:")) {
            return st.queue.iter().find(|i| i.id == q).map(|i| i.workspace.clone());
        }
        st.needs.iter().find(|n| n.id == id).map(|n| n.workspace.clone())
    }

    fn mark(&self, id: &str, ws: &str, f: impl FnOnce(&mut Mark)) {
        {
            let mut st = self.store.lock().unwrap();
            f(st.need_marks.entry(id.to_string()).or_default());
            st.save();
        }
        self.needs_changed(ws);
    }

    // ---- the agents' tools ----

    /// request_user_action: a task the user does by hand, with a precise plan.
    pub fn need_action(&self, key: &str, args: &Value) -> Result<String, String> {
        let plan = needs::parse_plan(args)?;
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let (tldr, why) = (s("tldr"), s("why"));
        if tldr.is_empty() || why.is_empty() {
            return Err("tldr (one plain line: what the user does) and why (one line: what it unlocks) are required".into());
        }
        let recommend = if s("recommend").is_empty() { "do".to_string() } else { s("recommend").to_lowercase() };
        if !needs::RECOMMENDS.contains(&recommend.as_str()) {
            return Err(format!("recommend must be one of {}", needs::RECOMMENDS.join(", ")));
        }
        let minutes = args.get("minutes").and_then(Value::as_f64).unwrap_or(plan.steps.len() as f64).clamp(1.0, 600.0) as u32;
        let msg_ts = self.hub.emit(key, json!({"kind": "notice", "text": format!("📌 Added to Needs you: {tldr}")}));
        let id = self.need_add(Need {
            workspace: self.ws_of(key),
            kind: "action".into(),
            title: plan.goal.clone(),
            tldr,
            details: plan.before_you_start.clone(),
            why,
            recommend,
            minutes,
            priority: s("priority"),
            source: key.into(),
            msg_ts,
            link: s("link"),
            plan: Some(plan),
            ..Default::default()
        });
        Ok(format!("added {id} to the user's Needs you list. You'll get a message when they finish it, get stuck or dismiss it; don't ask again in chat."))
    }

    /// tell_user important: a heads-up entry, besides the chat message.
    pub fn need_heads_up(&self, key: &str, text: &str, args: &Value, msg_ts: f64) -> String {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let tldr = if s("tldr").is_empty() { first_line(text, 140) } else { s("tldr") };
        self.need_add(Need {
            workspace: self.ws_of(key),
            kind: "heads_up".into(),
            title: tldr.clone(),
            tldr,
            details: text.trim().to_string(),
            why: s("why"),
            recommend: s("recommend"),
            minutes: args.get("minutes").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.0, 600.0) as u32,
            priority: s("priority"),
            source: key.into(),
            msg_ts,
            ..Default::default()
        })
    }

    /// list_needs_you: what's waiting on the user here. The orchestrator can set its
    /// Today picks with `today`.
    pub fn need_list(&self, key: &str, today: Option<Vec<String>>) -> Result<String, String> {
        let ws = self.ws_of(key);
        if let Some(ids) = today {
            if !key.starts_with(ORCHESTRATOR) {
                return Err("only the orchestrator picks Today's entries".into());
            }
            if ids.len() > needs::TODAY_MAX {
                return Err(format!("pick at most {} for Today", needs::TODAY_MAX));
            }
            let open: Vec<String> = { let n = self.needs_of(&ws); n.open.iter().chain(n.snoozed.iter()).map(|v| v.id.clone()).collect() };
            if let Some(bad) = ids.iter().find(|i| !open.contains(i)) {
                return Err(format!("{bad} isn't an open entry here"));
            }
            {
                let mut st = self.store.lock().unwrap();
                for id in &open {
                    let m = st.need_marks.entry(id.clone()).or_default();
                    m.today = ids.contains(id);
                }
                st.need_marks.retain(|_, m| *m != Mark::default());
                st.save();
            }
            self.needs_changed(&ws);
        }
        let n = self.needs_of(&ws);
        let brief = |v: &View| {
            let mut e = json!({
                "id": v.id, "kind": v.kind, "tldr": v.tldr, "title": v.title, "priority": v.priority, "minutes": v.minutes,
                "recommend": v.recommend, "why": v.why, "source": v.source, "status": v.status, "created_at": v.created_at,
            });
            if v.kind == "question" {
                e["waiting"] = json!(v.waiting);
                e["timed_out"] = json!(v.timed_out);
                e["default_answer"] = json!(v.default_answer);
            }
            if let Some(p) = &v.plan {
                e["progress"] = json!(format!("step {} of {}", v.step + 1, p.steps.len()));
            }
            if v.status == "snoozed" {
                e["snoozed_until"] = json!(crate::app::clock_time(v.snoozed_until));
            }
            if !v.note.is_empty() {
                e["note"] = json!(v.note);
            }
            if !v.closed_by.is_empty() {
                e["closed_by"] = json!(v.closed_by);
            }
            e
        };
        Ok(serde_json::to_string_pretty(&json!({
            "open": n.open.iter().map(brief).collect::<Vec<_>>(),
            "today": n.today,
            "snoozed": n.snoozed.iter().map(brief).collect::<Vec<_>>(),
            "done_today": n.done_today.iter().map(brief).collect::<Vec<_>>(),
            "minutes_waiting": n.minutes(),
        })).unwrap_or_default())
    }

    /// dismiss_need: an agent closes its own question, action or heads-up (the
    /// orchestrator: any in its workspace), with the reason shown to the user.
    pub fn need_dismiss_agent(&self, key: &str, id: &str, reason: &str) -> Result<String, String> {
        if reason.trim().is_empty() {
            return Err("give the reason (shown to the user)".into());
        }
        if id.starts_with("queue:") || id.starts_with("manual:") {
            return Err("queue items are decided by the user; you can't dismiss them".into());
        }
        let n = self.need_get(id).ok_or(format!("no entry {id}"))?;
        let ws = self.ws_of(key);
        let mine = n.source == key || (key.starts_with(ORCHESTRATOR) && same_path(&n.workspace, &ws));
        if !mine {
            return Err(format!("{id} isn't yours to dismiss"));
        }
        if n.status != "open" {
            return Err(format!("{id} is already {}", n.status));
        }
        if n.kind == "question" && self.questions.lock().unwrap().contains_key(&n.ref_id) {
            let msg = format!("(withdrawn by the orchestrator: {})", reason.trim());
            let answers: Vec<Value> = n.questions.as_array().map(|q| q.iter().map(|_| json!({"selected": [], "other": msg})).collect()).unwrap_or_default();
            let _ = self.answer(&n.ref_id, json!(answers));
        }
        self.close_need(id, "dismissed", "agent", reason);
        Ok(format!("dismissed {id}; the user sees your reason"))
    }

    // ---- change fan-out: mirror, feed, notifications ----

    /// After any change to a workspace's list: the mirror, the feed event, urgent pushes,
    /// and the window.
    pub fn needs_changed(&self, ws: &str) {
        let business = self.is_business(ws);
        let n = self.needs_of(ws);
        self.write_mirror(ws, business, &n);
        if business {
            self.business_event(ws, "needs_you_changed", None, json!({"count": n.open.len(), "urgent": n.urgent(), "minutes": n.minutes()}));
            let fresh: Vec<&View> = {
                let mut st = self.store.lock().unwrap();
                let fresh: Vec<&View> = n.open.iter().filter(|v| v.priority == "urgent" && !st.push_sent.contains(&format!("{}|needs:{}", orch_key(ws), v.id))).collect();
                for v in &fresh {
                    st.push_sent.push(format!("{}|needs:{}", orch_key(ws), v.id));
                }
                if !fresh.is_empty() {
                    st.save();
                }
                fresh
            };
            for v in fresh {
                self.business_event(ws, "needs_you", Some("needs_you"), json!({"text": v.tldr, "ref": v.id, "entry_kind": v.kind}));
                self.notify(ws, "needs_you", "Needs you now", &v.tldr);
            }
        }
        self.touch();
    }

    /// <ws>\TODO-FOR-YOU.md, only for business workspaces or where it already exists, and
    /// only when its text changes.
    fn write_mirror(&self, ws: &str, business: bool, n: &WsNeeds) {
        let path = std::path::Path::new(ws).join(needs::MIRROR);
        if !business && !path.exists() {
            return;
        }
        let text = needs::mirror(&super::folder_name(ws), &n.open, &n.today, &n.snoozed, &n.done_today);
        if std::fs::read_to_string(&path).is_ok_and(|t| t == text) {
            return;
        }
        crate::queue::set_readonly(&path, false);
        let _ = std::fs::write(&path, text);
        crate::queue::set_readonly(&path, true);
    }

    /// Every 30 s: snoozes that ran out, the mirror (Done today rolls over at midnight),
    /// and the daily summary after 09:00 (business workspaces).
    pub fn needs_tick(&self) {
        use chrono::Timelike;
        let t = now();
        let (wss, woke) = {
            let mut st = self.store.lock().unwrap();
            let mut w: Vec<String> = st.open_workspaces.clone();
            w.extend(st.projects.iter().filter(|(_, n)| n.kind == "business").map(|(k, _)| k.clone()));
            w.dedup_by(|a, b| same_path(a, b));
            // Snoozes that ran out.
            let mut woke = false;
            for m in st.need_marks.values_mut().filter(|m| m.snooze_until > 0.0 && m.snooze_until <= t) {
                m.snooze_until = 0.0;
                woke = true;
            }
            let before = st.needs.len();
            st.needs.retain(|n| n.status == "open" || t - n.closed_at < KEEP_CLOSED_S);
            if woke || before != st.needs.len() {
                st.save();
            }
            (w, woke)
        };
        let local = chrono::Local::now();
        let day = local.format("%Y-%m-%d").to_string();
        let mut seen: Vec<String> = vec![];
        for ws in wss {
            if seen.iter().any(|s| same_path(s, &ws)) {
                continue;
            }
            seen.push(ws.clone());
            let business = self.is_business(&ws);
            let n = self.needs_of(&ws);
            self.write_mirror(&ws, business, &n);
            if !business || local.hour() < 9 {
                continue;
            }
            let due = {
                let mut st = self.store.lock().unwrap();
                let notes = st.notes_mut(&ws);
                if notes.needs_summary_day == day {
                    false
                } else {
                    notes.needs_summary_day = day.clone();
                    st.save();
                    true
                }
            };
            if due && !n.open.is_empty() {
                let top: Vec<String> = n.open.iter().filter(|v| n.today.contains(&v.id)).map(|v| v.tldr.clone()).collect();
                let text = format!("{} waiting, about {} min. Today: {}", n.open.len(), n.minutes(), top.join("; "));
                self.notify(&ws, "needs_you_daily", "Still waiting on you", &text);
                self.business_event(&ws, "needs_you_daily", Some("needs_you_daily"), json!({"text": text, "count": n.open.len(), "minutes": n.minutes()}));
            }
        }
        if woke {
            self.touch();
        }
    }
}

#[cfg(test)]
mod tests;
