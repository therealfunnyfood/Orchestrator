//! The tests the Needs you spec asks for, on a real App.

use super::*;
use crate::config::Paths;

/// A real App on a paused temp business workspace (plus a development one): messages
/// to agents wait in app.queues, so no CLI turn starts.
fn app(name: &str) -> (Arc<App>, String, String) {
    let root = std::env::temp_dir().join(format!("orch-needs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mk = |n: &str| {
        let p = root.join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().to_string()
    };
    let (ws, dev) = (mk("biz"), mk("dev"));
    let state = json!({
        "workspace": ws, "open_workspaces": [ws, dev],
        "projects": {ws.clone(): {"kind": "business", "paused": true}, dev.clone(): {}},
    });
    std::fs::write(root.join("state.json"), state.to_string()).unwrap();
    let app = App::new(Paths { config_dir: root.clone(), data_dir: root.clone() }, "t".repeat(40));
    (app, ws, dev)
}

/// Everything waiting for the agent behind `key` (its queue and its inbox).
fn told(app: &App, key: &str) -> String {
    let q: Vec<String> = app.queues.lock().unwrap().get(key).map(|q| q.iter().map(|m| m.text.clone()).collect()).unwrap_or_default();
    let i: Vec<String> = app.inbox.lock().unwrap().get(key).map(|m| m.iter().map(|(_, t)| t.clone()).collect()).unwrap_or_default();
    format!("{}\n{}", q.join("\n"), i.join("\n"))
}

fn action_args() -> Value {
    json!({"goal": "Make the itch.io account", "tldr": "Make your itch.io account", "why": "the game page needs it",
        "minutes": 10, "priority": "urgent", "done_when": "You see your dashboard",
        "steps": [{"do": "Open itch.io/register", "expect": "A sign-up form"}, {"do": "Pick a username", "expect": "It's accepted", "user_types_this": true}]})
}

fn id_of(out: &str) -> String {
    out.split_whitespace().nth(1).unwrap().to_string()
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_timed_out_question_stays_open_with_its_default_and_a_late_answer_reaches_the_agent() {
    let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ws, _) = app("timeout");
    let k = orch_key(&ws);
    let q = json!([{"question": "Which board?", "options": [{"label": "Cats"}, {"label": "Mugs (Recommended)"}]}]);
    let (a, k2, q2) = (app.clone(), k.clone(), q.clone());
    let h = tokio::spawn(async move { a.ask_user(&k2, q2).await });
    settle().await;
    let open = app.needs_of(&ws).open;
    assert_eq!(open.len(), 1);
    assert!(open[0].waiting && !open[0].timed_out && open[0].kind == "question");
    assert!(open[0].msg_ts > 0.0, "jump to chat");
    // The agent stops waiting (the 1 h tool timeout, or its turn was stopped).
    h.abort();
    settle().await;
    let v = app.needs_of(&ws).open.into_iter().next().expect("still open");
    assert!(v.timed_out && !v.waiting);
    assert_eq!(v.default_answer, "Mugs");
    assert!(std::fs::read_to_string(std::path::Path::new(&ws).join(needs::MIRROR)).unwrap().contains("Went ahead with: Mugs"));
    // Answering it now sends the agent the change.
    app.need_answer(&v.id, json!([{"selected": ["Cats"]}])).unwrap();
    assert!(app.needs_of(&ws).open.is_empty());
    let t = told(&app, &k);
    assert!(t.contains("[late answer from the user]") && t.contains("\"Mugs\"") && t.contains("A: Cats"), "{t}");
    // A permission prompt that expires just closes.
    let (a, k2) = (app.clone(), k.clone());
    let h = tokio::spawn(async move {
        a.ask_user_with(&k2, json!([{"question": "Allow Bash?", "options": [{"label": "Allow"}, {"label": "Deny"}]}]), json!({"permission": {"tool": "Bash"}})).await
    });
    settle().await;
    assert_eq!(app.needs_of(&ws).open.len(), 1);
    h.abort();
    settle().await;
    assert!(app.needs_of(&ws).open.is_empty(), "permission prompts close when they expire");
}

#[tokio::test]
async fn a_live_answer_closes_the_question() {
    let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ws, _) = app("live");
    let k = orch_key(&ws);
    let (a, k2) = (app.clone(), k.clone());
    let h = tokio::spawn(async move { a.ask_user(&k2, json!([{"question": "Go?", "options": [{"label": "Yes"}, {"label": "No"}]}])).await });
    settle().await;
    let id = app.needs_of(&ws).open[0].id.clone();
    app.need_user("answer", &id, None, &json!({"answers": [{"selected": ["Yes"]}]})).unwrap();
    let got = h.await.unwrap().unwrap();
    assert_eq!(got[0]["selected"][0], "Yes");
    let n = app.needs_of(&ws);
    assert!(n.open.is_empty());
    assert_eq!(n.done_today[0].answer, "Q: Go?\nA: Yes");
}

#[tokio::test]
async fn a_queue_item_appears_once_and_goes_when_decided() {
    let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ws, _) = app("queue");
    let k = orch_key(&ws);
    let base = json!({"type": "post_pin", "line": "C", "summary": "Pin the cat mug", "payload": "board: cats", "risk": "low"});
    let mut missing = base.clone();
    missing["tldr"] = json!("Pin it");
    assert!(app.queue_add(&k, &missing, vec![]).unwrap_err().contains("required"));
    let mut ok = base.clone();
    ok["tldr"] = json!("Pin the cat mug to Cats");
    ok["recommend"] = json!("approve");
    ok["why"] = json!("cats pins sold 3 mugs last week");
    let qid = id_of(&app.queue_add(&k, &ok, vec![]).unwrap());
    let item = app.store.lock().unwrap().queue.iter().find(|i| i.id == qid).cloned().unwrap();
    assert_eq!(item.hash_v, 3);
    let open = app.needs_of(&ws).open;
    assert_eq!(open.iter().filter(|v| v.queue_item == qid).count(), 1, "once");
    assert_eq!(open[0].id, format!("queue:{qid}"));
    assert_eq!(open[0].queue["payload_hash"], json!(crate::queue::content_hash(&item)));
    assert!(app.need_user("dismiss", &open[0].id, None, &json!({})).is_err(), "decisions are decided, not dismissed");
    // Decided in the Queue tab (or from Needs you: both go through queue_decide).
    app.queue_decide(&qid, "reject", "off-brand", None, false, "window", None).unwrap();
    let n = app.needs_of(&ws);
    assert!(n.open.is_empty(), "gone once decided");
    assert_eq!(n.done_today[0].note, "rejected");
    // A manual task: a decision first, then (approved) a task the user marks done here.
    let mut manual = ok.clone();
    manual["who_executes"] = json!("user");
    assert!(app.queue_add(&k, &manual, vec![]).unwrap_err().contains("step-by-step plan"));
    manual["steps"] = json!([{"do": "Open Pinterest", "expect": "Your boards"}]);
    manual["done_when"] = json!("The pin is on Cats");
    let mid = id_of(&app.queue_add(&k, &manual, vec![]).unwrap());
    let h = app.store.lock().unwrap().queue.iter().find(|i| i.id == mid).map(crate::queue::content_hash).unwrap();
    app.queue_decide(&mid, "approve", "", None, false, "window", Some(&h)).unwrap();
    let open = app.needs_of(&ws).open;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, format!("manual:{mid}"));
    assert_eq!(open[0].kind, "manual");
    assert_eq!(open[0].plan.as_ref().unwrap().steps.len(), 1);
    app.need_user("done", &format!("manual:{mid}"), Some(&ws), &json!({})).unwrap();
    assert!(app.needs_of(&ws).open.is_empty());
    assert_eq!(app.store.lock().unwrap().queue.iter().find(|i| i.id == mid).unwrap().status, "done");
}

#[tokio::test]
async fn an_action_done_in_the_panel_tells_the_agent_and_the_mirror_follows() {
    let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ws, dev) = app("action");
    let k = orch_key(&ws);
    let mirror = std::path::Path::new(&ws).join(needs::MIRROR);
    let id = id_of(&app.need_action(&k, &action_args()).unwrap());
    let text = std::fs::read_to_string(&mirror).unwrap();
    assert!(text.contains("**Make your itch.io account** (to do, about 10 min)"), "{text}");
    let v = app.needs_of(&ws).open[0].clone();
    assert!(v.msg_ts > 0.0 && v.priority == "urgent" && v.plan.is_some());
    // A stuck note reaches the agent with the step; the entry stays open.
    app.need_user("stuck", &id, Some(&ws), &json!({"step": 1, "note": "the name is taken"})).unwrap();
    assert_eq!(app.needs_of(&ws).open[0].step, 1);
    let t = told(&app, &k);
    assert!(t.contains("step 2 of 2") && t.contains("the name is taken"), "{t}");
    // A workspace that isn't the entry's is refused.
    assert!(app.need_user("done", &id, Some(&dev), &json!({})).is_err());
    app.need_user("done", &id, Some(&ws), &json!({})).unwrap();
    let t = told(&app, &k);
    assert!(t.contains("[done by the user]"), "{t}");
    let text = std::fs::read_to_string(&mirror).unwrap();
    assert!(text.contains("Nothing is waiting on you.") && text.contains("## Done today\n\n- [x] **Make your itch.io account**"), "{text}");
    assert!(std::fs::metadata(&mirror).unwrap().permissions().readonly(), "read-only");
    // Development workspaces get no stray file.
    app.need_action(&orch_key(&dev), &action_args()).unwrap();
    assert!(!std::path::Path::new(&dev).join(needs::MIRROR).exists());
    assert_eq!(app.needs_counts(&dev), (1, 1));
    assert_eq!(app.needs_all_json()["count"], 1);
}

#[tokio::test]
async fn snooze_hides_an_entry_until_its_time() {
    let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ws, _) = app("snooze");
    let k = orch_key(&ws);
    let id = app.need_heads_up(&k, "Your Etsy shop renews tomorrow", &json!({}), 0.0);
    app.need_user("snooze", &id, None, &json!({"until": "1h"})).unwrap();
    let n = app.needs_of(&ws);
    assert!(n.open.is_empty() && n.snoozed.len() == 1 && n.snoozed[0].snoozed_until > now() + 3000.0);
    assert!(app.need_user("snooze", &id, None, &json!({"until": "soon"})).is_err());
    // Time passes.
    app.store.lock().unwrap().need_marks.get_mut(&id).unwrap().snooze_until = now() - 1.0;
    app.needs_tick();
    assert_eq!(app.needs_of(&ws).open.len(), 1);
    assert_eq!(app.store.lock().unwrap().need_marks[&id].snooze_until, 0.0);
    // Pins put an entry in Today first.
    let a2 = app.need_heads_up(&k, "Second", &json!({}), 0.0);
    app.need_user("pin", &a2, None, &json!({"pin": 1})).unwrap();
    assert_eq!(app.needs_of(&ws).today[0], a2);
}

#[tokio::test]
async fn an_agent_dismissal_records_its_reason() {
    let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ws, _) = app("dismiss");
    let k = orch_key(&ws);
    let id = id_of(&app.need_action(&k, &action_args()).unwrap());
    assert!(app.need_dismiss_agent("some-thread", &id, "nope").is_err(), "not a thread's to dismiss");
    assert!(app.need_dismiss_agent(&k, &id, " ").is_err(), "a reason is required");
    app.need_dismiss_agent(&k, &id, "the account already exists").unwrap();
    let n = app.needs_of(&ws);
    assert!(n.open.is_empty());
    let d = &n.done_today[0];
    assert_eq!((d.status.as_str(), d.closed_by.as_str(), d.note.as_str()), ("dismissed", "agent", "the account already exists"));
    assert!(std::fs::read_to_string(std::path::Path::new(&ws).join(needs::MIRROR)).unwrap().contains("(the account already exists)"));
    let listed = app.need_list(&k, None).unwrap();
    assert!(listed.contains("the account already exists"), "{listed}");
    // The orchestrator's Today picks; threads can't set them.
    let b = app.need_heads_up(&k, "Read the new terms", &json!({}), 0.0);
    app.need_list(&k, Some(vec![b.clone()])).unwrap();
    assert!(app.needs_of(&ws).open[0].agent_today);
    assert!(app.need_list("some-thread", Some(vec![b])).is_err());
}

#[tokio::test]
async fn after_a_restart_open_questions_count_as_timed_out() {
    let _g = crate::vault::tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ws, _) = app("restart");
    let k = orch_key(&ws);
    // An entry whose question id the app doesn't know (as after a restart).
    app.need_question(&k, "q-gone", &json!([{"question": "Price?", "options": [{"label": "$16 (Recommended)"}, {"label": "$18"}]}]), false, 0.0);
    let v = app.needs_of(&ws).open[0].clone();
    assert!(v.timed_out && !v.waiting && v.default_answer == "$16");
    app.need_answer(&v.id, json!([{"selected": ["$18"]}])).unwrap();
    assert!(told(&app, &k).contains("[late answer from the user]"));
}
