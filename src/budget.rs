//! Budget caps for business workspaces: daily and weekly dollar caps on the work's cost
//! (API-rate cost of every turn, on the plan or the API, as the app already records it).
//! At 80% of a cap the user is notified; at 100% no new thread starts unless its task is
//! a paid order.

use chrono::{Datelike, Local, TimeZone};

/// Threads with this priority always run: a paid order must be delivered.
pub const PAID_ORDER: &str = "paid-order";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Caps {
    /// 0 = no cap.
    pub day: f64,
    pub week: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    pub day: f64,
    pub week: f64,
}

/// Start of today and of this week (Monday), local time, as Unix seconds.
pub fn period_starts(now: f64) -> (f64, f64) {
    let dt = Local.timestamp_opt(now as i64, 0).single().unwrap_or_else(Local::now);
    let day = dt.date_naive().and_hms_opt(0, 0, 0).unwrap();
    let day_start = Local.from_local_datetime(&day).earliest().map_or(now, |d| d.timestamp() as f64);
    let monday = day - chrono::Duration::days(dt.weekday().num_days_from_monday() as i64);
    let week_start = Local.from_local_datetime(&monday).earliest().map_or(now, |d| d.timestamp() as f64);
    (day_start, week_start)
}

/// Sum costs (time, cost) into today's and this week's spend.
pub fn spend(rows: impl Iterator<Item = (f64, f64)>, now: f64) -> Spend {
    let (day_start, week_start) = period_starts(now);
    let mut s = Spend::default();
    for (t, cost) in rows {
        if t >= week_start {
            s.week += cost;
            if t >= day_start {
                s.day += cost;
            }
        }
    }
    s
}

/// Why new work is blocked, if a cap is reached.
pub fn exceeded(caps: Caps, s: Spend) -> Option<String> {
    if caps.day > 0.0 && s.day >= caps.day {
        return Some(format!("today's budget is spent (${:.2} of ${:.2})", s.day, caps.day));
    }
    if caps.week > 0.0 && s.week >= caps.week {
        return Some(format!("this week's budget is spent (${:.2} of ${:.2})", s.week, caps.week));
    }
    None
}

/// Alerts due now: (key that identifies the period and threshold, message). The caller
/// remembers keys it already sent, so each alert goes out once per period.
pub fn alerts(caps: Caps, s: Spend, now: f64) -> Vec<(String, String)> {
    let dt = Local.timestamp_opt(now as i64, 0).single().unwrap_or_else(Local::now);
    let day_id = dt.format("%Y-%m-%d").to_string();
    let week_id = dt.format("%G-W%V").to_string();
    let mut out = vec![];
    for (label, cap, spent, id) in [("today's", caps.day, s.day, day_id), ("this week's", caps.week, s.week, week_id)] {
        if cap <= 0.0 {
            continue;
        }
        let pct = spent / cap;
        for (level, text) in [(1.0, "reached"), (0.8, "at 80% of")] {
            if pct >= level {
                out.push((format!("{id}:{}", (level * 100.0) as u32), format!("Budget {text} {label} cap: ${spent:.2} of ${cap:.2}")));
                break;
            }
        }
    }
    out
}

/// Whether a new thread may start.
pub fn spawn_allowed(caps: Caps, s: Spend, priority: &str) -> Result<(), String> {
    match exceeded(caps, s) {
        Some(why) if priority != PAID_ORDER => Err(format!(
            "{why}. New threads only start for paid orders (priority: \"{PAID_ORDER}\") until the budget resets or the user raises it."
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32) -> f64 {
        Local.with_ymd_and_hms(y, m, d, h, 0, 0).earliest().unwrap().timestamp() as f64
    }

    #[test]
    fn spend_counts_today_and_this_week() {
        // Wednesday 2026-11-25; the week started Monday 11-23.
        let now = at(2026, 11, 25, 15);
        let rows = vec![(at(2026, 11, 25, 9), 2.0), (at(2026, 11, 24, 9), 3.0), (at(2026, 11, 22, 9), 50.0)];
        let s = spend(rows.into_iter(), now);
        assert_eq!(s.day, 2.0);
        assert_eq!(s.week, 5.0);
    }

    #[test]
    fn caps_block_new_threads_but_not_paid_orders() {
        let caps = Caps { day: 10.0, week: 0.0 };
        assert!(spawn_allowed(caps, Spend { day: 9.99, week: 9.99 }, "").is_ok());
        let over = Spend { day: 10.0, week: 10.0 };
        assert!(spawn_allowed(caps, over, "").is_err());
        assert!(spawn_allowed(caps, over, "normal").is_err());
        assert!(spawn_allowed(caps, over, PAID_ORDER).is_ok(), "paid orders always run");
        assert!(spawn_allowed(Caps::default(), Spend { day: 1e6, week: 1e6 }, "").is_ok(), "no caps, no limit");
    }

    #[test]
    fn alerts_at_80_and_100_percent() {
        let now = at(2026, 11, 25, 15);
        let caps = Caps { day: 10.0, week: 50.0 };
        assert!(alerts(caps, Spend { day: 7.9, week: 7.9 }, now).is_empty());
        let a = alerts(caps, Spend { day: 8.0, week: 8.0 }, now);
        assert_eq!(a.len(), 1);
        assert!(a[0].0.ends_with(":80"));
        let a = alerts(caps, Spend { day: 10.5, week: 45.0 }, now);
        assert_eq!(a.len(), 2);
        assert!(a.iter().any(|(k, m)| k.ends_with(":100") && m.contains("reached today's")));
        assert!(a.iter().any(|(k, _)| k.contains("-W") && k.ends_with(":80")));
    }
}
