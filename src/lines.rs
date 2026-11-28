//! Business lines as data (business workspaces). The business isn't a fixed set of lines:
//! the orchestrator proposes ventures, tests them, and scales or kills them. Each line has
//! a status, a playbook, a weekly budget (the per-line cap) and kill criteria. Lines the
//! user adds in the window start right away; lines the orchestrator proposes, and its
//! requests to pause or kill one, go through the approval queue.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STATUSES: [&str; 4] = ["testing", "active", "paused", "killed"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Line {
    /// Short id threads are tagged with and the ledger uses (e.g. "A", "X-printables").
    pub id: String,
    pub name: String,
    pub status: String,
    /// Playbook file in the workspace (e.g. pipelines/shop.md).
    pub playbook: String,
    /// Weekly budget in dollars of API-rate cost for threads tagged with this line (0 = none).
    pub weekly_budget_usd: f64,
    pub kill_criteria: String,
    /// "YYYY-MM-DD"; a line in testing shows the days left until `end`.
    pub start: String,
    pub end: String,
    pub created_by: String,
    /// Paused by the app because it went over its weekly budget.
    pub auto_paused: bool,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_date(d: &str) -> bool {
    d.is_empty() || chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok()
}

/// A line from JSON (what propose_line queues and the window sends). Checks every field.
pub fn parse(v: &Value) -> Result<Line, String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let l = Line {
        id: s("id"),
        name: s("name"),
        status: if s("status").is_empty() { "testing".into() } else { s("status").to_lowercase() },
        playbook: s("playbook"),
        weekly_budget_usd: v.get("weekly_budget_usd").and_then(Value::as_f64).unwrap_or(0.0),
        kill_criteria: s("kill_criteria"),
        start: s("start"),
        end: s("end"),
        ..Default::default()
    };
    if !valid_id(&l.id) {
        return Err("id must be 1-32 letters, digits, - or _".into());
    }
    if l.name.is_empty() {
        return Err("name is required".into());
    }
    if !STATUSES.contains(&l.status.as_str()) {
        return Err(format!("status must be one of {}", STATUSES.join(", ")));
    }
    if !(l.weekly_budget_usd >= 0.0 && l.weekly_budget_usd.is_finite()) {
        return Err("weekly_budget_usd must be 0 or more".into());
    }
    if !valid_date(&l.start) || !valid_date(&l.end) {
        return Err("start and end are dates: YYYY-MM-DD".into());
    }
    Ok(l)
}

/// Days until a line's end date (negative once it's past), if it has one.
pub fn days_left(l: &Line, today: chrono::NaiveDate) -> Option<i64> {
    let end = chrono::NaiveDate::parse_from_str(&l.end, "%Y-%m-%d").ok()?;
    Some((end - today).num_days())
}

/// May a new thread tagged with this line start? Paid orders always may.
pub fn spawn_allowed(l: Option<&Line>, tag: &str, spent_week: f64, priority: &str) -> Result<(), String> {
    if tag.is_empty() {
        return Ok(());
    }
    let l = l.ok_or_else(|| format!("no business line '{tag}': see list_lines (lines are added by the user or through propose_line)"))?;
    if priority == crate::budget::PAID_ORDER {
        return Ok(());
    }
    if l.status == "paused" || l.status == "killed" {
        return Err(format!("line {} is {}: only paid-order threads start for it", l.id, l.status));
    }
    if l.weekly_budget_usd > 0.0 && spent_week >= l.weekly_budget_usd {
        return Err(format!("line {} has spent its weekly budget (${spent_week:.2} of ${:.2}): only paid-order threads start for it", l.id, l.weekly_budget_usd));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line() -> Line {
        parse(&json!({"id": "X-printables", "name": "Printables", "weekly_budget_usd": 5.0, "end": "2026-12-12"})).unwrap()
    }

    #[test]
    fn parses_and_checks_lines() {
        let l = line();
        assert_eq!(l.status, "testing");
        assert!(parse(&json!({"id": "bad id", "name": "x"})).is_err());
        assert!(parse(&json!({"id": "A", "name": ""})).is_err());
        assert!(parse(&json!({"id": "A", "name": "x", "status": "zombie"})).is_err());
        assert!(parse(&json!({"id": "A", "name": "x", "end": "next week"})).is_err());
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 28).unwrap();
        assert_eq!(days_left(&l, today), Some(14));
    }

    #[test]
    fn per_line_caps_and_status_gate_new_threads() {
        let mut l = line();
        assert!(spawn_allowed(Some(&l), "X-printables", 4.99, "").is_ok());
        assert!(spawn_allowed(Some(&l), "X-printables", 5.0, "").is_err(), "over its weekly budget");
        assert!(spawn_allowed(Some(&l), "X-printables", 5.0, crate::budget::PAID_ORDER).is_ok(), "paid orders always run");
        l.status = "paused".into();
        assert!(spawn_allowed(Some(&l), "X-printables", 0.0, "").is_err());
        assert!(spawn_allowed(None, "nope", 0.0, "").is_err(), "the tag must be a registered line");
        assert!(spawn_allowed(None, "", 0.0, "").is_ok(), "untagged threads aren't affected");
    }
}
