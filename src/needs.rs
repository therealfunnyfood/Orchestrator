//! "Needs you": everything waiting on the user, kept by the app instead of the chat.
//!
//! Stored entries are questions (ask_user), action steps (request_user_action) and
//! heads-ups (tell_user important). Decisions and manual tasks are not stored: they're
//! read from the approval queue each time, so the queue and this list can't disagree.
//! Per-entry marks (snooze, Today pin, the agent's Today pick) live in `Mark`, keyed by
//! the entry id, so they work for queue entries too.

use chrono::{Datelike, Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PRIORITIES: [&str; 4] = ["urgent", "today", "week", "whenever"];
pub const RECOMMENDS: [&str; 5] = ["approve", "reject", "edit", "do", "skip"];
pub const MIRROR: &str = "TODO-FOR-YOU.md";
/// At most this many entries show under "Today".
pub const TODAY_MAX: usize = 3;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Need {
    pub id: String,
    pub workspace: String,
    /// question | action | heads_up
    pub kind: String,
    pub title: String,
    /// One line, plain words.
    pub tldr: String,
    pub details: String,
    pub why: String,
    /// approve | reject | edit | do | skip (or empty)
    pub recommend: String,
    pub minutes: u32,
    /// urgent | today | week | whenever
    pub priority: String,
    /// The thread (or orchestrator) key that asked; it hears about answers and Done.
    pub source: String,
    pub created_at: f64,
    /// The chat event it came from, for "Jump to chat".
    pub msg_ts: f64,
    /// open | done | dismissed
    pub status: String,
    pub note: String,
    /// user | agent | app
    pub closed_by: String,
    pub closed_at: f64,
    /// question: the ask_user id.
    pub ref_id: String,
    pub questions: Value,
    /// question: a permission or effort prompt (closes when it expires, no default).
    pub permission: bool,
    /// question: the agent stopped waiting and went ahead with its default.
    pub timed_out: bool,
    pub default_answer: String,
    pub answer: String,
    /// action: free-text notes and an optional link (a URL or a file in the workspace).
    pub steps: String,
    pub link: String,
    /// action: the step-by-step plan.
    pub plan: Option<Plan>,
}

/// The most steps one plan may have: longer tasks are split into parts.
pub const MAX_STEPS: usize = 12;

/// One step of a task the user does by hand (request_user_action, manual queue items).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Step {
    #[serde(rename = "do")]
    pub action: String,
    /// "You should see: …"
    pub expect: String,
    /// Text for a copy button.
    pub copy: String,
    /// A link for an open button.
    pub url: String,
    /// What the user types themselves (never pasted for them), or "yes".
    pub user_types_this: String,
}

/// A precise step-by-step plan, shown one step at a time.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Plan {
    pub goal: String,
    pub before_you_start: String,
    pub steps: Vec<Step>,
    /// Shown on the finish screen: "Did you see that?"
    pub done_when: String,
    pub if_stuck: String,
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::trim).collect::<Vec<_>>().join("\n"),
        Some(Value::Bool(true)) => "yes".into(),
        _ => String::new(),
    }
}

/// A plan from tool arguments: goal, before_you_start, steps [{do, expect, copy?, url?,
/// user_types_this?}], done_when, if_stuck. Every step needs `do` and `expect`.
pub fn parse_plan(args: &Value) -> Result<Plan, String> {
    let steps = args.get("steps").and_then(Value::as_array).ok_or("steps is required: a list of {do, expect, copy?, url?, user_types_this?}")?;
    if steps.is_empty() {
        return Err("steps is empty: give at least one {do, expect}".into());
    }
    if steps.len() > MAX_STEPS {
        return Err(format!("{} steps is too many (at most {MAX_STEPS}): split the task into parts, one request each", steps.len()));
    }
    let mut out = vec![];
    for (i, s) in steps.iter().enumerate() {
        let step = Step {
            action: text_of(s.get("do")),
            expect: text_of(s.get("expect")),
            copy: text_of(s.get("copy")),
            url: text_of(s.get("url")),
            user_types_this: text_of(s.get("user_types_this")),
        };
        if step.action.is_empty() || step.expect.is_empty() {
            return Err(format!("step {} needs both do (one exact action) and expect (what the user should see after it)", i + 1));
        }
        out.push(step);
    }
    let plan = Plan {
        goal: text_of(args.get("goal")),
        before_you_start: text_of(args.get("before_you_start")),
        steps: out,
        done_when: text_of(args.get("done_when")),
        if_stuck: text_of(args.get("if_stuck")),
    };
    if plan.goal.is_empty() || plan.done_when.is_empty() {
        return Err("goal and done_when are required (what this achieves, and how the user can tell it worked)".into());
    }
    Ok(plan)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Mark {
    /// Step-by-step progress: the step the user is on (0-based), kept across "Stop here".
    pub step: usize,
    /// 1 = the user pinned it to Today, -1 = the user took it off, 0 = no choice.
    pub pin: i8,
    /// The agent picked it for Today (list_needs_you today).
    pub today: bool,
    pub snooze_until: f64,
}

/// What the window, the feed, the mirror and the agents see.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct View {
    pub id: String,
    pub ws: String,
    /// question | decision | manual | action | heads_up
    pub kind: String,
    pub title: String,
    pub tldr: String,
    pub details: String,
    pub why: String,
    pub recommend: String,
    pub minutes: u32,
    pub priority: String,
    pub source: String,
    pub created_at: f64,
    pub msg_ts: f64,
    /// open | snoozed | done | dismissed
    pub status: String,
    pub snoozed_until: f64,
    /// question: still waiting (answer goes straight to the agent) or timed out.
    pub waiting: bool,
    pub timed_out: bool,
    pub default_answer: String,
    pub questions: Value,
    pub steps: String,
    pub link: String,
    /// action / manual: the step-by-step plan and the step the user is on.
    pub plan: Option<Plan>,
    pub step: usize,
    /// decision / manual: the queue item id, and the item as the Queue tab shows it.
    pub queue_item: String,
    pub queue: Value,
    /// High risk, spending or a failed capacity check: never a one-key approve.
    pub careful: bool,
    pub pin: i8,
    pub agent_today: bool,
    pub note: String,
    pub closed_by: String,
    pub closed_at: f64,
    pub answer: String,
}

pub fn rank(priority: &str) -> u8 {
    match priority {
        "urgent" => 0,
        "today" => 1,
        "week" => 2,
        "whenever" => 3,
        _ => 2,
    }
}

pub fn priority_or_default(p: &str) -> String {
    let p = p.trim().to_lowercase();
    let p = match p.as_str() {
        "this week" | "this_week" => "week".to_string(),
        _ => p,
    };
    if PRIORITIES.contains(&p.as_str()) { p } else { "today".into() }
}

pub fn label(priority: &str) -> &'static str {
    match priority {
        "urgent" => "Urgent",
        "today" => "Today",
        "week" => "This week",
        _ => "Whenever",
    }
}

/// Urgent first, then oldest first within a priority.
pub fn sort(v: &mut [View]) {
    v.sort_by(|a, b| rank(&a.priority).cmp(&rank(&b.priority)).then(a.created_at.total_cmp(&b.created_at)));
}

/// Today's 3: the user's pins, then the agent's picks, then the top of the list; never one
/// the user took off. `open` must be sorted.
pub fn today(open: &[View]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut take = |pred: &dyn Fn(&View) -> bool| {
        for v in open.iter().filter(|v| pred(v)) {
            if out.len() < TODAY_MAX && !out.contains(&v.id) {
                out.push(v.id.clone());
            }
        }
    };
    take(&|v| v.pin == 1);
    take(&|v| v.agent_today && v.pin != -1);
    take(&|v| v.pin != -1);
    out
}

/// The answer an agent assumes when it stops waiting: each question's recommended option
/// (or its first).
pub fn default_answer(questions: &Value) -> String {
    let qs = questions.as_array().cloned().unwrap_or_default();
    qs.iter()
        .map(|q| {
            let opts = q.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
            let label = |o: &Value| o.get("label").and_then(Value::as_str).unwrap_or("").to_string();
            opts.iter().find(|o| label(o).contains("(Recommended)")).or(opts.first()).map(label).unwrap_or_default()
                .replace(" (Recommended)", "")
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// One line of the questions, for the list and the mirror.
pub fn questions_text(questions: &Value) -> String {
    let qs = questions.as_array().cloned().unwrap_or_default();
    qs.iter()
        .map(|q| {
            let opts: Vec<String> = q.get("options").and_then(Value::as_array).into_iter().flatten()
                .filter_map(|o| o.get("label").and_then(Value::as_str).map(str::to_string)).collect();
            format!("{} ({})", q.get("question").and_then(Value::as_str).unwrap_or("?"), opts.join(" / "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Snooze presets: "1h", "tonight" (19:00, or 3 hours if that's passed) and "tomorrow"
/// (09:00 tomorrow). A number is taken as a Unix time.
pub fn snooze_until(preset: &str, now: chrono::DateTime<Local>) -> Option<f64> {
    let ts = now.timestamp() as f64;
    let at = |d: chrono::NaiveDate, h: u32| Local.from_local_datetime(&d.and_hms_opt(h, 0, 0)?).earliest().map(|t| t.timestamp() as f64);
    match preset {
        "1h" => Some(ts + 3600.0),
        "tonight" => {
            let t = at(now.date_naive(), 19)?;
            Some(if t > ts + 600.0 { t } else { ts + 3.0 * 3600.0 })
        }
        "tomorrow" => at(now.date_naive().succ_opt()?, 9),
        n => n.parse::<f64>().ok().filter(|t| *t > ts),
    }
}

/// Whether `t` falls on the same local day as `now`.
pub fn same_day(t: f64, now: chrono::DateTime<Local>) -> bool {
    Local.timestamp_opt(t as i64, 0).single().is_some_and(|d| d.year() == now.year() && d.ordinal() == now.ordinal())
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "question" => "question",
        "decision" => "decision",
        "manual" => "do by hand",
        "action" => "to do",
        _ => "heads-up",
    }
}

fn line(v: &View, done: bool) -> String {
    let what = if v.tldr.trim().is_empty() { v.title.trim() } else { v.tldr.trim() };
    let mut s = format!("- [{}] **{}** ({}", if done { "x" } else { " " }, what.replace('\n', " "), kind_label(&v.kind));
    if v.minutes > 0 {
        s += &format!(", about {} min", v.minutes);
    }
    s += ")";
    if !done && !v.recommend.is_empty() {
        s += &format!(". Suggested: {}", v.recommend);
        if !v.why.trim().is_empty() {
            s += &format!(", because {}", v.why.trim().trim_end_matches('.'));
        }
        s += ".";
    }
    if v.timed_out && !done {
        s += &format!(" Went ahead with: {}. Change it in the app.", v.default_answer);
    }
    if done && !v.note.trim().is_empty() {
        s += &format!(" ({})", v.note.trim());
    }
    s + &format!(" `{}`", v.id)
}

/// The read-only TODO-FOR-YOU.md. No timestamp, so it's only rewritten when the list changes.
pub fn mirror(ws_name: &str, open: &[View], today_ids: &[String], snoozed: &[View], done_today: &[View]) -> String {
    let minutes: u32 = open.iter().map(|v| v.minutes).sum();
    let mut s = format!(
        "# Needs you: {ws_name}\n\n_Written by Orchestrator from its 📌 Needs you list. Read-only: changes here are \
overwritten. Answer, finish or snooze things in the app._\n\n"
    );
    if open.is_empty() {
        s += "Nothing is waiting on you.\n";
    } else {
        s += &format!("{} waiting, about {minutes} min of your time.\n", open.len());
        s += "\n## Today\n\n";
        for v in open.iter().filter(|v| today_ids.contains(&v.id)) {
            s += &line(v, false);
            s += "\n";
        }
        let later: Vec<&View> = open.iter().filter(|v| !today_ids.contains(&v.id)).collect();
        if !later.is_empty() {
            s += &format!("\n## Later ({})\n\n", later.len());
            for v in later {
                s += &line(v, false);
                s += "\n";
            }
        }
    }
    if !snoozed.is_empty() {
        s += &format!("\n## Snoozed ({})\n\n", snoozed.len());
        for v in snoozed {
            s += &line(v, false);
            s += "\n";
        }
    }
    if !done_today.is_empty() {
        s += "\n## Done today\n\n";
        for v in done_today {
            s += &line(v, true);
            s += "\n";
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(id: &str, priority: &str, created: f64) -> View {
        View { id: id.into(), priority: priority.into(), created_at: created, status: "open".into(), ..Default::default() }
    }

    #[test]
    fn urgent_first_then_oldest() {
        let mut l = vec![v("a", "week", 1.0), v("b", "urgent", 5.0), v("c", "today", 2.0), v("d", "urgent", 3.0)];
        sort(&mut l);
        assert_eq!(l.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["d", "b", "c", "a"]);
    }

    #[test]
    fn today_takes_pins_then_agent_picks_then_the_top() {
        let mut l = vec![v("a", "urgent", 1.0), v("b", "today", 2.0), v("c", "week", 3.0), v("d", "week", 4.0)];
        assert_eq!(today(&l), ["a", "b", "c"]);
        l[3].agent_today = true;
        assert_eq!(today(&l), ["d", "a", "b"]);
        l[2].pin = 1;
        l[0].pin = -1;
        assert_eq!(today(&l), ["c", "d", "b"]);
    }

    #[test]
    fn the_default_is_the_recommended_option() {
        let q = json!([
            {"question": "Which board?", "options": [{"label": "Cats"}, {"label": "Mugs (Recommended)"}]},
            {"question": "Price?", "options": [{"label": "$16"}, {"label": "$18"}]},
        ]);
        assert_eq!(default_answer(&q), "Mugs; $16");
        assert!(questions_text(&q).starts_with("Which board? (Cats / Mugs (Recommended))"));
    }

    #[test]
    fn plans_need_do_and_expect_and_stay_short() {
        let ok = json!({"goal": "Make the itch.io account", "done_when": "You see your dashboard",
            "steps": [{"do": "Open itch.io/register", "expect": "A sign-up form", "url": "https://itch.io/register"},
                      {"do": "Type your username", "expect": "The name is accepted", "user_types_this": true}]});
        let p = parse_plan(&ok).unwrap();
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[1].user_types_this, "yes");
        assert_eq!(serde_json::to_value(&p.steps[0]).unwrap()["do"], "Open itch.io/register");
        let no_expect = json!({"goal": "g", "done_when": "d", "steps": [{"do": "Click Save"}]});
        assert!(parse_plan(&no_expect).unwrap_err().contains("step 1 needs both"));
        let long = json!({"goal": "g", "done_when": "d", "steps": vec![json!({"do": "x", "expect": "y"}); 13]});
        assert!(parse_plan(&long).unwrap_err().contains("split the task"));
        assert!(parse_plan(&json!({"goal": "g", "steps": [{"do": "x", "expect": "y"}]})).is_err(), "done_when is required");
    }

    #[test]
    fn snooze_presets() {
        let now = Local.with_ymd_and_hms(2026, 11, 28, 10, 0, 0).unwrap();
        let ts = now.timestamp() as f64;
        assert_eq!(snooze_until("1h", now), Some(ts + 3600.0));
        assert_eq!(snooze_until("tonight", now), Some(ts + 9.0 * 3600.0));
        let late = Local.with_ymd_and_hms(2026, 11, 28, 22, 0, 0).unwrap();
        assert_eq!(snooze_until("tonight", late), Some(late.timestamp() as f64 + 3.0 * 3600.0));
        assert_eq!(snooze_until("tomorrow", now), Some(ts + 23.0 * 3600.0));
        assert_eq!(snooze_until("nonsense", now), None);
    }

    #[test]
    fn the_mirror_lists_today_later_and_done() {
        let mut a = v("n1", "urgent", 1.0);
        a.tldr = "Make the itch.io account".into();
        a.kind = "action".into();
        a.minutes = 10;
        a.recommend = "do".into();
        a.why = "the game page needs it.".into();
        let b = v("queue:q4", "today", 2.0);
        let mut d = v("n0", "today", 0.0);
        d.title = "Old".into();
        d.status = "done".into();
        let m = mirror("biz", &[a.clone(), b], &["n1".into()], &[], &[d]);
        assert!(m.contains("2 waiting, about 10 min"), "{m}");
        assert!(m.contains("## Today\n\n- [ ] **Make the itch.io account** (to do, about 10 min). Suggested: do, because the game page needs it. `n1`"), "{m}");
        assert!(m.contains("## Later (1)") && m.contains("## Done today\n\n- [x] **Old**"), "{m}");
        assert_eq!(m, mirror("biz", &[a, v("queue:q4", "today", 2.0)], &["n1".into()], &[], &[{ let mut d = v("n0", "today", 0.0); d.title = "Old".into(); d }]), "stable");
    }
}
