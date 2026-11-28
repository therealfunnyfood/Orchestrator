//! Approval queue for business workspaces.
//!
//! Agents propose outbound actions (publishing, messaging, spending) with `queue_item`.
//! Only the user approves, by clicking in the desktop window: the click reaches the app
//! through the webview's private IPC channel, never over HTTP, because threads can read
//! the local API token (instance.json, their own MCP config). An approval stores a hash
//! of everything that will be executed (type, line, payload, who executes, and the bytes
//! of every attachment); `list_approved` returns only items whose hash still matches, so
//! nothing can change after approval. The app's store is the source of truth; the files
//! in `<workspace>/queue/<status>/` are read-only copies for reading.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const STATUSES: [&str; 5] = ["pending", "approved", "rejected", "done", "void"];
pub const RISKS: [&str; 3] = ["low", "medium", "high"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct QueueItem {
    pub id: String,
    pub workspace: String,
    /// The item's type (publish_listing, send_message, spend, ...): free text, so new
    /// business lines don't need an app change.
    #[serde(rename = "type")]
    pub kind: String,
    pub line: String,
    pub summary: String,
    /// Exactly what will be executed or sent.
    pub payload: String,
    pub expected_value_usd: f64,
    pub risk: String,
    /// "agent" (an agent executes it after approval) or "user" (the user does it by hand).
    pub who_executes: String,
    pub urgent: bool,
    /// Absolute paths; their contents are part of the approval hash.
    pub attachments: Vec<String>,
    pub created_by: String,
    pub created_at: f64,
    pub status: String,
    /// Hash of what was approved; empty unless approved.
    pub approved_hash: String,
    pub decided_at: f64,
    /// The user's reason (rejections) or note.
    pub note: String,
    /// The user edited the payload before approving.
    pub edited: bool,
    pub result: String,
    pub done_at: f64,
    /// File name of the read-only copy in queue/<status>/.
    pub file: String,
}

/// Hash of everything an approval covers. Attachments are hashed by content, so a file
/// swapped after approval no longer matches.
pub fn content_hash(item: &QueueItem) -> String {
    let mut h = Sha256::new();
    for part in [&item.kind, &item.line, &item.payload, &item.who_executes] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    for a in &item.attachments {
        h.update((a.len() as u64).to_le_bytes());
        h.update(a.as_bytes());
        match std::fs::read(a) {
            Ok(bytes) => h.update(Sha256::digest(&bytes)),
            Err(_) => h.update(b"<missing>"),
        }
    }
    format!("{:x}", h.finalize())
}

/// Approved items that still match what the user approved.
pub fn still_approved<'a>(items: impl Iterator<Item = &'a QueueItem>) -> (Vec<&'a QueueItem>, Vec<&'a QueueItem>) {
    let mut ok = vec![];
    let mut changed = vec![];
    for i in items.filter(|i| i.status == "approved") {
        if !i.approved_hash.is_empty() && content_hash(i) == i.approved_hash {
            ok.push(i);
        } else {
            changed.push(i);
        }
    }
    (ok, changed)
}

/// What the user decided in the Queue tab.
pub enum Decision {
    Approve,
    /// Edit the payload, then approve it as edited.
    EditApprove(String),
    Reject(String),
}

/// Apply the user's decision to a pending item. Only reachable from the desktop window.
pub fn decide(item: &mut QueueItem, d: Decision, note: &str, now: f64) -> Result<(), String> {
    if item.status != "pending" {
        return Err(format!("item {} is already {}", item.id, item.status));
    }
    match d {
        Decision::Approve => {}
        Decision::EditApprove(payload) => {
            if payload.trim().is_empty() {
                return Err("the edited payload is empty".into());
            }
            item.edited = item.payload != payload;
            item.payload = payload;
        }
        Decision::Reject(reason) => {
            if reason.trim().is_empty() {
                return Err("a rejection needs a one-line reason".into());
            }
            item.status = "rejected".into();
            item.note = reason.trim().to_string();
            item.decided_at = now;
            return Ok(());
        }
    }
    item.approved_hash = content_hash(item);
    item.status = "approved".into();
    item.note = note.trim().to_string();
    item.decided_at = now;
    Ok(())
}

/// Mark an approved item done. It must still match its approval.
pub fn complete(item: &mut QueueItem, result: &str, now: f64) -> Result<(), String> {
    if item.status != "approved" {
        return Err(format!("item {} is {}, not approved", item.id, item.status));
    }
    if content_hash(item) != item.approved_hash {
        return Err(format!("item {} changed since it was approved; it can't be executed", item.id));
    }
    item.status = "done".into();
    item.result = result.trim().to_string();
    item.done_at = now;
    Ok(())
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The read-only copy of an item, in the format of the workspace's queue/README.md.
pub fn render(item: &QueueItem) -> String {
    let mut s = format!(
        "---\nid: {}\ntype: {}\nline: {}\nstatus: {}\nurgent: {}\nexpected_value_usd: {:.2}\nrisk: {}\nwho_executes: {}\ncreated_by: {}\n---\n\
<!-- Written by the Orchestrator app. Editing or moving this file changes nothing: decisions are made in the app's Queue tab. -->\n\n\
## What\n{}\n\n## Payload\n{}\n",
        item.id, item.kind, item.line, item.status, item.urgent, item.expected_value_usd, item.risk, item.who_executes,
        item.created_by, item.summary, item.payload
    );
    if !item.attachments.is_empty() {
        s += "\n## Attachments\n";
        for a in &item.attachments {
            s += &format!("- {a}\n");
        }
    }
    if item.status != "pending" {
        s += &format!("\n## Decision\n{}{}\n", item.status, if item.edited { " (payload edited by the user before approval)" } else { "" });
        if !item.note.is_empty() {
            s += &format!("\nUser note: {}\n", item.note);
        }
    }
    if item.status == "done" {
        s += &format!("\n## Result\n{}\n", item.result);
    }
    s
}

/// Write the item's read-only copy into queue/<status>/ and remove copies elsewhere.
pub fn mirror(item: &QueueItem) {
    let root = Path::new(&item.workspace).join("queue");
    for st in STATUSES {
        let p = root.join(st).join(&item.file);
        if st != item.status && p.exists() {
            set_readonly(&p, false);
            let _ = std::fs::remove_file(&p);
        }
    }
    let dir = root.join(&item.status);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let p: PathBuf = dir.join(&item.file);
    if p.exists() {
        set_readonly(&p, false);
    }
    let _ = std::fs::write(&p, render(item));
    set_readonly(&p, true);
}

fn set_readonly(p: &Path, ro: bool) {
    if let Ok(m) = std::fs::metadata(p) {
        let mut perm = m.permissions();
        perm.set_readonly(ro);
        let _ = std::fs::set_permissions(p, perm);
    }
}

/// What `list_approved` may return: nothing at all while outbound is paused.
pub fn listable<'a>(items: impl Iterator<Item = &'a QueueItem>, outbound_paused: bool) -> Vec<&'a QueueItem> {
    if outbound_paused {
        return vec![];
    }
    still_approved(items).0
}

/// The chain hash that ends an audit line: it covers the previous line's hash, so an
/// edited or deleted line breaks the chain.
fn chain_hash(prev: &str, body: &str) -> String {
    let h = Sha256::digest(format!("{prev}\n{body}").as_bytes());
    format!("{:x}", h)[..16].to_string()
}

/// The chain hash at the end of the file's last line ("" for an empty or missing log).
pub fn audit_file_tip(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().rev().find(|l| !l.trim().is_empty()).and_then(|l| l.rsplit_once("  #")).map(|(_, h)| h.trim().to_string()).unwrap_or_default()
}

/// Lines whose chain hash doesn't match: an edited, inserted or deleted line shows up here.
pub fn audit_broken_lines(path: &Path) -> Vec<usize> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut prev = String::new();
    let mut bad = vec![];
    for (n, l) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        match l.rsplit_once("  #") {
            Some((body, h)) if chain_hash(&prev, body) == h.trim() => prev = h.trim().to_string(),
            Some((_, h)) => {
                bad.push(n + 1);
                prev = h.trim().to_string();
            }
            None => bad.push(n + 1),
        }
    }
    bad
}

/// Append a line to the workspace's audit log (queue/audit.log), chained to the previous
/// one. `tip` is the chain hash the app last wrote; if the file ends differently, it was
/// changed outside the app, and that is logged first. Returns the new tip. The file is
/// kept read-only between writes; the app never rewrites it.
pub fn audit_append(workspace: &str, tip: &str, line: &str) -> String {
    use std::io::Write;
    let dir = Path::new(workspace).join("queue");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("audit.log");
    let found = audit_file_tip(&path);
    let mut lines = vec![];
    // Broken links after the last TAMPER line are new (earlier ones were already reported).
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let last_report = text.lines().enumerate().filter(|(_, l)| l.contains("  TAMPER: ") || l.contains("  LEGACY: ")).map(|(n, _)| n + 1).last().unwrap_or(0);
    let new_breaks: Vec<usize> = audit_broken_lines(&path).into_iter().filter(|&n| n > last_report).collect();
    let chained_before = text.lines().any(|l| l.rsplit_once("  #").is_some_and(|(_, h)| h.len() == 16));
    if tip.is_empty() && !chained_before && !text.trim().is_empty() {
        // A log from before 0.5.1, which didn't chain its lines: start the chain here.
        lines.push(format!("LEGACY: the {} line(s) above were written before this log was hash-chained", text.lines().filter(|l| !l.trim().is_empty()).count()));
    } else if found != tip || !new_breaks.is_empty() {
        lines.push(format!(
            "TAMPER: audit.log was changed outside the app (the app last wrote #{}, the file ends with #{}{})",
            if tip.is_empty() { "-" } else { tip },
            if found.is_empty() { "-" } else { &found },
            if new_breaks.is_empty() { String::new() } else { format!("; altered lines: {:?}", new_breaks) }
        ));
    }
    lines.push(line.replace(['\n', '\r'], " "));
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    set_readonly(&path, false);
    let mut prev = if chained_before { found } else { String::new() };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        for l in lines {
            let body = format!("{stamp}  {l}");
            let h = chain_hash(&prev, &body);
            let _ = writeln!(f, "{body}  #{h}");
            prev = h;
        }
    }
    set_readonly(&path, true);
    prev
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(dir: &Path) -> QueueItem {
        let att = dir.join("design.png");
        std::fs::write(&att, b"original image bytes").unwrap();
        QueueItem {
            id: "q1".into(),
            workspace: dir.to_string_lossy().into(),
            kind: "publish_listing".into(),
            line: "B".into(),
            summary: "List the cat mug".into(),
            payload: "title: Cat mug\nprice: 14.00".into(),
            risk: "low".into(),
            who_executes: "agent".into(),
            attachments: vec![att.to_string_lossy().into()],
            status: "pending".into(),
            file: "q1.md".into(),
            ..Default::default()
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("orch-queue-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn approved_item_is_listed() {
        let d = tmp("listed");
        let mut i = item(&d);
        decide(&mut i, Decision::Approve, "", 1.0).unwrap();
        let items = vec![i];
        let (ok, changed) = still_approved(items.iter());
        assert_eq!(ok.len(), 1);
        assert!(changed.is_empty());
    }

    #[test]
    fn payload_changed_after_approval_is_not_listed() {
        let d = tmp("payload");
        let mut i = item(&d);
        decide(&mut i, Decision::Approve, "", 1.0).unwrap();
        i.payload = "title: Cat mug\nprice: 1.00".into();
        let items = vec![i.clone()];
        let (ok, changed) = still_approved(items.iter());
        assert!(ok.is_empty(), "a changed payload must not be returned");
        assert_eq!(changed.len(), 1);
        assert!(complete(&mut i, "done", 2.0).is_err(), "a changed item can't be completed");
    }

    #[test]
    fn attachment_swapped_after_approval_is_not_listed() {
        let d = tmp("attachment");
        let mut i = item(&d);
        decide(&mut i, Decision::Approve, "", 1.0).unwrap();
        std::fs::write(&i.attachments[0], b"a different image").unwrap();
        let items = vec![i];
        assert!(still_approved(items.iter()).0.is_empty());
    }

    #[test]
    fn pending_and_rejected_items_are_never_listed() {
        let d = tmp("pending");
        let pending = item(&d);
        let mut rejected = item(&d);
        decide(&mut rejected, Decision::Reject("off-brand".into()), "", 1.0).unwrap();
        // Even a forged hash on a pending item doesn't make it approved.
        let mut forged = item(&d);
        forged.approved_hash = content_hash(&forged);
        let items = vec![pending, rejected, forged];
        assert!(still_approved(items.iter()).0.is_empty());
    }

    #[test]
    fn rejection_needs_a_reason() {
        let d = tmp("reason");
        let mut i = item(&d);
        assert!(decide(&mut i, Decision::Reject("  ".into()), "", 1.0).is_err());
        assert_eq!(i.status, "pending");
    }

    #[test]
    fn edit_then_approve_covers_the_edited_payload() {
        let d = tmp("edit");
        let mut i = item(&d);
        decide(&mut i, Decision::EditApprove("title: Cat mug\nprice: 16.00".into()), "", 1.0).unwrap();
        assert!(i.edited);
        let items = vec![i.clone()];
        assert_eq!(still_approved(items.iter()).0.len(), 1);
        complete(&mut i, "listed as #123", 2.0).unwrap();
        assert_eq!(i.status, "done");
    }

    #[test]
    fn decided_items_cant_be_decided_again() {
        let d = tmp("twice");
        let mut i = item(&d);
        decide(&mut i, Decision::Reject("no".into()), "", 1.0).unwrap();
        assert!(decide(&mut i, Decision::Approve, "", 2.0).is_err());
    }

    #[test]
    fn kill_switch_blocks_list_approved() {
        let d = tmp("killswitch");
        let mut i = item(&d);
        decide(&mut i, Decision::Approve, "", 1.0).unwrap();
        let items = vec![i];
        assert_eq!(listable(items.iter(), false).len(), 1);
        assert!(listable(items.iter(), true).is_empty(), "nothing may be listed while outbound is paused");
    }

    #[test]
    fn audit_log_is_chained_and_detects_edits() {
        let d = tmp("audit");
        let ws = d.to_string_lossy().to_string();
        let path = d.join("queue/audit.log");
        let mut tip = audit_append(&ws, "", "QUEUED q1");
        tip = audit_append(&ws, &tip, "APPROVED q1 by=user");
        assert!(audit_broken_lines(&path).is_empty());
        assert!(std::fs::metadata(&path).unwrap().permissions().readonly(), "kept read-only between writes");
        // Someone rewrites history outside the app.
        set_readonly(&path, false);
        let text = std::fs::read_to_string(&path).unwrap().replace("APPROVED q1 by=user", "REJECTED q1 by=user");
        std::fs::write(&path, text).unwrap();
        assert_eq!(audit_broken_lines(&path), vec![2]);
        // The next append notices the file no longer ends where the app left it.
        let _ = audit_append(&ws, &tip, "EXECUTED q2");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("TAMPER: audit.log was changed outside the app"));
    }

    #[test]
    fn unchained_log_from_before_is_marked_legacy_not_tampered() {
        let d = tmp("legacy");
        let ws = d.to_string_lossy().to_string();
        std::fs::create_dir_all(d.join("queue")).unwrap();
        std::fs::write(d.join("queue/audit.log"), "2026-11-28 10:50:45  QUEUED q1\n2026-11-28 10:53:22  APPROVED q1 by=user\n").unwrap();
        let tip = audit_append(&ws, "", "QUEUED q2");
        let text = std::fs::read_to_string(d.join("queue/audit.log")).unwrap();
        assert!(text.contains("LEGACY: the 2 line(s) above"));
        assert!(!text.contains("TAMPER"));
        // From here on the chain holds.
        let _ = audit_append(&ws, &tip, "APPROVED q2 by=user");
        assert!(!std::fs::read_to_string(d.join("queue/audit.log")).unwrap().contains("TAMPER"));
    }

    #[test]
    fn mirror_moves_the_read_only_copy() {
        let d = tmp("mirror");
        let mut i = item(&d);
        mirror(&i);
        assert!(d.join("queue/pending/q1.md").exists());
        decide(&mut i, Decision::Approve, "", 1.0).unwrap();
        mirror(&i);
        assert!(!d.join("queue/pending/q1.md").exists());
        let p = d.join("queue/approved/q1.md");
        assert!(std::fs::metadata(&p).unwrap().permissions().readonly());
        assert!(std::fs::read_to_string(&p).unwrap().contains("Editing or moving this file changes nothing"));
    }
}
