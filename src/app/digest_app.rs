//! Notifications, digests and busy mode, wired into the app. What waits and what goes out
//! right away is decided in digest.rs; sending (toast, phone push) is in notify.rs.

use super::App;
use crate::digest::{self, Held, Level};
use crate::store::now;
use serde_json::{json, Value};

/// How many held notifications the window lists (the count covers all of them).
const HELD_SHOWN: usize = 20;

impl App {
    /// Notify the user about a business event: a Windows toast, and a phone push if they set
    /// one up. Only events they chose; only a redacted summary line. Non-urgent ones wait
    /// for the next digest, and in busy mode everything but deadline risk waits.
    pub fn notify(&self, ws: &str, event: &str, title: &str, body: &str) {
        self.notify_at(ws, event, digest::level_of(event), title, body);
    }

    /// `notify` with an explicit level (e.g. a budget hard stop is Urgent).
    pub fn notify_at(&self, ws: &str, event: &str, level: Level, title: &str, body: &str) {
        let hold = {
            let mut st = self.store.lock().unwrap();
            let busy = st.busy_on;
            let Some(n) = st.notes(ws).filter(|n| n.kind == "business") else { return };
            // The user's event choices cover the events they can choose (notify::EVENTS);
            // others (Needs you, phone pauses, the test) always go.
            let choosable = crate::notify::EVENTS.iter().any(|(e, _)| *e == event);
            if choosable && n.notify_set && !n.notify_events.iter().any(|e| e == event) {
                return;
            }
            let digests_on = !digest::effective_times(n.digest_set, &n.digest_times).is_empty();
            let hold = digest::holds(level, busy, digests_on);
            if hold {
                let (t, b) = crate::notify::summary_line(title, body);
                st.notes_mut(ws).held.push(Held { ts: now(), event: event.to_string(), title: t, body: b });
                st.save();
            }
            hold
        };
        if hold {
            self.touch();
        } else {
            self.notify_send(ws, title, body);
        }
    }

    /// Send a notification now (toast and phone push).
    /// Toast, plus the phone push if one is set up. False when the phone push couldn't be
    /// sent (its secrets couldn't be read).
    fn notify_send(&self, ws: &str, title: &str, body: &str) -> bool {
        let phone = {
            let st = self.store.lock().unwrap();
            let Some(n) = st.notes(ws) else { return true };
            n.notify_phone.clone()
        };
        let name = std::path::Path::new(ws).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let title = format!("{name}: {title}");
        crate::notify::toast(&title, body);
        if !phone.is_empty() {
            let names: Vec<String> = crate::notify::phone_secrets(&phone).iter().map(|s| s.to_string()).collect();
            match self.vault.get(ws, &names) {
                Ok(secrets) => crate::notify::push(&phone, &secrets, &title, body),
                Err(e) => {
                    self.hub.broadcast(json!({"kind": "toast", "text": format!("phone notification not sent: {e}")}));
                    return false;
                }
            }
        }
        true
    }

    /// Send what's held for `ws` as one digest; returns how many it covered.
    pub fn send_digest(&self, ws: &str, busy_ended: bool) -> usize {
        let held = {
            let mut st = self.store.lock().unwrap();
            let n = st.notes_mut(ws);
            n.digest_last = now();
            let held = std::mem::take(&mut n.held);
            st.save();
            held
        };
        if held.is_empty() {
            return 0;
        }
        let (title, body) = digest::digest_text(&held, busy_ended);
        if !self.notify_send(ws, &title, &body) {
            // The phone didn't get it: keep the items for the next digest.
            let mut st = self.store.lock().unwrap();
            let n = st.notes_mut(ws);
            let newer = std::mem::take(&mut n.held);
            n.held = held.iter().cloned().chain(newer).collect();
            st.save();
        }
        self.business_event(ws, "digest", Some("digest"), json!({"count": held.len(), "text": format!("{title}: {body}")}));
        self.touch();
        held.len()
    }

    fn business_workspaces(&self) -> Vec<String> {
        let st = self.store.lock().unwrap();
        st.projects.iter().filter(|(_, n)| n.kind == "business").map(|(k, _)| k.clone()).collect()
    }

    /// Every 30 s: end busy mode when its time is up, and send digests that are due.
    pub fn digest_tick(&self) {
        let t = now();
        let (busy, expired) = {
            let st = self.store.lock().unwrap();
            (st.busy_on, st.busy_on && st.busy_until > 0.0 && t >= st.busy_until)
        };
        if expired {
            self.set_busy(false, 0.0, "time");
            return;
        }
        if busy {
            return;
        }
        for ws in self.business_workspaces() {
            let (times, last, held) = {
                let st = self.store.lock().unwrap();
                let Some(n) = st.notes(&ws) else { continue };
                (digest::effective_times(n.digest_set, &n.digest_times), n.digest_last, n.held.len())
            };
            if times.is_empty() {
                // Digests were turned off with some still held: send them now.
                if held > 0 {
                    self.send_digest(&ws, false);
                }
                continue;
            }
            if digest::due_slot(&times, last, t).is_some() {
                // Mark the slot even when nothing's held, so a later hold waits for the next one.
                self.send_digest(&ws, false);
            }
        }
    }

    /// Turn busy mode on (for `minutes`, 0 = until turned off) or off. Turning it off
    /// sends one digest per workspace of what was held. Returns a line for the user.
    pub fn set_busy(&self, on: bool, minutes: f64, by: &str) -> String {
        let t = now();
        let was = {
            let mut st = self.store.lock().unwrap();
            let was = st.busy_on;
            st.busy_on = on;
            if on {
                st.busy_since = t;
                st.busy_until = if minutes > 0.0 { t + minutes * 60.0 } else { 0.0 };
            } else {
                st.busy_until = 0.0;
            }
            st.save();
            was
        };
        let wss = self.business_workspaces();
        let mut sent = 0;
        if !on && was {
            for ws in &wss {
                sent += self.send_digest(ws, true);
            }
        }
        let busy = self.busy_json();
        for ws in &wss {
            let held = self.store.lock().unwrap().notes(ws).map_or(0, |n| n.held.len());
            let mut ev = busy.clone();
            ev["held"] = json!(held);
            self.business_event(ws, "busy_changed", None, ev);
            if on != was {
                let line = if on {
                    format!("BUSY MODE ON by={by}{}", if minutes > 0.0 { format!(" for {minutes:.0} min") } else { String::new() })
                } else {
                    format!("BUSY MODE OFF by={by}")
                };
                self.audit(ws, &line);
            }
        }
        self.touch();
        if on {
            if minutes > 0.0 { format!("Busy mode on for {minutes:.0} min: only deadline risks come through") } else { "Busy mode on: only deadline risks come through".into() }
        } else if sent > 0 {
            format!("Busy mode off: sent a digest of {sent} held notification{}", if sent == 1 { "" } else { "s" })
        } else {
            "Busy mode off: nothing was held".into()
        }
    }

    /// Busy mode for the window and the feed.
    pub fn busy_json(&self) -> Value {
        let st = self.store.lock().unwrap();
        json!({
            "on": st.busy_on,
            "since": if st.busy_on { json!(st.busy_since) } else { Value::Null },
            "until": if st.busy_on && st.busy_until > 0.0 { json!(st.busy_until) } else { Value::Null },
        })
    }

    /// Busy mode plus how much is held across business workspaces (window state).
    pub fn busy_state_json(&self) -> Value {
        let mut v = self.busy_json();
        let held: usize = self.store.lock().unwrap().projects.values().filter(|n| n.kind == "business").map(|n| n.held.len()).sum();
        v["held"] = json!(held);
        v
    }

    /// Digest settings and what's held, for the shown workspace's `project` state.
    pub fn digest_json(&self, ws: &str) -> Value {
        let st = self.store.lock().unwrap();
        let n = st.notes(ws).cloned().unwrap_or_default();
        let times = digest::effective_times(n.digest_set, &n.digest_times);
        let start = n.held.len().saturating_sub(HELD_SHOWN);
        json!({
            "digest_times": times,
            "held": n.held.len(),
            "held_items": &n.held[start..],
            "next_digest": digest::next_slot(&times, now()),
        })
    }

    /// Digest times for the shown workspace (from the window only). [] turns digests off.
    pub fn set_digests(&self, times: Vec<String>) -> Result<(), String> {
        let ws = self.workspace();
        if !self.is_business(&ws) {
            return Err("digests are for business workspaces".into());
        }
        let times = digest::parse_times(&times)?;
        {
            let mut st = self.store.lock().unwrap();
            let n = st.notes_mut(&ws);
            n.digest_set = true;
            n.digest_times = times.clone();
            // Don't send a digest for a slot that passed before this change.
            n.digest_last = n.digest_last.max(now());
            st.save();
        }
        self.audit(&ws, &format!("DIGESTS SET times=[{}] by=user", times.join(",")));
        self.touch();
        Ok(())
    }

    /// Send the shown workspace's held notifications now (from the window).
    pub fn digest_now(&self) -> usize {
        let ws = self.workspace();
        // digest_now keeps digest_last as now: the next slot still goes out on time.
        self.send_digest(&ws, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    /// A real App on a paused temp business workspace (no CLI turn starts).
    fn test_app() -> Arc<App> {
        let root = std::env::temp_dir().join(format!("orch-digest-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("biz");
        std::fs::create_dir_all(&ws).unwrap();
        let ws = ws.to_string_lossy().to_string();
        let state = json!({"workspace": ws, "open_workspaces": [ws], "projects": {ws.clone(): {"kind": "business", "paused": true}}});
        std::fs::write(root.join("state.json"), state.to_string()).unwrap();
        App::new(Paths { config_dir: root.clone(), data_dir: root.clone() }, "t".repeat(40))
    }

    fn business(app: &App) -> String {
        app.workspace()
    }

    fn held(app: &App, ws: &str) -> Vec<Held> {
        app.store.lock().unwrap().notes(ws).map(|n| n.held.clone()).unwrap_or_default()
    }

    #[test]
    fn non_urgent_notifications_wait_for_the_digest() {
        let app = test_app();
        let ws = business(&app);
        app.notify(&ws, "schedule", "Scheduled run finished", "ok");
        app.notify(&ws, "budget", "Budget alert", "at 80%");
        assert_eq!(held(&app, &ws).len(), 2);
        // Urgent ones go straight out.
        app.notify(&ws, "urgent", "Urgent: waiting", "q1");
        app.notify(&ws, "deadline", "Order may miss", "late");
        app.notify_at(&ws, "budget", Level::Urgent, "Budget alert", "reached");
        assert_eq!(held(&app, &ws).len(), 2);
        // A digest sends them and empties the list.
        assert_eq!(app.send_digest(&ws, false), 2);
        assert!(held(&app, &ws).is_empty());
        assert_eq!(app.send_digest(&ws, false), 0);
    }

    #[test]
    fn held_notifications_survive_until_sent() {
        let app = test_app();
        let ws = business(&app);
        for i in 0..50 {
            app.notify(&ws, "schedule", &format!("run {i}"), "done");
        }
        assert_eq!(held(&app, &ws).len(), 50);
        // The tick only sends at a slot; with the last digest just now, nothing goes.
        app.store.lock().unwrap().notes_mut(&ws).digest_last = now();
        app.digest_tick();
        assert_eq!(held(&app, &ws).len(), 50);
        let d = app.digest_json(&ws);
        assert_eq!(d["held"], 50);
        assert_eq!(d["held_items"].as_array().unwrap().len(), HELD_SHOWN);
        // A missed slot (last digest two days ago) sends them all at once.
        app.store.lock().unwrap().notes_mut(&ws).digest_last = now() - 2.0 * 86400.0;
        app.digest_tick();
        assert!(held(&app, &ws).is_empty());
    }

    /// The phone push can't be sent (its secret is missing): the items stay for next time.
    #[test]
    fn a_failed_phone_push_keeps_the_held_items() {
        let app = test_app();
        let ws = business(&app);
        app.notify(&ws, "schedule", "held one", "x");
        app.store.lock().unwrap().notes_mut(&ws).notify_phone = "ntfy".into();
        app.send_digest(&ws, false);
        assert_eq!(held(&app, &ws).len(), 1, "kept for the next digest");
        app.store.lock().unwrap().notes_mut(&ws).notify_phone.clear();
        assert_eq!(app.send_digest(&ws, false), 1);
        assert!(held(&app, &ws).is_empty());
    }

    #[test]
    fn digests_off_send_everything_now() {
        let app = test_app();
        let ws = business(&app);
        app.notify(&ws, "schedule", "held", "x");
        app.set_digests(vec![]).unwrap();
        app.notify(&ws, "schedule", "not held", "x");
        assert_eq!(held(&app, &ws).len(), 1);
        app.digest_tick();
        assert!(held(&app, &ws).is_empty());
        assert!(app.set_digests(vec!["7pm".into()]).is_err());
        app.set_digests(vec!["18:00".into(), "08:30".into()]).unwrap();
        assert_eq!(app.digest_json(&ws)["digest_times"], json!(["08:30", "18:00"]));
    }

    #[test]
    fn busy_mode_holds_all_but_deadline_risk_and_sends_one_digest_after() {
        let app = test_app();
        let ws = business(&app);
        app.set_digests(vec![]).unwrap();
        app.set_busy(true, 0.0, "user");
        assert_eq!(app.busy_json()["on"], true);
        app.notify(&ws, "urgent", "Urgent: waiting", "q1");
        app.notify(&ws, "needs_you", "Needs you now", "a");
        app.notify(&ws, "schedule", "run", "done");
        app.notify(&ws, "pause", "Paused from phone", "x");
        app.notify(&ws, "deadline", "Order may miss", "late");
        assert_eq!(held(&app, &ws).len(), 4);
        // The tick doesn't send digests while busy.
        app.digest_tick();
        assert_eq!(held(&app, &ws).len(), 4);
        assert_eq!(app.busy_state_json()["held"], 4);
        let msg = app.set_busy(false, 0.0, "user");
        assert!(msg.contains("4 held"), "{msg}");
        assert!(held(&app, &ws).is_empty());
        assert_eq!(app.busy_json()["on"], false);
    }

    #[test]
    fn busy_mode_ends_by_itself() {
        let app = test_app();
        let ws = business(&app);
        app.set_busy(true, 30.0, "user");
        assert!(app.busy_json()["until"].as_f64().unwrap() > now());
        app.notify(&ws, "urgent", "Urgent", "x");
        app.store.lock().unwrap().busy_until = now() - 1.0;
        app.digest_tick();
        assert_eq!(app.busy_json()["on"], false);
        assert!(held(&app, &ws).is_empty());
    }

    #[test]
    fn unchosen_events_are_dropped_but_needs_you_is_not() {
        let app = test_app();
        let ws = business(&app);
        app.set_busy(true, 0.0, "user");
        {
            let mut st = app.store.lock().unwrap();
            let n = st.notes_mut(&ws);
            n.notify_set = true;
            n.notify_events = vec!["deadline".into()];
        }
        app.notify(&ws, "schedule", "run", "done");
        app.notify(&ws, "needs_you", "Needs you now", "a");
        let h = held(&app, &ws);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].event, "needs_you");
    }
}
