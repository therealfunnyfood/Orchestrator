//! Business feed for a paired phone (the Atlas iPhone app's Business tab), read-only.
//! Off by default, turned on per workspace in Settings. These routes can change nothing:
//! decisions need a phone signature (later phases), never just the instance token.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// Ledger totals from `<workspace>/ledger/ledger.csv`
/// (`date,line,kind,item,amount_usd,status,notes`; amounts are signed dollars).
#[derive(Default, Debug, Clone, PartialEq, serde::Serialize)]
pub struct Totals {
    pub revenue: f64,
    pub fees: f64,
    /// Other costs (tools, materials...), negative.
    pub costs: f64,
    /// Agent usage at API rates (kind "tokens"), negative.
    pub tokens: f64,
    /// Revenue + fees + costs + tokens.
    pub net: f64,
    /// The user's own time (kind "hours", the amount column holds hours); not money.
    pub hours: f64,
    pub rows: u32,
}

impl Totals {
    fn add(&mut self, kind: &str, amount: f64) {
        self.rows += 1;
        match kind {
            "hours" | "time" => {
                self.hours += amount;
                return;
            }
            "revenue" | "sale" | "income" => self.revenue += amount,
            "fee" | "fees" => self.fees += amount,
            "tokens" | "usage" => self.tokens += amount,
            _ => self.costs += amount,
        }
        self.net += amount;
    }

    /// Net profit per hour of the user's time (None without hours logged).
    pub fn per_hour(&self) -> Option<f64> {
        (self.hours > 0.0).then(|| self.net / self.hours)
    }

    /// Net profit per dollar of agent usage (None without token costs).
    pub fn per_token_dollar(&self) -> Option<f64> {
        (self.tokens < 0.0).then(|| self.net / -self.tokens)
    }
}

#[derive(Default, Debug, Clone, PartialEq, serde::Serialize)]
pub struct Ledger {
    pub this_week: Totals,
    pub last_week: Totals,
    pub all_time: Totals,
    /// All time, per line.
    pub per_line: BTreeMap<String, Totals>,
    pub per_line_this_week: BTreeMap<String, Totals>,
    pub per_line_last_week: BTreeMap<String, Totals>,
    /// Net per line for each of the last 8 weeks (oldest first; the last is this week).
    pub weekly_net: BTreeMap<String, Vec<f64>>,
}

pub const CHART_WEEKS: usize = 8;

/// Split one CSV line (quoted fields with "" escapes).
pub fn csv_fields(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Totals for this week (from Monday), last week and all time, plus per line (all time).
pub fn ledger(text: &str, now: f64) -> Ledger {
    let (_, week_start) = crate::budget::period_starts(now);
    let last_week_start = week_start - 7.0 * 86400.0;
    let mut l = Ledger::default();
    let mut lines = text.lines();
    let header = csv_fields(lines.next().unwrap_or_default());
    let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
    let (Some(ci_date), Some(ci_line), Some(ci_kind), Some(ci_amount)) = (col("date"), col("line"), col("kind"), col("amount_usd")) else {
        return l;
    };
    for row in lines.filter(|r| !r.trim().is_empty()) {
        let f = csv_fields(row);
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        let Ok(amount) = get(ci_amount).trim_start_matches('$').replace(',', "").parse::<f64>() else { continue };
        let kind = get(ci_kind).to_lowercase();
        let ts = parse_date(get(ci_date));
        let line = if get(ci_line).is_empty() { "-".to_string() } else { get(ci_line).to_string() };
        l.all_time.add(&kind, amount);
        l.per_line.entry(line.clone()).or_default().add(&kind, amount);
        if let Some(t) = ts {
            if t >= week_start {
                l.this_week.add(&kind, amount);
                l.per_line_this_week.entry(line.clone()).or_default().add(&kind, amount);
            } else if t >= last_week_start {
                l.last_week.add(&kind, amount);
                l.per_line_last_week.entry(line.clone()).or_default().add(&kind, amount);
            }
            // Weekly net for the chart (hours aren't money).
            let weeks_ago = ((week_start - t) / (7.0 * 86400.0)).ceil().max(0.0) as usize;
            if weeks_ago < CHART_WEEKS && !matches!(kind.as_str(), "hours" | "time") {
                let series = l.weekly_net.entry(line).or_insert_with(|| vec![0.0; CHART_WEEKS]);
                series[CHART_WEEKS - 1 - weeks_ago] += amount;
            }
        }
    }
    l
}

fn parse_date(s: &str) -> Option<f64> {
    use chrono::TimeZone;
    let d = chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()?;
    chrono::Local.from_local_datetime(&d.and_hms_opt(12, 0, 0)?).earliest().map(|t| t.timestamp() as f64)
}

pub fn read_ledger(ws: &str, now: f64) -> Option<Ledger> {
    let text = std::fs::read_to_string(Path::new(ws).join("ledger").join("ledger.csv")).ok()?;
    Some(ledger(&text, now))
}

/// Facts about an attachment the phone shows before downloading it.
pub fn attachment_info(path: &str) -> Value {
    use sha2::{Digest, Sha256};
    let p = Path::new(path);
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let bytes = std::fs::read(p).ok();
    let (w, h) = image::image_dimensions(p).map(|(w, h)| (Some(w), Some(h))).unwrap_or((None, None));
    json!({
        "name": name,
        "sha256": bytes.as_ref().map(|b| format!("{:x}", Sha256::digest(b))),
        "size": bytes.as_ref().map(|b| b.len()),
        "width": w, "height": h,
        "image": w.is_some(),
    })
}

/// A JPEG thumbnail at most `max` pixels on its longer side.
pub fn thumbnail(path: &Path, max: u32) -> Result<Vec<u8>, String> {
    let img = image::open(path).map_err(|e| format!("not an image: {e}"))?;
    let t = img.thumbnail(max, max).to_rgb8();
    let mut out = std::io::Cursor::new(vec![]);
    t.write_to(&mut out, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// Push hints due now for a workspace: (key to send once, hint, text). `waiting` is
/// (item id, created_at, urgent, type) of pending items; `reservations` the open ones.
pub fn push_hints(waiting: &[(String, f64, bool, String)], reservations: &[crate::capacity::Reservation], now: f64) -> Vec<(String, &'static str, String)> {
    let mut out = vec![];
    for (id, created, urgent, kind) in waiting {
        let hours = (now - created) / 3600.0;
        // A buyer waiting on a reply (or anything marked urgent) for over 4 hours.
        if hours >= 4.0 && (*urgent || kind == "send_message") {
            out.push((format!("urgent_buyer:{id}"), "urgent_buyer", format!("{id} has waited {hours:.0} h for your answer")));
        }
    }
    for r in reservations.iter().filter(|r| r.remaining_units > 0.0 && r.deadline > r.created_at) {
        let elapsed = (now - r.created_at) / (r.deadline - r.created_at);
        for (level, pct) in [(0.75, 75), (0.5, 50)] {
            if elapsed >= level {
                out.push((
                    format!("deadline_risk:{}:{pct}", r.order_id),
                    "deadline_risk",
                    format!("Order {} is {pct}% through its time with {:.1} weekly points of work left", r.order_id, r.remaining_units),
                ));
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> f64 {
        chrono::Local.with_ymd_and_hms(y, m, d, 15, 0, 0).earliest().unwrap().timestamp() as f64
    }

    #[test]
    fn ledger_totals_by_week_and_line() {
        let csv = "date,line,kind,item,amount_usd,status,notes\n\
2026-11-25,A,revenue,gig-1,40.00,paid,\n\
2026-11-25,A,fee,gig-1,-8.00,paid,\"Fiverr 20%, standard\"\n\
2026-11-24,B,tokens,listing-thread,-1.25,paid,\n\
2026-11-18,B,revenue,mug,15,paid,\n\
2026-11-01,C,cost,canva,-3,paid,\n\
bad,row\n";
        // Wednesday 2026-11-25: this week started Monday 11-23.
        let l = ledger(csv, at(2026, 11, 25));
        assert_eq!(l.this_week.revenue, 40.0);
        assert_eq!(l.this_week.fees, -8.0);
        assert_eq!(l.this_week.tokens, -1.25);
        assert_eq!(l.this_week.costs, 0.0);
        assert!((l.this_week.net - 30.75).abs() < 1e-9);
        assert_eq!(l.last_week.revenue, 15.0);
        assert_eq!(l.all_time.rows, 5);
        assert!((l.all_time.net - 42.75).abs() < 1e-9);
        assert_eq!(l.per_line["A"].net, 32.0);
        assert_eq!(l.per_line_this_week["B"].tokens, -1.25);
        assert_eq!(l.per_line_last_week["B"].revenue, 15.0);
        // Weekly chart: B made 15 last week and spent 1.25 this week; C's cost is 4 weeks back.
        assert_eq!(l.weekly_net["B"][CHART_WEEKS - 1], -1.25);
        assert_eq!(l.weekly_net["B"][CHART_WEEKS - 2], 15.0);
        assert_eq!(l.weekly_net["C"][CHART_WEEKS - 5], -3.0);
        // Hours count as the user's time, not money.
        let with_hours = format!("{csv}2026-11-25,A,hours,gig-1,2,,\n");
        let l = ledger(&with_hours, at(2026, 11, 25));
        assert_eq!(l.per_line["A"].hours, 2.0);
        assert_eq!(l.per_line["A"].net, 32.0);
        assert_eq!(l.per_line["A"].per_hour(), Some(16.0));
        assert_eq!(l.per_line["B"].per_token_dollar(), Some(13.75 / 1.25));
        assert_eq!(csv_fields("a,\"b, c\",\"say \"\"hi\"\"\""), vec!["a", "b, c", "say \"hi\""]);
    }

    #[test]
    fn push_hints_for_waiting_buyers_and_deadlines() {
        let now = 1_800_000_000.0;
        let waiting = vec![
            ("q1".to_string(), now - 5.0 * 3600.0, false, "send_message".to_string()),
            ("q2".to_string(), now - 5.0 * 3600.0, false, "post_pin".to_string()),
            ("q3".to_string(), now - 3.0 * 3600.0, true, "spend".to_string()),
        ];
        let r = crate::capacity::Reservation { order_id: "o1".into(), remaining_units: 4.0, est_units: 8.0, created_at: now - 60.0 * 3600.0, deadline: now + 40.0 * 3600.0, ..Default::default() };
        let hints = push_hints(&waiting, &[r], now);
        let keys: Vec<&str> = hints.iter().map(|h| h.0.as_str()).collect();
        assert_eq!(keys, vec!["urgent_buyer:q1", "deadline_risk:o1:50"]);
    }

    #[test]
    fn thumbnails_are_small_jpegs() {
        let dir = std::env::temp_dir().join(format!("orch-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("big.png");
        image::RgbaImage::from_pixel(2000, 1000, image::Rgba([200, 100, 50, 255])).save(&p).unwrap();
        let jpg = thumbnail(&p, 512).unwrap();
        let t = image::load_from_memory(&jpg).unwrap();
        assert_eq!((t.width(), t.height()), (512, 256));
        let info = attachment_info(&p.to_string_lossy());
        assert_eq!(info["width"], 2000);
        assert_eq!(info["image"], true);
    }
}
