//! Cross-workspace session messages (message_session), gated by the user.
//!
//! A session may message a session in ANOTHER open workspace only when the user allows it:
//! once (a "message" entry in the sender workspace's Needs you, plus a card in the chat), or
//! by a standing rule from-workspace → to-workspace. Rules are created, approved, denied and
//! revoked only from the window (IPC op "xws"), never over HTTP and never by an agent.
//! Within one workspace the orchestrator and its threads talk as before, with no gate.

use super::{folder_name, orch_key, App, Queued, ORCHESTRATOR};
use crate::needs::Need;
use crate::store::{now, same_path, XwsRule};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// How long a message request waits for the user before it fails closed (well under the
/// CLI's 1 h MCP tool timeout).
pub const XWS_WAIT: Duration = Duration::from_secs(30 * 60);
pub const XWS_MAX_CHARS: usize = 8000;

pub enum XwsDecision {
    Allow { always: bool },
    Deny { note: String },
}

pub struct XwsPending {
    /// The sending session's key (its turn stopping cancels the request).
    pub key: String,
    pub need_id: String,
    tx: oneshot::Sender<XwsDecision>,
}

/// A resolved target: workspace path, session label ("orchestrator" or a thread name), key.
struct Target {
    ws: String,
    session: String,
    key: String,
}

impl App {
    /// "orchestrator" for an orchestrator key, else the thread name.
    fn session_label(key: &str) -> String {
        if key.starts_with(ORCHESTRATOR) { ORCHESTRATOR.to_string() } else { key.to_string() }
    }

    /// Find an open workspace by path or folder name (case-insensitive).
    fn open_ws_named(&self, name: &str) -> Result<String, String> {
        let open = self.store.lock().unwrap().open_workspaces.clone();
        let n = name.trim();
        if let Some(p) = open.iter().find(|p| same_path(p, n)) {
            return Ok(p.clone());
        }
        let hits: Vec<&String> = open.iter().filter(|p| folder_name(p).eq_ignore_ascii_case(n)).collect();
        match hits.len() {
            1 => Ok(hits[0].clone()),
            0 => Err(format!("no open workspace '{n}': list_sessions shows the open ones")),
            _ => Err(format!("more than one open workspace is called '{n}': give its full path")),
        }
    }

    fn xws_rule_exists(&self, from: &str, to: &str) -> bool {
        self.store.lock().unwrap().xws_rules.iter().any(|r| same_path(&r.from_ws, from) && same_path(&r.to_ws, to))
    }

    /// Audit: business workspaces get their hash-chained log; every workspace gets the app's copy.
    fn xws_audit(&self, ws: &str, line: &str) {
        let line = format!("xws {line}");
        if self.is_business(ws) {
            self.audit(ws, &line);
        } else {
            let mut st = self.store.lock().unwrap();
            st.audit.push((now(), ws.to_string(), crate::vault::redact(&line)));
            let over = st.audit.len().saturating_sub(5000);
            st.audit.drain(..over);
            st.save();
        }
    }

    /// list_sessions: open workspaces and their sessions (no transcripts).
    pub fn list_sessions(&self, key: &str, only: Option<&str>) -> Result<String, String> {
        let mine = self.ws_of(key);
        let open = match only.filter(|w| !w.trim().is_empty()) {
            Some(w) => vec![self.open_ws_named(w)?],
            None => self.store.lock().unwrap().open_workspaces.clone(),
        };
        let threads = self.store.lock().unwrap().threads.clone();
        let plans = self.store.lock().unwrap().plans.clone();
        let mut out = vec![];
        for w in &open {
            let ours = same_path(w, &mine);
            let gate = if ours {
                "your own workspace: use send_to_thread / message_orchestrator, not message_session".to_string()
            } else if self.xws_rule_exists(&mine, w) {
                "standing rule: your messages go straight through".to_string()
            } else {
                "each message needs the user's OK".to_string()
            };
            out.push(format!("## {} ({w}) — {gate}", folder_name(w)));
            let ok = orch_key(w);
            let topic = plans.get(&ok).and_then(|p| p.as_array().and_then(|a| a.iter().find(|i| i["status"] == "in_progress").and_then(|i| i["content"].as_str()).map(str::to_string)));
            out.push(format!("- orchestrator: {}{}", if self.busy(&ok) { "working" } else { "idle" }, topic.map(|t| format!(" — {}", crate::events::clip(&t))).unwrap_or_default()));
            for t in threads.iter().filter(|t| same_path(&t.workspace, w)) {
                let status = if self.busy(&t.name) { "working" } else if t.resolved { "resolved" } else { t.status.as_str() };
                let line: String = t.last_prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(100).collect();
                out.push(format!("- {}: {status}{}", t.name, if line.is_empty() { String::new() } else { format!(" — {line}") }));
            }
        }
        Ok(out.join("\n"))
    }

    fn xws_target(&self, ws: &str, session: &str) -> Result<Target, String> {
        let to_ws = self.open_ws_named(ws)?;
        let s = session.trim();
        if s.is_empty() || s.eq_ignore_ascii_case(ORCHESTRATOR) || s == orch_key(&to_ws) {
            return Ok(Target { key: orch_key(&to_ws), ws: to_ws, session: ORCHESTRATOR.into() });
        }
        let row = self.store.lock().unwrap().get(s).cloned().ok_or(format!("no session '{s}' in {}: list_sessions shows them", folder_name(&to_ws)))?;
        if !same_path(&row.workspace, &to_ws) {
            return Err(format!("no session '{s}' in {}: list_sessions shows them", folder_name(&to_ws)));
        }
        Ok(Target { key: row.name.clone(), ws: to_ws, session: row.name })
    }

    /// message_session: send to a session in another open workspace, once the user allows it.
    pub async fn message_session(self: &Arc<Self>, key: &str, args: &Value) -> Result<String, String> {
        self.message_session_with(key, args, XWS_WAIT).await
    }

    pub async fn message_session_with(self: &Arc<Self>, key: &str, args: &Value, wait: Duration) -> Result<String, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let (ws_arg, session, message, why) = (s("workspace"), s("session"), s("message"), s("why"));
        if ws_arg.is_empty() {
            return Err("workspace is required".into());
        }
        if message.is_empty() {
            return Err("message is required".into());
        }
        if why.is_empty() {
            return Err("why is required: one line the user reads before allowing it".into());
        }
        if message.chars().count() > XWS_MAX_CHARS {
            return Err(format!("message too long (limit {XWS_MAX_CHARS} characters)"));
        }
        let from_ws = self.ws_of(key);
        let target = self.xws_target(&ws_arg, &session)?;
        if same_path(&target.ws, &from_ws) {
            return Err(if key.starts_with(ORCHESTRATOR) {
                "that's your own workspace: use send_to_thread to message your threads".into()
            } else {
                "that's your own workspace: use message_orchestrator to reach your orchestrator".into()
            });
        }
        let from_session = Self::session_label(key);
        let (from_name, to_name) = (folder_name(&from_ws), folder_name(&target.ws));
        let route = format!("{from_name}/{from_session} -> {to_name}/{}", target.session);
        self.xws_audit(&from_ws, &format!("request {route}: why: {why}; message: {}", crate::events::clip(&message)));

        if self.xws_rule_exists(&from_ws, &target.ws) {
            self.xws_deliver(&from_ws, &from_session, &target, &message);
            self.xws_audit(&from_ws, &format!("delivered {route} (standing rule)"));
            self.hub.emit(key, json!({"kind": "xws_result", "id": "", "outcome": "rule",
                "note": format!("sent to {to_name} / {} (standing rule)", target.session)}));
            return Ok(format!("delivered to {to_name} / {} (the user has a standing rule allowing {from_name} → {to_name})", target.session));
        }

        // Ask the user: a Needs you entry in the sender's workspace and a card in its chat.
        let id = format!("x{}-{:04x}", (now() * 1000.0) as u64, rand::random::<u16>());
        let info = json!({
            "request": id, "from_ws": from_ws, "from_name": from_name, "from_session": from_session,
            "to_ws": target.ws, "to_name": to_name, "to_session": target.session, "message": message, "why": why,
        });
        let need_id = self.need_add(Need {
            workspace: from_ws.clone(),
            kind: "message".into(),
            title: format!("Send a message to {to_name} / {}?", target.session),
            tldr: format!("{from_name} / {from_session} wants to message {to_name} / {}", target.session),
            details: message.clone(),
            why: why.clone(),
            minutes: 1,
            priority: "today".into(),
            source: key.into(),
            ref_id: id.clone(),
            permission: true,
            xws: info.clone(),
            ..Default::default()
        });
        let (tx, rx) = oneshot::channel();
        self.xws_pending.lock().unwrap().insert(id.clone(), XwsPending { key: key.into(), need_id: need_id.clone(), tx });
        let mut ev = info.clone();
        ev["kind"] = json!("xws_request");
        ev["id"] = json!(id);
        ev["need_id"] = json!(need_id);
        let ts = self.hub.emit(key, ev);
        if let Some(n) = self.store.lock().unwrap().needs.iter_mut().find(|n| n.id == need_id) {
            n.msg_ts = ts;
        }
        self.needs_changed(&from_ws);
        self.touch();
        crate::attention();

        // Stopped turn: the CLI hangs up, this future is dropped and nothing is sent.
        struct Cleanup(Arc<App>, String, String, String, String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(p) = self.0.xws_pending.lock().unwrap().remove(&self.1) {
                    self.0.hub.emit(&self.2, json!({"kind": "xws_result", "id": self.1, "outcome": "cancelled", "note": "the agent stopped waiting: not sent"}));
                    self.0.close_need(&p.need_id, "dismissed", "app", "expired: the agent stopped waiting, nothing was sent");
                    self.0.xws_audit(&self.3, &format!("cancelled {} (turn stopped): not sent", self.4));
                    self.0.touch();
                }
            }
        }
        let _cleanup = Cleanup(self.clone(), id.clone(), key.to_string(), from_ws.clone(), route.clone());

        let decision = match tokio::time::timeout(wait, rx).await {
            Ok(Ok(d)) => d,
            Ok(Err(_)) => {
                // Dropped by stop() (xws_drop_for closed the entry and logged it): fail closed.
                return Err("the request was cancelled (your turn was stopped); the message was NOT sent".into());
            }
            Err(_) => {
                self.xws_pending.lock().unwrap().remove(&id);
                self.xws_closed(&id, key, &need_id, "expired", "no answer in time: not sent");
                self.xws_audit(&from_ws, &format!("expired {route}: no answer, not sent"));
                return Err("the user didn't answer in time; the message was NOT sent. Don't retry unless it still matters; say so in your summary".into());
            }
        };
        match decision {
            XwsDecision::Deny { note } => {
                let n = if note.is_empty() { String::new() } else { format!(": {note}") };
                self.xws_closed(&id, key, &need_id, "denied", &format!("denied{n}"));
                self.xws_audit(&from_ws, &format!("denied {route}{n}"));
                Err(format!("the user denied this message; it was NOT sent{}", if note.is_empty() { ". Don't send it again the same way".to_string() } else { format!(". Their note: {note}") }))
            }
            XwsDecision::Allow { always } => {
                // The user may have taken a while: the target must still be there, and be the
                // same session (a thread deleted and re-made elsewhere is not the same one).
                let target = match self.xws_target(&target.ws, &target.session) {
                    Ok(t) if t.key == target.key && same_path(&t.ws, &target.ws) => t,
                    _ => {
                        self.xws_closed(&id, key, &need_id, "gone", "not sent: the target is gone");
                        self.xws_audit(&from_ws, &format!("not sent {route}: the target is gone"));
                        return Err("not sent: the target is gone (its workspace was closed or the session was deleted)".into());
                    }
                };
                if always {
                    let _ = self.xws_rule_add(&from_ws, &target.ws, "prompt");
                }
                self.xws_deliver(&from_ws, &from_session, &target, &message);
                self.xws_audit(&from_ws, &format!("allowed{} and delivered {route}", if always { " (always)" } else { " once" }));
                self.xws_closed(&id, key, &need_id, if always { "always" } else { "allowed" }, "sent");
                Ok(format!("the user allowed it{}; delivered to {to_name} / {}", if always { " (and set a standing rule for this direction)" } else { "" }, target.session))
            }
        }
    }

    /// Mark a resolved request: chat card result, Needs entry closed.
    fn xws_closed(&self, id: &str, key: &str, need_id: &str, outcome: &str, note: &str) {
        self.hub.emit(key, json!({"kind": "xws_result", "id": id, "outcome": outcome, "note": note}));
        let status = if matches!(outcome, "allowed" | "always") { "done" } else { "dismissed" };
        let by = if matches!(outcome, "allowed" | "always" | "denied") { "user" } else { "app" };
        self.close_need(need_id, status, by, note);
        self.touch();
    }

    /// Put the message in the target's queue (thread) or news (orchestrator). In a paused
    /// workspace it waits in the queue like any other non-"you" message.
    fn xws_deliver(self: &Arc<Self>, from_ws: &str, from_session: &str, t: &Target, message: &str) {
        let from_name = folder_name(from_ws);
        let text = format!(
            "[from workspace {from_name} / session {from_session}] {message}\n\n(To reply, use message_session with workspace \"{from_name}\" and session \"{from_session}\". The user may have to approve it.)"
        );
        let sender = format!("xws:{from_name}/{from_session}");
        let paused = self.is_paused(&t.ws);
        if paused || !t.key.starts_with(ORCHESTRATOR) {
            let msg = Queued { id: String::new(), text, sender, attach: vec![], fork: None };
            // Paused: enqueue holds it (the worker only runs the user's own messages).
            self.deliver(&t.key, msg, !paused);
        } else {
            // Its own label: another agent's words must never look like a notice from the app.
            self.post_news(&t.key, &format!("@workspace {from_name}/{from_session}"), &text);
        }
        // A business target keeps its own chained record of what came in.
        if self.is_business(&t.ws) {
            self.audit(&t.ws, &format!("xws received {from_name}/{from_session} -> {}: {}", t.session, crate::events::clip(message)));
        }
    }

    /// Request a waiting message by its request id or Needs entry id.
    fn xws_find(&self, id: &str) -> Option<String> {
        let p = self.xws_pending.lock().unwrap();
        if p.contains_key(id) {
            return Some(id.to_string());
        }
        p.iter().find(|(_, v)| v.need_id == id).map(|(k, _)| k.clone())
    }

    /// The user's answer to a message request (IPC only).
    pub fn xws_decide(&self, id: &str, d: XwsDecision) -> Result<(), String> {
        let rid = self.xws_find(id).ok_or("that message request is no longer waiting")?;
        let p = self.xws_pending.lock().unwrap().remove(&rid).ok_or("that message request is no longer waiting")?;
        p.tx.send(d).map_err(|_| "the agent stopped waiting".to_string())
    }

    /// Add a standing rule (the window, or Always allow). Both must be open workspaces.
    pub fn xws_rule_add(&self, from: &str, to: &str, via: &str) -> Result<String, String> {
        let from = self.open_ws_named(from)?;
        let to = self.open_ws_named(to)?;
        if same_path(&from, &to) {
            return Err("pick two different workspaces".into());
        }
        if self.xws_rule_exists(&from, &to) {
            return Err(format!("{} → {} is already allowed", folder_name(&from), folder_name(&to)));
        }
        let id = {
            let mut st = self.store.lock().unwrap();
            st.xws_seq += 1;
            let id = format!("r{}", st.xws_seq);
            st.xws_rules.push(XwsRule { id: id.clone(), from_ws: from.clone(), to_ws: to.clone(), created_at: now() });
            st.save();
            id
        };
        self.xws_audit(&from, &format!("rule added {id}: {} -> {} (by the user, {via})", folder_name(&from), folder_name(&to)));
        self.touch();
        Ok(id)
    }

    pub fn xws_rule_revoke(&self, id: &str) -> Result<(), String> {
        let r = {
            let mut st = self.store.lock().unwrap();
            let i = st.xws_rules.iter().position(|r| r.id == id).ok_or(format!("no rule {id}"))?;
            let r = st.xws_rules.remove(i);
            st.save();
            r
        };
        self.xws_audit(&r.from_ws, &format!("rule revoked {id}: {} -> {}", folder_name(&r.from_ws), folder_name(&r.to_ws)));
        self.touch();
        Ok(())
    }

    /// The window's xws buttons (handle_ipc op "xws" only, never HTTP).
    pub fn xws_user(&self, action: &str, v: &Value) -> Result<(), String> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        match action {
            "allow_once" => self.xws_decide(&s("id"), XwsDecision::Allow { always: false }),
            "allow_always" => self.xws_decide(&s("id"), XwsDecision::Allow { always: true }),
            "deny" => self.xws_decide(&s("id"), XwsDecision::Deny { note: s("note") }),
            "rule_add" => self.xws_rule_add(&s("from"), &s("to"), "settings").map(|_| ()),
            "rule_revoke" => self.xws_rule_revoke(&s("id")),
            other => Err(format!("unknown xws action {other}")),
        }
    }

    /// For the state push and GET /api/xws/rules (read-only).
    pub fn xws_rules_json(&self) -> Value {
        let rules = self.store.lock().unwrap().xws_rules.clone();
        let rows: Vec<Value> = rules.iter().rev().map(|r| json!({
            "id": r.id, "from": r.from_ws, "from_name": folder_name(&r.from_ws),
            "to": r.to_ws, "to_name": folder_name(&r.to_ws), "created_at": r.created_at,
        })).collect();
        json!(rows)
    }

    pub fn xws_state_json(&self) -> Value {
        json!({"rules": self.xws_rules_json(), "pending": self.xws_pending.lock().unwrap().len()})
    }

    pub(super) fn xws_waiting_ids(&self) -> Vec<String> {
        self.xws_pending.lock().unwrap().keys().cloned().collect()
    }

    /// A stopped turn drops its waiting requests (the agent then fails closed).
    pub(super) fn xws_drop_for(&self, key: &str) {
        let dropped: Vec<(String, XwsPending)> = {
            let mut m = self.xws_pending.lock().unwrap();
            let ids: Vec<String> = m.iter().filter(|(_, p)| p.key == key).map(|(k, _)| k.clone()).collect();
            ids.into_iter().filter_map(|k| m.remove(&k).map(|p| (k, p))).collect()
        };
        for (id, p) in dropped {
            let ws = self.ws_of(&p.key);
            self.xws_closed(&id, &p.key, &p.need_id, "cancelled", "the turn was stopped: not sent");
            self.xws_audit(&ws, &format!("cancelled {id} (turn stopped): not sent"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

    /// dev (development) and biz (business, paused, so nothing starts a CLI turn), with a
    /// thread "worker-a" in biz. Returns (app, dev, biz, sender key = dev's orchestrator).
    fn app(name: &str) -> (Arc<App>, String, String, String) {
        let root = std::env::temp_dir().join(format!("orch-xws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |n: &str| { let p = root.join(n); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() };
        let (dev, biz) = (mk("dev"), mk("biz"));
        let state = json!({
            "workspace": dev, "open_workspaces": [dev, biz],
            "projects": {biz.clone(): {"kind": "business", "paused": true}},
            "threads": [{"name": "worker-a", "workspace": biz, "cwd": biz, "status": "idle"}],
        });
        std::fs::write(root.join("state.json"), state.to_string()).unwrap();
        let app = App::new(Paths { config_dir: root.clone(), data_dir: root.clone() }, "t".repeat(40));
        let key = orch_key(&dev);
        (app, dev, biz, key)
    }

    fn queued(app: &App, key: &str) -> Vec<String> {
        app.queues.lock().unwrap().get(key).map(|q| q.iter().map(|m| m.text.clone()).collect()).unwrap_or_default()
    }

    fn msg(session: &str, text: &str) -> Value {
        json!({"workspace": "biz", "session": session, "message": text, "why": "it needs the price list"})
    }

    /// Wait until one request is pending; returns its Needs entry id.
    async fn pending(app: &App) -> String {
        for _ in 0..200 {
            if let Some(p) = app.xws_pending.lock().unwrap().values().next() {
                return p.need_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no request became pending");
    }

    fn spawn(app: &Arc<App>, key: &str, args: Value) -> tokio::task::JoinHandle<Result<String, String>> {
        let (a, k) = (app.clone(), key.to_string());
        tokio::spawn(async move { a.message_session(&k, &args).await })
    }

    #[tokio::test]
    async fn the_call_blocks_until_the_user_allows_it() {
        let (app, dev, biz, key) = app("allow");
        let h = spawn(&app, &key, msg("worker-a", "HELLO-A"));
        let need = pending(&app).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!h.is_finished(), "it waits for the user");
        assert!(queued(&app, "worker-a").is_empty(), "nothing is sent before the OK");
        let v = app.needs_of(&dev).open.into_iter().find(|v| v.id == need).expect("a Needs you entry in the sender workspace");
        assert_eq!(v.kind, "message");
        assert!(v.waiting && v.details == "HELLO-A" && v.why == "it needs the price list");
        assert_eq!(v.xws["to_session"], "worker-a");
        assert!(app.needs_of(&biz).open.iter().all(|v| v.kind != "message"), "only the sender's workspace shows it");
        app.xws_user("allow_once", &json!({"id": need})).unwrap();
        let r = h.await.unwrap().unwrap();
        assert!(r.contains("delivered"), "{r}");
        let q = queued(&app, "worker-a");
        assert_eq!(q.len(), 1, "queued in the paused workspace");
        assert!(q[0].starts_with("[from workspace dev / session orchestrator] HELLO-A"), "{}", q[0]);
        assert!(app.store.lock().unwrap().xws_rules.is_empty(), "allow once adds no rule");
        assert!(app.needs_of(&dev).open.iter().all(|v| v.id != need), "the entry closes");
        let audit = app.store.lock().unwrap().audit.iter().map(|a| a.2.clone()).collect::<Vec<_>>().join("\n");
        assert!(audit.contains("xws request") && audit.contains("xws allowed once and delivered"), "{audit}");
        assert!(audit.contains("xws received dev/orchestrator -> worker-a"), "the business target logs it too: {audit}");
        assert!(app.queues.lock().unwrap()["worker-a"].iter().all(|m| !m.id.is_empty()), "held messages get an id");
    }

    #[tokio::test]
    async fn deny_returns_the_note_and_sends_nothing() {
        let (app, _, biz, key) = app("deny");
        let h = spawn(&app, &key, msg("orchestrator", "HELLO-O"));
        let need = pending(&app).await;
        app.xws_user("deny", &json!({"id": need, "note": "not now, ask me tomorrow"})).unwrap();
        let e = h.await.unwrap().unwrap_err();
        assert!(e.contains("NOT sent") && e.contains("not now, ask me tomorrow"), "{e}");
        assert!(queued(&app, &orch_key(&biz)).is_empty());
        assert!(app.store.lock().unwrap().audit.iter().any(|a| a.2.contains("xws denied")));
    }

    #[tokio::test]
    async fn always_allow_skips_the_prompt_until_revoked() {
        let (app, dev, biz, key) = app("always");
        let h = spawn(&app, &key, msg("orchestrator", "ONE"));
        let need = pending(&app).await;
        app.xws_user("allow_always", &json!({"id": need})).unwrap();
        h.await.unwrap().unwrap();
        let rules = app.store.lock().unwrap().xws_rules.clone();
        assert_eq!(rules.len(), 1);
        assert!(same_path(&rules[0].from_ws, &dev) && same_path(&rules[0].to_ws, &biz));
        // Next time: straight through, no request.
        let r = app.message_session(&key, &msg("orchestrator", "TWO")).await.unwrap();
        assert!(r.contains("standing rule"), "{r}");
        assert_eq!(queued(&app, &orch_key(&biz)).len(), 2);
        // One direction only: biz → dev still asks (worker-a in biz → dev's orchestrator).
        let back = json!({"workspace": "dev", "session": "orchestrator", "message": "BACK", "why": "reply"});
        let h = spawn(&app, "worker-a", back);
        let n = pending(&app).await;
        app.xws_user("deny", &json!({"id": n})).unwrap();
        assert!(h.await.unwrap().is_err());
        // Revoked: it asks again.
        app.xws_user("rule_revoke", &json!({"id": rules[0].id})).unwrap();
        let h = spawn(&app, &key, msg("orchestrator", "THREE"));
        let n = pending(&app).await;
        app.xws_user("deny", &json!({"id": n})).unwrap();
        assert!(h.await.unwrap().is_err());
        assert_eq!(queued(&app, &orch_key(&biz)).len(), 2);
    }

    #[tokio::test]
    async fn a_timeout_fails_closed() {
        let (app, dev, _, key) = app("timeout");
        let e = app.message_session_with(&key, &msg("worker-a", "LATE"), Duration::from_millis(50)).await.unwrap_err();
        assert!(e.contains("NOT sent"), "{e}");
        assert!(queued(&app, "worker-a").is_empty());
        assert!(app.xws_pending.lock().unwrap().is_empty());
        assert!(app.needs_of(&dev).open.iter().all(|v| v.kind != "message"), "the entry closes");
        // A late click finds nothing to approve.
        assert!(app.xws_user("allow_once", &json!({"id": "n1"})).is_err());
    }

    #[tokio::test]
    async fn a_stopped_turn_fails_closed() {
        let (app, _, _, key) = app("stop");
        let h = spawn(&app, &key, msg("worker-a", "STOPPED"));
        let need = pending(&app).await;
        app.stop(&key);
        let e = h.await.unwrap().unwrap_err();
        assert!(e.contains("NOT sent"), "{e}");
        assert!(queued(&app, "worker-a").is_empty());
        assert!(app.xws_user("allow_once", &json!({"id": need})).is_err());
    }

    #[tokio::test]
    async fn same_workspace_and_unknown_targets_are_refused() {
        let (app, _, _, key) = app("same");
        let e = app.message_session(&key, &json!({"workspace": "dev", "session": "orchestrator", "message": "x", "why": "y"})).await.unwrap_err();
        assert!(e.contains("own workspace") && e.contains("send_to_thread"), "{e}");
        let e = app.message_session("worker-a", &json!({"workspace": "biz", "session": "orchestrator", "message": "x", "why": "y"})).await.unwrap_err();
        assert!(e.contains("message_orchestrator"), "{e}");
        assert!(app.message_session(&key, &msg("nobody", "x")).await.is_err());
        assert!(app.message_session(&key, &json!({"workspace": "biz", "session": "worker-a", "message": "x"})).await.is_err(), "why is required");
        assert!(app.xws_pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rules_from_the_window_need_two_open_workspaces() {
        let (app, dev, biz, key) = app("rules");
        assert!(app.xws_user("rule_add", &json!({"from": dev, "to": dev})).is_err());
        assert!(app.xws_user("rule_add", &json!({"from": dev, "to": "C:\\nowhere"})).is_err());
        app.xws_user("rule_add", &json!({"from": dev, "to": biz})).unwrap();
        assert!(app.xws_user("rule_add", &json!({"from": dev, "to": biz})).is_err(), "no duplicates");
        assert_eq!(app.xws_rules_json().as_array().unwrap().len(), 1);
        assert!(app.message_session(&key, &msg("worker-a", "RULED")).await.unwrap().contains("standing rule"));
        let ls = app.list_sessions(&key, None).unwrap();
        assert!(ls.contains("worker-a") && ls.contains("standing rule") && ls.contains("your own workspace"), "{ls}");
    }

    #[tokio::test]
    async fn a_target_gone_before_the_ok_gets_nothing() {
        let (app, dev, _, key) = app("gone");
        let h = spawn(&app, &key, msg("worker-a", "GONE"));
        let need = pending(&app).await;
        app.store.lock().unwrap().threads.retain(|t| t.name != "worker-a");
        app.xws_user("allow_once", &json!({"id": need})).unwrap();
        let e = h.await.unwrap().unwrap_err();
        assert!(e.contains("not sent: the target is gone"), "{e}");
        assert!(queued(&app, "worker-a").is_empty());
        assert!(app.needs_of(&dev).open.iter().all(|v| v.id != need));
        assert!(app.store.lock().unwrap().audit.iter().any(|a| a.2.contains("the target is gone")));
    }

    #[tokio::test]
    async fn an_orchestrator_target_sees_a_workspace_label_not_the_app() {
        let (app, dev, biz, key) = app("label");
        // Unpause biz so the message goes to the orchestrator's news, not its queue.
        app.store.lock().unwrap().projects.get_mut(&biz).unwrap().paused = false;
        app.xws_user("rule_add", &json!({"from": dev, "to": biz})).unwrap();
        // Hold the news: pretend the orchestrator is in wait_for_threads.
        app.waiting.lock().unwrap().insert(orch_key(&biz));
        app.message_session(&key, &msg("orchestrator", "LABELLED")).await.unwrap();
        let inbox = app.drain_inbox(&orch_key(&biz)).unwrap();
        assert!(inbox.starts_with("[from the workspace dev/orchestrator]"), "{inbox}");
        assert!(!inbox.contains("[from the app]"), "{inbox}");
    }

    async fn tool(app: &Arc<App>, key: &str, name: &str, args: Value) -> (bool, String) {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}});
        let r = crate::mcp::handle(app, key, &msg).await.unwrap();
        (r["result"]["isError"].as_bool().unwrap(), r["result"]["content"][0]["text"].as_str().unwrap().to_string())
    }

    /// Thread names are global, but an orchestrator only reaches its own workspace's
    /// threads; another workspace's need message_session (and the user's OK).
    #[tokio::test]
    async fn thread_tools_refuse_another_workspaces_thread() {
        let (app, dev, _, key) = app("own");
        {
            let mut st = app.store.lock().unwrap();
            let mut mine = st.get("worker-a").unwrap().clone();
            mine.name = "worker-d".into();
            mine.workspace = dev.clone();
            mine.cwd = dev.clone();
            st.threads.push(mine.clone());
            mine.name = "worker-none".into();
            mine.workspace = String::new();
            st.threads.push(mine);
        }
        let (err, text) = tool(&app, &key, "send_to_thread", json!({"name": "worker-none", "message": "X"})).await;
        assert!(err && text.contains("in this workspace"), "no workspace recorded: {text}");
        assert!(queued(&app, "worker-none").is_empty());
        for (name, args) in [
            ("send_to_thread", json!({"name": "worker-a", "message": "SNEAK"})),
            ("read_thread", json!({"name": "worker-a"})),
            ("configure_thread", json!({"name": "worker-a", "model": "haiku"})),
            ("resolve_thread", json!({"name": "worker-a"})),
            ("resolve_thread", json!({"name": "worker-a", "reopen": true})),
            ("stop_thread", json!({"name": "worker-a"})),
            ("rate_thread", json!({"name": "worker-a", "verdict": "good"})),
        ] {
            let (err, text) = tool(&app, &key, name, args).await;
            assert!(err && text.contains("in this workspace"), "{name}: {text}");
        }
        assert!(queued(&app, "worker-a").is_empty(), "nothing queued for the other workspace's thread");
        assert!(!app.store.lock().unwrap().get("worker-a").unwrap().resolved);
        let (_, text) = tool(&app, &key, "wait_for_threads", json!({"names": ["worker-a"], "timeout_seconds": 1})).await;
        assert!(!text.contains("worker-a"), "{text}");
        let (err, text) = tool(&app, &key, "read_thread", json!({"name": "worker-d"})).await;
        assert!(!err && text.contains("worker-d"), "own thread still works: {text}");
        let (_, text) = tool(&app, &key, "list_threads", json!({})).await;
        assert!(text.contains("worker-d") && !text.contains("worker-a"), "{text}");
    }

    #[tokio::test]
    async fn dismissing_a_message_entry_denies_it() {
        let (app, _, _, key) = app("dismiss");
        let h = spawn(&app, &key, msg("worker-a", "DISMISSED"));
        let need = pending(&app).await;
        app.need_user("dismiss", &need, None, &json!({"note": "no"})).unwrap();
        let e = h.await.unwrap().unwrap_err();
        assert!(e.contains("NOT sent") && e.contains("no"), "{e}");
        assert!(queued(&app, "worker-a").is_empty());
    }
}
