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

/// Append one line to the workspace's audit log (queue/audit.log). Append-only: the
/// app never rewrites it.
pub fn audit(workspace: &str, line: &str) {
    use std::io::Write;
    let dir = Path::new(workspace).join("queue");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("audit.log")) {
        let _ = writeln!(f, "{stamp}  {}", line.replace('\n', " "));
    }
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
