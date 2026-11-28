//! Usage-aware admission control for business workspaces that run on the user's Claude
//! plan (see the workspace's pipelines/capacity.md). Never take paid work the 5-hour window
//! or the weekly limit can't finish.
//!
//! Units: everything is measured in percentage points of the **weekly** plan limit
//! ("weekly points"): an order estimated at 5 uses about 5% of a week's limit. The app
//! learns how many weekly points a dollar of API-rate cost uses from its own usage samples,
//! which turns the per-turn costs it already records into usage estimates.

use serde::{Deserialize, Serialize};

pub const WEEK_S: f64 = 7.0 * 86400.0;

/// Queue item types that commit the business to paid work: they need a capacity check.
pub const COMMITTING: [&str; 3] = ["send_offer", "accept_order", "publish_gig"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Reservation {
    pub order_id: String,
    pub workspace: String,
    /// Weekly points estimated when reserved, and what's still left to do.
    pub est_units: f64,
    pub remaining_units: f64,
    pub deadline: f64,
    pub created_at: f64,
    /// Day key of the last at-risk alert, so the watchdog alerts once a day per order.
    pub alerted: String,
}

/// One reading of the plan's usage.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sample {
    pub ts: f64,
    /// Utilization 0..1 of the 5-hour window and of the week.
    pub five: f64,
    pub week: f64,
    pub week_resets_at: f64,
}

/// Gate thresholds (utilization 0..1).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Gates {
    /// At or above either, only paid-order threads start.
    pub five_hour: f64,
    pub week: f64,
    /// At or above this, running non-priority threads checkpoint and stop.
    pub week_stop: f64,
    /// Share of the projected weekly usage left that committed work may take.
    pub share: f64,
}

impl Default for Gates {
    fn default() -> Self {
        Gates { five_hour: 0.70, week: 0.60, week_stop: 0.80, share: 0.50 }
    }
}

/// The current reading, if it's fresh enough to act on (`max_age_s`).
pub fn fresh(s: Option<Sample>, now: f64, max_age_s: f64) -> Option<Sample> {
    s.filter(|s| now - s.ts <= max_age_s)
}

/// Weekly points left before `t`: what's left this week, plus, for every weekly reset
/// before `t`, the part of the new week's limit that the time between that reset and `t`
/// can actually use (a reset half an hour before a deadline adds almost nothing). None
/// when utilization is unknown.
pub fn available_before(s: Option<Sample>, now: f64, t: f64) -> Option<f64> {
    let s = s?;
    if t <= now {
        return Some(0.0);
    }
    let mut avail = (1.0 - s.week).max(0.0) * 100.0;
    if s.week_resets_at > now {
        let mut reset = s.week_resets_at;
        while reset < t {
            avail += 100.0 * ((t - reset) / WEEK_S).min(1.0);
            reset += WEEK_S;
        }
    }
    Some(avail)
}

/// Weekly points burned in the last hour, from two samples at least 15 minutes apart
/// (None if there aren't any, or a reset happened in between).
pub fn burn_last_hour(samples: &[Sample], now: f64) -> Option<f64> {
    let recent: Vec<&Sample> = samples.iter().filter(|s| now - s.ts <= 3600.0).collect();
    let (first, last) = (recent.first()?, recent.last()?);
    let span = last.ts - first.ts;
    if span < 900.0 || last.week < first.week {
        return None;
    }
    Some((last.week - first.week) * 100.0 * 3600.0 / span)
}

/// Weekly points per dollar of API-rate cost, learned from samples and the costs recorded
/// between them (`spend_between(t0, t1)`). Median over usable intervals; None if too few.
pub fn points_per_dollar(samples: &[Sample], spend_between: impl Fn(f64, f64) -> f64) -> Option<f64> {
    let mut ratios = vec![];
    for w in samples.windows(2) {
        let (a, b) = (w[0], w[1]);
        let dp = (b.week - a.week) * 100.0;
        if dp <= 0.0 || b.week_resets_at != a.week_resets_at {
            continue; // no change, or a reset in between
        }
        let usd = spend_between(a.ts, b.ts);
        if usd >= 0.25 {
            ratios.push(dp / usd);
        }
    }
    if ratios.len() < 3 {
        return None;
    }
    ratios.sort_by(f64::total_cmp);
    Some(ratios[ratios.len() / 2])
}

/// The app's capacity check for an offer: does the reserved load plus this order fit in
/// the allowed share of the weekly usage left before its deadline?
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Check {
    pub ok: bool,
    pub note: String,
    pub est_units: f64,
    pub deadline: f64,
    pub reserved: f64,
    pub available: f64,
}

pub fn check_offer(sample: Option<Sample>, reservations: &[Reservation], est_units: f64, deadline: f64, now: f64, gates: Gates) -> Check {
    // (+ 0.0: an empty f64 sum is -0.0, which prints as "-0.0".)
    let reserved: f64 = reservations.iter().map(|r| r.remaining_units).sum::<f64>() + 0.0;
    let mut c = Check { est_units, deadline, reserved, ..Default::default() };
    if est_units <= 0.0 {
        c.note = "no usage estimate given".into();
        return c;
    }
    if deadline <= now {
        c.note = "the deadline is in the past".into();
        return c;
    }
    let Some(avail) = available_before(sample, now, deadline) else {
        c.note = "plan usage is unknown right now, so capacity can't be checked".into();
        return c;
    };
    c.available = avail;
    let allowed = avail * gates.share;
    c.ok = reserved + est_units <= allowed;
    c.note = format!(
        "reserved {reserved:.1} + this order {est_units:.1} = {:.1} weekly points; {}% of the {avail:.1} left before the deadline is {allowed:.1}: {}",
        reserved + est_units,
        (gates.share * 100.0).round(),
        if c.ok { "fits" } else { "DOESN'T FIT" }
    );
    c
}

/// Whether a new thread may start under the usage gates. Paid orders always may.
pub fn spawn_allowed(sample: Option<Sample>, gates: Gates, priority: &str) -> Result<(), String> {
    if priority == crate::budget::PAID_ORDER {
        return Ok(());
    }
    let Some(s) = sample else { return Ok(()) };
    if s.five >= gates.five_hour || s.week >= gates.week {
        return Err(format!(
            "plan usage is high (5-hour window {:.0}%, week {:.0}%; gates at {:.0}% / {:.0}%): only paid-order threads start now (spawn_thread priority \"{}\"). Paid orders get the remaining usage.",
            s.five * 100.0, s.week * 100.0, gates.five_hour * 100.0, gates.week * 100.0, crate::budget::PAID_ORDER
        ));
    }
    Ok(())
}

/// Reservations at risk: their remaining work (plus everything due earlier) doesn't fit
/// in the usage projected to be left before 75% of their deadline.
pub fn at_risk(sample: Option<Sample>, burn_per_hour: f64, reservations: &[Reservation], now: f64) -> Vec<(String, String)> {
    let mut rs: Vec<&Reservation> = reservations.iter().filter(|r| r.remaining_units > 0.0).collect();
    rs.sort_by(|a, b| a.deadline.total_cmp(&b.deadline));
    let mut out = vec![];
    let mut cumulative = 0.0;
    for r in rs {
        cumulative += r.remaining_units;
        let t75 = r.created_at + 0.75 * (r.deadline - r.created_at);
        let horizon = t75.max(now);
        let avail = available_before(sample, now, horizon);
        let why = match avail {
            None => Some("plan usage is unknown".to_string()),
            Some(a) => {
                // Usage other work keeps burning meanwhile isn't available to orders.
                let other = burn_per_hour.max(0.0) * ((horizon - now) / 3600.0) * 0.5;
                let left = a - other;
                (cumulative > left).then(|| format!(
                    "needs {cumulative:.1} weekly points (this order and the ones due before it) by 75% of its deadline, but only about {:.1} are projected to be left",
                    left.max(0.0)
                ))
            }
        };
        if let Some(w) = why {
            out.push((r.order_id.clone(), w));
        }
    }
    out
}

/// Which backend a business thread runs on: the plan, or (paid orders only, with the user's
/// setting and an API key, under the order's dollar cap) the plan with API fallback.
pub fn thread_backend(priority: &str, fallback_on: bool, api_key_set: bool, api_spent: f64, cap_usd: f64) -> &'static str {
    if priority == crate::budget::PAID_ORDER && fallback_on && api_key_set && (cap_usd <= 0.0 || api_spent < cap_usd) {
        "auto"
    } else {
        "plan"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0;
    const H: f64 = 3600.0;

    fn sample(week: f64, resets_in: f64) -> Option<Sample> {
        Some(Sample { ts: NOW, five: 0.2, week, week_resets_at: NOW + resets_in })
    }

    fn res(id: &str, units: f64, deadline_in: f64) -> Reservation {
        Reservation { order_id: id.into(), est_units: units, remaining_units: units, deadline: NOW + deadline_in, created_at: NOW, ..Default::default() }
    }

    #[test]
    fn an_over_capacity_offer_is_flagged() {
        // 30% of the week left, resets in 5 days; deadline in 2 days: 30 points left, 50% = 15.
        let s = sample(0.70, 5.0 * 24.0 * H);
        let reserved = vec![res("o1", 10.0, 24.0 * H)];
        let ok = check_offer(s, &reserved, 4.0, NOW + 48.0 * H, NOW, Gates::default());
        assert!(ok.ok, "{}", ok.note);
        let over = check_offer(s, &reserved, 6.0, NOW + 48.0 * H, NOW, Gates::default());
        assert!(!over.ok, "10 reserved + 6 > 15 must be flagged");
        assert!(over.note.contains("DOESN'T FIT"));
        // A weekly reset before the deadline adds the part of the new week that's usable
        // before it: here one day of it (about 14 points), so 30 + 14 = 44, half is 22.
        let later = check_offer(s, &reserved, 6.0, NOW + 6.0 * 24.0 * H, NOW, Gates::default());
        assert!(later.ok, "{}", later.note);
        // A reset just before the deadline adds almost nothing.
        let just_after_reset = check_offer(s, &reserved, 6.0, NOW + 5.0 * 24.0 * H + 0.5 * H, NOW, Gates::default());
        assert!(!just_after_reset.ok, "{}", just_after_reset.note);
    }

    #[test]
    fn unknown_usage_never_passes_a_capacity_check() {
        let c = check_offer(None, &[], 1.0, NOW + 48.0 * H, NOW, Gates::default());
        assert!(!c.ok);
        assert!(c.note.contains("unknown"));
    }

    #[test]
    fn non_priority_spawns_are_refused_above_the_gate() {
        let g = Gates::default();
        let mut s = Sample { ts: NOW, five: 0.50, week: 0.50, week_resets_at: NOW + H };
        assert!(spawn_allowed(Some(s), g, "").is_ok());
        s.five = 0.70;
        assert!(spawn_allowed(Some(s), g, "").is_err(), "5-hour gate");
        s.five = 0.10;
        s.week = 0.61;
        assert!(spawn_allowed(Some(s), g, "normal").is_err(), "weekly gate");
    }

    #[test]
    fn paid_order_threads_are_never_refused() {
        let s = Sample { ts: NOW, five: 0.99, week: 0.99, week_resets_at: NOW + H };
        assert!(spawn_allowed(Some(s), Gates::default(), crate::budget::PAID_ORDER).is_ok());
        assert!(crate::budget::spawn_allowed(
            crate::budget::Caps { day: 1.0, week: 1.0 }, crate::budget::Spend { day: 9.0, week: 9.0 }, crate::budget::PAID_ORDER
        ).is_ok());
    }

    #[test]
    fn the_watchdog_fires_for_an_at_risk_reservation() {
        // 20 points left this week, no reset before the deadlines.
        let s = sample(0.80, 6.0 * 24.0 * H);
        let fine = vec![res("small", 5.0, 48.0 * H)];
        assert!(at_risk(s, 0.0, &fine, NOW).is_empty());
        let risky = vec![res("small", 5.0, 48.0 * H), res("big", 18.0, 72.0 * H)];
        let alerts = at_risk(s, 0.0, &risky, NOW);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].0, "big", "the later order no longer fits after the earlier one");
        assert!(!at_risk(None, 0.0, &fine, NOW).is_empty(), "unknown usage is reported, not assumed fine");
    }

    #[test]
    fn fallback_never_applies_to_non_priority_threads() {
        for p in ["", "normal", "research"] {
            assert_eq!(thread_backend(p, true, true, 0.0, 10.0), "plan");
        }
        assert_eq!(thread_backend(crate::budget::PAID_ORDER, true, true, 2.0, 10.0), "auto");
        assert_eq!(thread_backend(crate::budget::PAID_ORDER, true, true, 10.0, 10.0), "plan", "the order's API cap is spent");
        assert_eq!(thread_backend(crate::budget::PAID_ORDER, false, true, 0.0, 10.0), "plan", "fallback off");
        assert_eq!(thread_backend(crate::budget::PAID_ORDER, true, false, 0.0, 10.0), "plan", "no API key");
    }

    #[test]
    fn learns_points_per_dollar_and_burn() {
        let mk = |ts: f64, w: f64| Sample { ts, five: 0.0, week: w, week_resets_at: NOW + 99.0 * H };
        let samples = vec![mk(NOW, 0.10), mk(NOW + 1200.0, 0.11), mk(NOW + 2400.0, 0.12), mk(NOW + 3600.0, 0.13)];
        // $0.50 spent in each 20-minute interval -> 1 point / $0.50 = 2 points per dollar.
        let ppd = points_per_dollar(&samples, |_, _| 0.5).unwrap();
        assert!((ppd - 2.0).abs() < 1e-9);
        let burn = burn_last_hour(&samples, NOW + 3600.0).unwrap();
        assert!((burn - 3.0).abs() < 1e-9, "3 points in an hour");
    }
}
