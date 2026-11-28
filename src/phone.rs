//! Signed decisions from a paired phone (the Atlas iPhone app's Business tab).
//!
//! The instance token is shared with Orchestrator's own agents, so it never decides
//! anything by itself. A phone decision is accepted only when it carries a P-256 ECDSA
//! signature, made by a phone key the user enrolled in the Orchestrator window, over the
//! exact domain-separated string
//!   atlas-orch/v1|decide|<device>|<ws>|<item_id>|<decision>|<payload_hash>|<sha256(reason)>|<ts>|<nonce>
//! with the item's *current* payload hash, a timestamp within 120 s, and a nonce never seen
//! before for that device. Formats match Atlas's own approval key: a 65-byte uncompressed
//! X9.63 public key, a DER signature over SHA-256 of the UTF-8 string, and a fingerprint of
//! the first 50 bits (10 Crockford characters) of SHA-256(key).

use base64::Engine;
use p256::ecdsa::signature::Verifier;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "atlas-orch/v1";
pub const MAX_SKEW_S: f64 = 120.0;
/// Nonces are remembered at least this long (longer than the timestamp window).
pub const NONCE_KEEP_S: f64 = 15.0 * 60.0;
const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Phone {
    pub device: String,
    pub name: String,
    /// The approve key, X9.63 uncompressed (65 bytes), base64.
    pub key: String,
    pub fingerprint: String,
    pub enrolled_at: f64,
    /// Set when revoked; a revoked phone can never decide again.
    pub revoked_at: f64,
}

/// Crockford base32 of the first 50 bits of SHA-256(key), as XXXXX-XXXXX (as Atlas shows it).
pub fn fingerprint(key_x963: &[u8]) -> String {
    let d = Sha256::digest(key_x963);
    let bits = u64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]);
    let chars: String = (0..10).map(|i| CROCKFORD[((bits >> (64 - 5 * (i + 1))) & 31) as usize] as char).collect();
    format!("{}-{}", &chars[..5], &chars[5..])
}

fn b64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    base64::engine::general_purpose::STANDARD.decode(s)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')))
        .ok()
}

/// Parse a phone's enrolment bundle: JSON `{"device", "name", "approve_key"}` or the compact
/// QR form `atlas-orch/v1|pair|<device>|<approve_key base64>|<name>`. Checks the key is a
/// valid P-256 point.
pub fn parse_bundle(raw: &str) -> Result<Phone, String> {
    let raw = raw.trim();
    let (device, name, key) = if raw.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(raw).map_err(|e| format!("not valid JSON: {e}"))?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        (s("device"), s("name"), s("approve_key"))
    } else {
        let parts: Vec<&str> = raw.split('|').collect();
        if parts.len() < 4 || parts[0] != PREFIX || parts[1] != "pair" {
            return Err(format!("expected {PREFIX}|pair|<device>|<approve_key>|<name>, or JSON"));
        }
        (parts[2].trim().into(), parts.get(4..).map(|p| p.join("|")).unwrap_or_default().trim().into(), parts[3].trim().into())
    };
    if device.is_empty() || device.len() > 64 || !device.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        return Err("the device id must be 1-64 letters, digits, - _ or .".into());
    }
    let bytes = b64(&key).ok_or("approve_key isn't base64")?;
    if bytes.len() != 65 || bytes[0] != 4 {
        return Err("approve_key must be a 65-byte uncompressed P-256 point (x963)".into());
    }
    p256::ecdsa::VerifyingKey::from_sec1_bytes(&bytes).map_err(|_| "approve_key isn't a valid P-256 point".to_string())?;
    Ok(Phone {
        device,
        name: if name.is_empty() { "iPhone".into() } else { name.chars().take(60).collect() },
        key: base64::engine::general_purpose::STANDARD.encode(&bytes),
        fingerprint: fingerprint(&bytes),
        ..Default::default()
    })
}

pub fn sha256_hex(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

/// The exact string a phone signs for a decision.
pub fn decide_message(device: &str, ws: &str, item_id: &str, decision: &str, payload_hash: &str, reason: &str, ts: i64, nonce: &str) -> String {
    format!("{PREFIX}|decide|{device}|{ws}|{item_id}|{decision}|{payload_hash}|{}|{ts}|{nonce}", sha256_hex(reason))
}

/// Does `sig_b64` (a DER ECDSA signature) verify `message` under this phone's key?
pub fn verify(phone: &Phone, message: &str, sig_b64: &str) -> bool {
    let (Some(key), Some(sig)) = (b64(&phone.key), b64(sig_b64)) else { return false };
    let Ok(vk) = p256::ecdsa::VerifyingKey::from_sec1_bytes(&key) else { return false };
    let Ok(sig) = p256::ecdsa::Signature::from_der(&sig) else { return false };
    vk.verify(message.as_bytes(), &sig).is_ok()
}

/// A decision request as the phone sends it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DecideRequest {
    pub item_id: String,
    pub decision: String,
    pub reason: String,
    pub device: String,
    pub ts: i64,
    pub nonce: String,
    pub sig: String,
}

/// Why a phone decision was refused: (HTTP status, error code, message).
pub type Refusal = (u16, &'static str, String);

fn refuse(status: u16, code: &'static str, msg: impl Into<String>) -> Refusal {
    (status, code, msg.into())
}

/// The item as Orchestrator knows it now.
pub struct ItemNow<'a> {
    pub status: &'a str,
    pub payload_hash: String,
}

/// Every check on a phone decision, in one place. `item` is None when there's no such item
/// in the workspace; `nonce_used` says whether this device already used the nonce. On
/// success returns the phone; the caller records the nonce, then calls the same decide
/// function the window uses.
pub fn check_decide<'a>(phones: &'a [Phone], req: &DecideRequest, ws_id: &str, item: Option<ItemNow>, now: f64, nonce_used: bool) -> Result<&'a Phone, Refusal> {
    if req.sig.trim().is_empty() {
        return Err(refuse(401, "signature_required", "a decision needs a signature from an enrolled phone; the instance token alone can't decide"));
    }
    if !["approve", "reject"].contains(&req.decision.as_str()) {
        return Err(refuse(400, "bad_request", "decision must be approve or reject"));
    }
    if req.nonce.len() < 16 || req.nonce.len() > 128 || !req.nonce.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) {
        return Err(refuse(400, "bad_request", "nonce must be 16-128 characters of A-Z a-z 0-9 - _"));
    }
    let phone = phones.iter().find(|p| p.device == req.device).ok_or_else(|| refuse(401, "unknown_device", "this device isn't enrolled in Orchestrator"))?;
    if phone.revoked_at > 0.0 {
        return Err(refuse(401, "revoked_device", "this device was revoked"));
    }
    if (now - req.ts as f64).abs() > MAX_SKEW_S {
        return Err(refuse(401, "stale_ts", format!("ts must be within {MAX_SKEW_S} s of Orchestrator's clock")));
    }
    if nonce_used {
        return Err(refuse(409, "replayed_nonce", "this nonce was already used"));
    }
    let item = item.ok_or_else(|| refuse(404, "no_item", "no such item in this workspace"))?;
    // The signature covers the item's *current* hash: an edited item, or a signature made
    // for another item, workspace, decision or reason, doesn't verify.
    let msg = decide_message(&req.device, ws_id, &req.item_id, &req.decision, &item.payload_hash, &req.reason, req.ts, &req.nonce);
    if !verify(phone, &msg, &req.sig) {
        return Err(refuse(401, "bad_signature", "the signature doesn't match this item's current payload hash, or wasn't made by this device"));
    }
    if item.status != "pending" {
        return Err(refuse(409, "not_pending", format!("the item is already {}", item.status)));
    }
    if req.decision == "reject" && req.reason.trim().is_empty() {
        return Err(refuse(400, "reason_required", "a rejection needs a reason"));
    }
    Ok(phone)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use p256::ecdsa::{signature::Signer, Signature, SigningKey};

    /// A phone for tests: its key, and its enrolment.
    pub fn test_phone(device: &str, seed: u8) -> (SigningKey, Phone) {
        let sk = SigningKey::from_bytes(&[seed; 32].into()).unwrap();
        let x963 = sk.verifying_key().to_encoded_point(false);
        let bundle = format!("{PREFIX}|pair|{device}|{}|Test iPhone", base64::engine::general_purpose::STANDARD.encode(x963.as_bytes()));
        (sk, parse_bundle(&bundle).unwrap())
    }

    pub fn sign(sk: &SigningKey, msg: &str) -> String {
        let sig: Signature = sk.sign(msg.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(sig.to_der().as_bytes())
    }

    const NOW: f64 = 1_800_000_000.0;
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn req(sk: &SigningKey, device: &str, ws: &str, item: &str, decision: &str, hash: &str, reason: &str, ts: i64, nonce: &str) -> DecideRequest {
        DecideRequest {
            item_id: item.into(), decision: decision.into(), reason: reason.into(), device: device.into(), ts, nonce: nonce.into(),
            sig: sign(sk, &decide_message(device, ws, item, decision, hash, reason, ts, nonce)),
        }
    }

    fn pending() -> Option<ItemNow<'static>> {
        Some(ItemNow { status: "pending", payload_hash: HASH.into() })
    }

    const NONCE: &str = "n0nce-0123456789abcdef";

    #[test]
    fn a_valid_signature_is_accepted() {
        let (sk, ph) = test_phone("iphone-1", 7);
        let r = req(&sk, "iphone-1", "orch-ws", "q1", "approve", HASH, "", NOW as i64, NONCE);
        assert!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, false).is_ok());
    }

    #[test]
    fn token_without_a_signature_is_refused() {
        let (_, ph) = test_phone("iphone-1", 7);
        let r = DecideRequest { item_id: "q1".into(), decision: "approve".into(), device: "iphone-1".into(), ts: NOW as i64, nonce: NONCE.into(), ..Default::default() };
        assert_eq!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "signature_required");
    }

    #[test]
    fn unenrolled_or_revoked_keys_are_refused() {
        let (sk, mut ph) = test_phone("iphone-1", 7);
        let (other_sk, _) = test_phone("iphone-1", 9);
        // A different key claiming to be the enrolled device.
        let forged = req(&other_sk, "iphone-1", "orch-ws", "q1", "approve", HASH, "", NOW as i64, NONCE);
        assert_eq!(check_decide(&[ph.clone()], &forged, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
        // A device nobody enrolled.
        let stranger = req(&other_sk, "iphone-2", "orch-ws", "q1", "approve", HASH, "", NOW as i64, NONCE);
        assert_eq!(check_decide(&[ph.clone()], &stranger, "orch-ws", pending(), NOW, false).unwrap_err().1, "unknown_device");
        // Revoked.
        ph.revoked_at = NOW - 10.0;
        let r = req(&sk, "iphone-1", "orch-ws", "q1", "approve", HASH, "", NOW as i64, NONCE);
        assert_eq!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "revoked_device");
    }

    #[test]
    fn a_wrong_hash_or_an_edited_item_is_refused() {
        let (sk, ph) = test_phone("iphone-1", 7);
        // Signed over an old hash; the item has changed since.
        let r = req(&sk, "iphone-1", "orch-ws", "q1", "approve", "bbbb", "", NOW as i64, NONCE);
        assert_eq!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
    }

    #[test]
    fn a_replayed_nonce_is_refused() {
        let (sk, ph) = test_phone("iphone-1", 7);
        let r = req(&sk, "iphone-1", "orch-ws", "q1", "approve", HASH, "", NOW as i64, NONCE);
        assert_eq!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, true).unwrap_err().1, "replayed_nonce");
    }

    #[test]
    fn a_timestamp_out_of_window_is_refused() {
        let (sk, ph) = test_phone("iphone-1", 7);
        for skew in [-121i64, 121, -100_000] {
            let ts = NOW as i64 + skew;
            let r = req(&sk, "iphone-1", "orch-ws", "q1", "approve", HASH, "", ts, NONCE);
            assert_eq!(check_decide(&[ph.clone()], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "stale_ts", "skew {skew}");
        }
    }

    #[test]
    fn a_signature_over_another_item_workspace_or_decision_is_refused() {
        let (sk, ph) = test_phone("iphone-1", 7);
        let ts = NOW as i64;
        // Signed for q2, sent for q1.
        let mut r = req(&sk, "iphone-1", "orch-ws", "q2", "approve", HASH, "", ts, NONCE);
        r.item_id = "q1".into();
        assert_eq!(check_decide(&[ph.clone()], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
        // Signed for another workspace.
        let r = req(&sk, "iphone-1", "orch-other", "q1", "approve", HASH, "", ts, NONCE);
        assert_eq!(check_decide(&[ph.clone()], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
        // Signed as a reject, sent as an approve.
        let mut r = req(&sk, "iphone-1", "orch-ws", "q1", "reject", HASH, "no", ts, NONCE);
        r.decision = "approve".into();
        assert_eq!(check_decide(&[ph.clone()], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
        // Signed with one reason, sent with another.
        let mut r = req(&sk, "iphone-1", "orch-ws", "q1", "reject", HASH, "too pricey", ts, NONCE);
        r.reason = "fine".into();
        assert_eq!(check_decide(&[ph.clone()], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
        // Signed with another nonce or time.
        let mut r = req(&sk, "iphone-1", "orch-ws", "q1", "approve", HASH, "", ts, NONCE);
        r.nonce = "other-nonce-0123456789".into();
        assert_eq!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
    }

    #[test]
    fn an_atlas_approval_signature_is_refused() {
        let (sk, ph) = test_phone("iphone-1", 7);
        let ts = NOW as i64;
        let atlas = format!("atlas-mobile/v1|approval|iphone-1|q1|approve|once|{HASH}|{ts}");
        let r = DecideRequest { item_id: "q1".into(), decision: "approve".into(), device: "iphone-1".into(), ts, nonce: NONCE.into(), sig: sign(&sk, &atlas), ..Default::default() };
        assert_eq!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "bad_signature");
    }

    #[test]
    fn reject_without_a_reason_is_refused() {
        let (sk, ph) = test_phone("iphone-1", 7);
        let r = req(&sk, "iphone-1", "orch-ws", "q1", "reject", HASH, "  ", NOW as i64, NONCE);
        assert_eq!(check_decide(&[ph], &r, "orch-ws", pending(), NOW, false).unwrap_err().1, "reason_required");
    }

    #[test]
    fn decided_or_missing_items_are_refused() {
        let (sk, ph) = test_phone("iphone-1", 7);
        let r = req(&sk, "iphone-1", "orch-ws", "q1", "approve", HASH, "", NOW as i64, NONCE);
        assert_eq!(check_decide(&[ph.clone()], &r, "orch-ws", None, NOW, false).unwrap_err().1, "no_item");
        let done = Some(ItemNow { status: "approved", payload_hash: HASH.into() });
        assert_eq!(check_decide(&[ph], &r, "orch-ws", done, NOW, false).unwrap_err().1, "not_pending");
    }

    /// The same value Atlas's devices.fingerprint() gives for this key (computed with its
    /// Python code: crockford(sha256(key))[:10]).
    #[test]
    fn fingerprint_matches_atlas_for_a_known_key() {
        let mut key = vec![4u8];
        key.extend(1..=64u8);
        assert_eq!(fingerprint(&key), "1V9TD-AWNFZ");
    }

    #[test]
    fn bundles_and_fingerprints_match_atlas() {
        let (_, ph) = test_phone("iphone-1", 7);
        assert_eq!(ph.fingerprint.len(), 11);
        assert!(ph.fingerprint.chars().all(|c| c == '-' || CROCKFORD.contains(&(c as u8))));
        // Crockford of SHA-256(key): the same bits Atlas's devices.fingerprint() takes.
        let key = b64(&ph.key).unwrap();
        let d = Sha256::digest(&key);
        let first = CROCKFORD[(d[0] >> 3) as usize] as char;
        assert_eq!(ph.fingerprint.chars().next().unwrap(), first);
        assert!(parse_bundle("atlas-mobile/v1|pair|x|y|z").is_err());
        assert!(parse_bundle(&format!("{PREFIX}|pair|dev|{}|n", base64::engine::general_purpose::STANDARD.encode([4u8; 65]))).is_err(), "not a point on the curve");
        let json = format!(r#"{{"device":"iphone-9","name":"Test iPhone 2","approve_key":"{}"}}"#, ph.key);
        assert_eq!(parse_bundle(&json).unwrap().fingerprint, ph.fingerprint);
    }
}
