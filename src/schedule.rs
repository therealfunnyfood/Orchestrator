//! Scheduled routines for business workspaces: "08:00 every day → send 'morning run' to
//! the orchestrator". The orchestrator can only propose a schedule; it runs once the user
//! confirms it in the window. A run is skipped (and logged) when the workspace is paused,
//! its budget is spent, or the previous run is still going.

use chrono::{Datelike, Local, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Schedule {
    pub id: String,
    /// "HH:MM", local time.
    pub time: String,
    /// Days it runs, 0 = Monday ... 6 = Sunday. Empty = every day.
    pub days: Vec<u8>,
    /// What's sent (to the orchestrator, or to `thread`).
    pub message: String,
    /// A standing thread to wake directly ("" = the orchestrator).
    pub thread: String,
    pub enabled: bool,
    /// The user confirmed it (schedules the orchestrator proposes start unconfirmed).
    pub confirmed: bool,
    pub created_by: String,
    pub reason: String,
    /// "YYYY-MM-DD HH:MM" of the last time it was due (ran or skipped), so it fires once.
    pub last_key: String,
    pub last_result: String,
    pub last_at: f64,
}

pub fn parse_time(t: &str) -> Option<(u32, u32)> {
    let (h, m) = t.trim().split_once(':')?;
    let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

pub const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// Days from names like "Mon,Wed,Fri", "weekdays", "weekends" or "daily".
pub fn parse_days(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim().to_lowercase();
    match s.as_str() {
        "" | "daily" | "every day" | "everyday" => return Ok(vec![]),
        "weekdays" => return Ok(vec![0, 1, 2, 3, 4]),
        "weekends" => return Ok(vec![5, 6]),
        _ => {}
    }
    let mut out = vec![];
    for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
        let i = DAY_NAMES.iter().position(|d| part.starts_with(&d.to_lowercase())).ok_or(format!("unknown day '{part}' (use Mon..Sun, weekdays, weekends or daily)"))?;
        if !out.contains(&(i as u8)) {
            out.push(i as u8);
        }
    }
    out.sort();
    Ok(out)
}

pub fn describe_days(days: &[u8]) -> String {
    match days {
        [] => "every day".into(),
        [0, 1, 2, 3, 4] => "weekdays".into(),
        [5, 6] => "weekends".into(),
        d => d.iter().map(|i| DAY_NAMES[*i as usize % 7]).collect::<Vec<_>>().join(", "),
    }
}

/// What the scheduler should do with a schedule at `now`.
#[derive(Debug, PartialEq)]
pub enum Due {
    NotDue,
    Run(String),
    Skip(String, String),
}

/// Whether a schedule is due at `now` (within `window_s` after its time), and if so,
/// whether to run it or skip it. Returns the period key to remember either way.
pub fn check(s: &Schedule, now: f64, window_s: f64, paused: bool, budget_spent: Option<&str>, previous_running: bool) -> Due {
    if !s.enabled || !s.confirmed {
        return Due::NotDue;
    }
    let Some((h, m)) = parse_time(&s.time) else { return Due::NotDue };
    let Some(dt) = Local.timestamp_opt(now as i64, 0).single() else { return Due::NotDue };
    let Some(at) = dt.date_naive().and_hms_opt(h, m, 0).and_then(|d| Local.from_local_datetime(&d).earliest()) else { return Due::NotDue };
    let since = now - at.timestamp() as f64;
    if !(0.0..window_s).contains(&since) {
        return Due::NotDue;
    }
    if !s.days.is_empty() && !s.days.contains(&(dt.weekday().num_days_from_monday() as u8)) {
        return Due::NotDue;
    }
    let key = format!("{} {:02}:{:02}", dt.format("%Y-%m-%d"), at.hour(), at.minute());
    if s.last_key == key {
        return Due::NotDue;
    }
    if paused {
        return Due::Skip(key, "the workspace is paused".into());
    }
    if let Some(why) = budget_spent {
        return Due::Skip(key, why.to_string());
    }
    if previous_running {
        return Due::Skip(key, "the previous run is still going".into());
    }
    Due::Run(key)
}

/// The next time (Unix seconds) a schedule is due after `now`, if it's on.
pub fn next_run(s: &Schedule, now: f64) -> Option<f64> {
    if !s.enabled || !s.confirmed {
        return None;
    }
    let (h, m) = parse_time(&s.time)?;
    let today = Local.timestamp_opt(now as i64, 0).single()?.date_naive();
    (0..8).find_map(|d| {
        let day = today + chrono::Duration::days(d);
        if !s.days.is_empty() && !s.days.contains(&(day.weekday().num_days_from_monday() as u8)) {
            return None;
        }
        let t = Local.from_local_datetime(&day.and_hms_opt(h, m, 0)?).earliest()?.timestamp() as f64;
        (t > now).then_some(t)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> f64 {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap().timestamp() as f64
    }

    fn morning() -> Schedule {
        Schedule { id: "s1".into(), time: "08:00".into(), message: "morning run".into(), enabled: true, confirmed: true, ..Default::default() }
    }

    #[test]
    fn runs_once_in_its_minute() {
        let s = morning();
        assert_eq!(check(&s, at(2026, 11, 25, 7, 59), 90.0, false, None, false), Due::NotDue);
        let Due::Run(key) = check(&s, at(2026, 11, 25, 8, 0) + 20.0, 90.0, false, None, false) else { panic!("should run") };
        let done = Schedule { last_key: key, ..s.clone() };
        assert_eq!(check(&done, at(2026, 11, 25, 8, 0) + 50.0, 90.0, false, None, false), Due::NotDue, "fires once");
        assert_eq!(check(&s, at(2026, 11, 25, 8, 5), 90.0, false, None, false), Due::NotDue, "too late: missed runs don't pile up");
    }

    #[test]
    fn skipped_when_the_budget_is_spent() {
        let s = morning();
        match check(&s, at(2026, 11, 25, 8, 0), 90.0, false, Some("today's budget is spent ($10.00 of $10.00)"), false) {
            Due::Skip(_, why) => assert!(why.contains("budget")),
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    #[test]
    fn skipped_when_paused_or_still_running() {
        let s = morning();
        assert!(matches!(check(&s, at(2026, 11, 25, 8, 0), 90.0, true, None, false), Due::Skip(_, w) if w.contains("paused")));
        assert!(matches!(check(&s, at(2026, 11, 25, 8, 0), 90.0, false, None, true), Due::Skip(_, w) if w.contains("still going")));
    }

    #[test]
    fn unconfirmed_or_other_days_dont_run() {
        let mut s = morning();
        s.confirmed = false;
        assert_eq!(check(&s, at(2026, 11, 25, 8, 0), 90.0, false, None, false), Due::NotDue);
        let mut sunday = morning();
        sunday.days = parse_days("Sun").unwrap();
        // 2026-11-25 is a Wednesday, 11-29 a Sunday.
        assert_eq!(check(&sunday, at(2026, 11, 25, 8, 0), 90.0, false, None, false), Due::NotDue);
        assert!(matches!(check(&sunday, at(2026, 11, 29, 8, 0), 90.0, false, None, false), Due::Run(_)));
    }

    #[test]
    fn next_run_finds_the_next_matching_day() {
        let mut s = morning();
        assert_eq!(next_run(&s, at(2026, 11, 25, 7, 0)), Some(at(2026, 11, 25, 8, 0)));
        assert_eq!(next_run(&s, at(2026, 11, 25, 9, 0)), Some(at(2026, 11, 26, 8, 0)));
        s.days = parse_days("Mon").unwrap();
        assert_eq!(next_run(&s, at(2026, 11, 25, 9, 0)), Some(at(2026, 11, 30, 8, 0)));
        s.enabled = false;
        assert_eq!(next_run(&s, at(2026, 11, 25, 9, 0)), None);
    }

    #[test]
    fn parses_days_and_times() {
        assert_eq!(parse_days("weekdays").unwrap(), vec![0, 1, 2, 3, 4]);
        assert_eq!(parse_days("Mon, wed,Fri").unwrap(), vec![0, 2, 4]);
        assert!(parse_days("someday").is_err());
        assert_eq!(parse_time("18:30"), Some((18, 30)));
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(describe_days(&[5, 6]), "weekends");
    }
}
