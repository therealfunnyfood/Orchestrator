//! Secrets vault for business workspaces.
//!
//! Values are encrypted with Windows DPAPI (bound to the Windows user account) and kept
//! in their own file (vault.json next to settings.json), never in state.json. The UI only
//! ever sees names. Each secret has a scope:
//!   - "approved" (default): only available to `run_approved`, which runs the exact command
//!     of an approved queue item with the secrets that item names;
//!   - "all": set as an environment variable in every session of the workspace.
//! Every value is redacted from whatever the app writes or shows (see `redact`).

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Secret {
    /// DPAPI-encrypted value, base64.
    pub blob: String,
    pub scope: String,
    pub added_at: f64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct VaultFile {
    /// workspace -> name -> secret
    workspaces: BTreeMap<String, BTreeMap<String, Secret>>,
}

/// (name, value) of every secret in every workspace, for redaction.
static VALUES: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());

/// Values shorter than this aren't redacted (they'd match ordinary text).
const MIN_REDACT: usize = 6;

/// Replace every secret value in `s` with [secret:NAME]. Also catches the JSON-escaped
/// form, so it works on serialized events too.
pub fn redact(s: &str) -> String {
    let values = VALUES.read().unwrap();
    if values.is_empty() {
        return s.to_string();
    }
    let mut out = s.to_string();
    for (name, value) in values.iter().filter(|(_, v)| v.len() >= MIN_REDACT) {
        let tag = format!("[secret:{name}]");
        if out.contains(value.as_str()) {
            out = out.replace(value.as_str(), &tag);
        }
        let escaped = serde_json::to_string(value).unwrap_or_default();
        let escaped = escaped.trim_matches('"');
        if escaped != value && out.contains(escaped) {
            out = out.replace(escaped, &tag);
        }
    }
    out
}

/// The name of the first secret whose value appears in `s`.
pub fn contains_secret(s: &str) -> Option<String> {
    VALUES.read().unwrap().iter().find(|(_, v)| v.len() >= MIN_REDACT && s.contains(v.as_str())).map(|(n, _)| n.clone())
}

pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub struct Vault {
    path: PathBuf,
    data: Mutex<VaultFile>,
}

impl Vault {
    pub fn load(path: PathBuf) -> Vault {
        let data: VaultFile = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let v = Vault { path, data: Mutex::new(data) };
        v.refresh_values();
        v
    }

    fn save(&self, data: &VaultFile) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.path, serde_json::to_string_pretty(data).unwrap_or_default());
    }

    fn refresh_values(&self) {
        let data = self.data.lock().unwrap();
        let mut vals = vec![];
        for secrets in data.workspaces.values() {
            for (name, s) in secrets {
                if let Some(v) = decrypt(&s.blob) {
                    vals.push((name.clone(), v));
                }
            }
        }
        // Longest first, so a value that contains another is replaced whole.
        vals.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        *VALUES.write().unwrap() = vals;
    }

    fn ws_key(data: &VaultFile, ws: &str) -> String {
        data.workspaces.keys().find(|k| k.eq_ignore_ascii_case(ws)).cloned().unwrap_or_else(|| ws.to_string())
    }

    pub fn set(&self, ws: &str, name: &str, value: &str, scope: &str) -> Result<(), String> {
        if !valid_name(name) {
            return Err("secret names are like PINTEREST_TOKEN: capital letters, digits and _".into());
        }
        if value.is_empty() {
            return Err("the value is empty".into());
        }
        if value.len() < MIN_REDACT {
            return Err(format!("values shorter than {MIN_REDACT} characters can't be redacted reliably"));
        }
        let scope = if scope == "all" { "all" } else { "approved" };
        let blob = encrypt(value)?;
        {
            let mut data = self.data.lock().unwrap();
            let key = Self::ws_key(&data, ws);
            data.workspaces.entry(key).or_default().insert(name.to_string(), Secret { blob, scope: scope.into(), added_at: crate::store::now() });
            self.save(&data);
        }
        self.refresh_values();
        Ok(())
    }

    pub fn remove(&self, ws: &str, name: &str) -> bool {
        let removed = {
            let mut data = self.data.lock().unwrap();
            let key = Self::ws_key(&data, ws);
            let r = data.workspaces.get_mut(&key).and_then(|m| m.remove(name)).is_some();
            self.save(&data);
            r
        };
        self.refresh_values();
        removed
    }

    /// Names, scopes and dates (never values) for the UI.
    pub fn list(&self, ws: &str) -> Vec<(String, String, f64)> {
        let data = self.data.lock().unwrap();
        let key = Self::ws_key(&data, ws);
        data.workspaces.get(&key).map(|m| m.iter().map(|(n, s)| (n.clone(), s.scope.clone(), s.added_at)).collect()).unwrap_or_default()
    }

    /// Secrets scoped "all": environment variables for every session in the workspace.
    pub fn env_all(&self, ws: &str) -> Vec<(String, String)> {
        let data = self.data.lock().unwrap();
        let key = Self::ws_key(&data, ws);
        data.workspaces
            .get(&key)
            .map(|m| m.iter().filter(|(_, s)| s.scope == "all").filter_map(|(n, s)| decrypt(&s.blob).map(|v| (n.clone(), v))).collect())
            .unwrap_or_default()
    }

    /// Named secrets (any scope) for an approved command.
    pub fn get(&self, ws: &str, names: &[String]) -> Result<Vec<(String, String)>, String> {
        let data = self.data.lock().unwrap();
        let key = Self::ws_key(&data, ws);
        let m = data.workspaces.get(&key);
        names
            .iter()
            .map(|n| {
                let s = m.and_then(|m| m.get(n)).ok_or(format!("no secret named {n} in this workspace (add it in Settings → Secrets)"))?;
                decrypt(&s.blob).map(|v| (n.clone(), v)).ok_or(format!("secret {n} can't be decrypted on this Windows account"))
            })
            .collect()
    }
}

fn encrypt(value: &str) -> Result<String, String> {
    let bytes = dpapi(value.as_bytes(), true).ok_or("Windows couldn't encrypt the secret (DPAPI)")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn decrypt(blob: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(blob).ok()?;
    String::from_utf8(dpapi(&bytes, false)?).ok()
}

/// DPAPI encrypt/decrypt for the current Windows user.
#[cfg(windows)]
fn dpapi(input: &[u8], encrypt: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    let inp = CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        if encrypt {
            CryptProtectData(&inp, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&inp, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
    };
    if ok == 0 || out.pbData.is_null() {
        return None;
    }
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe { LocalFree(out.pbData as _) };
    Some(v)
}

#[cfg(not(windows))]
fn dpapi(_input: &[u8], _encrypt: bool) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the global list of values used for redaction.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn dpapi_round_trip_and_no_plain_text_on_disk() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("orch-vault-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let v = Vault::load(dir.join("vault.json"));
        v.set("C:\\ws", "TEST_TOKEN", "tok_live_ABCDEF123456", "approved").unwrap();
        let on_disk = std::fs::read_to_string(dir.join("vault.json")).unwrap();
        assert!(!on_disk.contains("tok_live_ABCDEF123456"), "the value must not be stored in plain text");
        assert_eq!(v.get("c:\\WS", &["TEST_TOKEN".into()]).unwrap()[0].1, "tok_live_ABCDEF123456");
        assert!(v.env_all("C:\\ws").is_empty(), "approved-only secrets aren't environment variables");
        assert!(v.set("C:\\ws", "bad name", "xxxxxxxx", "all").is_err());
        assert!(v.remove("C:\\ws", "TEST_TOKEN"));
        assert!(v.get("C:\\ws", &["TEST_TOKEN".into()]).is_err());
    }

    #[test]
    fn secrets_are_redacted_in_stored_transcripts() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("orch-redact-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let v = Vault::load(dir.join("vault.json"));
        v.set("C:\\ws", "PIN_TOKEN", "pina_SECRET\"value\\42", "all").unwrap();
        let hub = crate::events::Hub::new(dir.join("transcripts"), dir.join("media"));
        hub.emit("t1", serde_json::json!({"kind": "tool_use", "name": "Bash", "input": "curl -H 'Authorization: Bearer pina_SECRET\"value\\42' https://api.example"}));
        hub.emit("t1", serde_json::json!({"kind": "text", "text": "the token is pina_SECRET\"value\\42"}));
        let saved = std::fs::read_to_string(hub.path("t1")).unwrap();
        assert!(!saved.contains("pina_SECRET"), "secret leaked into the transcript: {saved}");
        assert_eq!(saved.matches("[secret:PIN_TOKEN]").count(), 2);
        assert_eq!(redact("plain text"), "plain text");
        v.remove("C:\\ws", "PIN_TOKEN");
    }
}
