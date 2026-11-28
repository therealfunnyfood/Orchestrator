//! Digests and busy mode: which notifications go out right away and which wait for the
//! next digest (up to three a day, at per-workspace times). Held notifications are kept
//! until a digest sends them; they're never dropped. The wiring is in app/digest_app.rs.

use chrono::{Local, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};

pub const DEFAULT_TIMES: [&str; 3] = ["09:00", "13:00", "19:00"];
pub const MAX_TIMES: usize = 3;

/// A notification waiting for the next digest.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Held {
    pub ts: f64,
    pub event: String,
    pub title: String,
    pub body: String,
}

/// How urgent a notification is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Waits for the next digest.
    Normal,
    /// Goes out right away, except in busy mode.
    Urgent,
    /// Always goes out right away: deadline risk (and the test notification).
    Critical,
}

/// The default level of a notification event. Budget alerts are Normal here; the caller
/// raises a hard stop (100 % of a cap, a line paused by its budget) to Urgent.
pub fn level_of(event: &str) -> Level {
    match event {
        "deadline" | "test" => Level::Critical,
        "urgent" | "needs_you" | "usage" | "pause" => Level::Urgent,
        _ => Level::Normal,
    }
}

/// Whether a notification is held (true) or sent now.
pub fn holds(level: Level, busy: bool, digests_on: bool) -> bool {
    match level {
        Level::Critical => false,
        Level::Urgent => busy,
        Level::Normal => busy || digests_on,
    }
}

/// Check and tidy digest times: 0 to 3 "HH:MM" (24 h), sorted, no repeats.
pub fn parse_times(times: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for t in times {
        let t = t.trim();
        let parsed = NaiveTime::parse_from_str(t, "%H:%M").map_err(|_| format!("'{t}' isn't a time like 09:00"))?;
        let s = parsed.format("%H:%M").to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    if out.len() > MAX_TIMES {
        return Err(format!("at most {MAX_TIMES} digests a day"));
    }
    out.sort();
    Ok(out)
}

/// The digest times in effect: the user's (possibly none) or the defaults.
pub fn effective_times(set: bool, times: &[String]) -> Vec<String> {
    if set { times.to_vec() } else { DEFAULT_TIMES.iter().map(|s| s.to_string()).collect() }
}

/// Unix times of each digest time on the local day `days` from `now`'s day.
fn slots_on(times: &[String], now: f64, days: i64) -> Vec<f64> {
    let Some(base) = Local.timestamp_opt(now as i64, 0).single() else { return vec![] };
    let date = base.date_naive() + chrono::Duration::days(days);
    times
        .iter()
        .filter_map(|t| NaiveTime::parse_from_str(t, "%H:%M").ok())
        .filter_map(|t| Local.from_local_datetime(&date.and_time(t)).earliest())
        .map(|d| d.timestamp() as f64)
        .collect()
}

/// The digest slot that's due now: the latest slot at or before `now` (today or
/// yesterday, so a slot missed while the app was closed still goes out once) that's
/// later than the last digest sent.
pub fn due_slot(times: &[String], last_sent: f64, now: f64) -> Option<f64> {
    let mut slots = slots_on(times, now, -1);
    slots.extend(slots_on(times, now, 0));
    slots.into_iter().filter(|s| *s <= now).fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.max(s)))).filter(|s| *s > last_sent)
}

/// The next digest time after `now`, if digests are on.
pub fn next_slot(times: &[String], now: f64) -> Option<f64> {
    let mut slots = slots_on(times, now, 0);
    slots.extend(slots_on(times, now, 1));
    slots.into_iter().filter(|s| *s > now).fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.min(s))))
}

/// One notification for a digest: a title with the count, and the titles (repeats
/// counted) as the body.
pub fn digest_text(held: &[Held], busy_ended: bool) -> (String, String) {
    let mut seen: Vec<(String, usize)> = vec![];
    for h in held {
        match seen.iter_mut().find(|(t, _)| *t == h.title) {
            Some((_, n)) => *n += 1,
            None => seen.push((h.title.clone(), 1)),
        }
    }
    let n = held.len();
    let title = if busy_ended {
        format!("While you were busy: {n} update{}", if n == 1 { "" } else { "s" })
    } else {
        format!("Digest: {n} update{}", if n == 1 { "" } else { "s" })
    };
    let body = seen
        .iter()
        .map(|(t, c)| if *c > 1 { format!("{t} (×{c})") } else { t.clone() })
        .collect::<Vec<_>>()
        .join("; ");
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32, days: i64) -> f64 {
        let d = Local::now().date_naive() + chrono::Duration::days(days);
        Local.from_local_datetime(&d.and_hms_opt(h, m, 0).unwrap()).earliest().unwrap().timestamp() as f64
    }

    fn defaults() -> Vec<String> {
        effective_times(false, &[])
    }

    #[test]
    fn levels_and_holding() {
        // Digests on, not busy: only normal ones wait.
        assert!(holds(level_of("schedule"), false, true));
        assert!(holds(level_of("budget"), false, true));
        assert!(!holds(level_of("urgent"), false, true));
        assert!(!holds(level_of("needs_you"), false, true));
        assert!(!holds(level_of("deadline"), false, true));
        // Digests off: everything goes now.
        assert!(!holds(level_of("schedule"), false, false));
        // Busy: everything waits except deadline risk (and the test).
        assert!(holds(level_of("schedule"), true, false));
        assert!(holds(level_of("urgent"), true, true));
        assert!(holds(Level::Urgent, true, false));
        assert!(!holds(level_of("deadline"), true, true));
        assert!(!holds(level_of("test"), true, true));
    }

    #[test]
    fn digest_times_are_checked() {
        assert_eq!(parse_times(&["19:00".into(), "9:00".into(), "09:00".into()]).unwrap(), vec!["09:00", "19:00"]);
        assert!(parse_times(&["25:00".into()]).is_err());
        assert!(parse_times(&["noon".into()]).is_err());
        assert!(parse_times(&["01:00".into(), "02:00".into(), "03:00".into(), "04:00".into()]).is_err());
        assert!(parse_times(&[]).unwrap().is_empty());
        assert_eq!(defaults(), vec!["09:00", "13:00", "19:00"]);
        assert!(effective_times(true, &[]).is_empty());
    }

    #[test]
    fn digest_slots_come_due_once() {
        let t = defaults();
        // 10:00, last digest at 09:00:30 → nothing due; next at 13:00.
        assert_eq!(due_slot(&t, at(9, 0, 0) + 30.0, at(10, 0, 0)), None);
        assert_eq!(next_slot(&t, at(10, 0, 0)), Some(at(13, 0, 0)));
        // 13:00:10, last at 09:00 → the 13:00 slot.
        assert_eq!(due_slot(&t, at(9, 0, 0), at(13, 0, 0) + 10.0), Some(at(13, 0, 0)));
        // Sent it: not due again.
        assert_eq!(due_slot(&t, at(13, 0, 0) + 10.0, at(13, 5, 0)), None);
        // 08:00, last digest yesterday at 13:00 → yesterday's 19:00 was missed: due once.
        assert_eq!(due_slot(&t, at(13, 0, 0 - 1), at(8, 0, 0)), Some(at(19, 0, -1)));
        // After 19:00 the next one is tomorrow 09:00.
        assert_eq!(next_slot(&t, at(20, 0, 0)), Some(at(9, 0, 1)));
        // Digests off.
        assert_eq!(due_slot(&[], 0.0, at(12, 0, 0)), None);
        assert_eq!(next_slot(&[], at(12, 0, 0)), None);
    }

    #[test]
    fn digest_text_counts_repeats() {
        let h = |t: &str| Held { ts: 1.0, event: "schedule".into(), title: t.into(), body: String::new() };
        let (title, body) = digest_text(&[h("Run A finished"), h("Run B finished"), h("Run A finished")], false);
        assert_eq!(title, "Digest: 3 updates");
        assert_eq!(body, "Run A finished (×2); Run B finished");
        let (title, _) = digest_text(&[h("x")], true);
        assert_eq!(title, "While you were busy: 1 update");
    }
}
