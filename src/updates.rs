//! Checks GitHub for a newer release. Nothing else is sent anywhere.

use serde_json::{json, Value};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const REPO: &str = "therealfunnyfood/Orchestrator";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn parse(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split(['.', '-']).map(|p| p.parse().unwrap_or(0)).collect()
}

/// Latest release on GitHub, if it's newer than this build.
pub fn check() -> Option<Value> {
    let script = format!(
        "$ProgressPreference='SilentlyContinue'; try {{ $r = Invoke-RestMethod -UseBasicParsing -TimeoutSec 15 \
         -Headers @{{'User-Agent'='Orchestrator/{VERSION}'}} https://api.github.com/repos/{REPO}/releases/latest; \
         [pscustomobject]@{{tag=$r.tag_name; url=$r.html_url; name=$r.name}} | ConvertTo-Json -Compress }} catch {{ }}"
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-Command", &script]);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = cmd.output().ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let tag = v.get("tag")?.as_str()?.to_string();
    if parse(&tag) > parse(VERSION) {
        Some(json!({"version": tag.trim_start_matches('v'), "url": v.get("url"), "name": v.get("name")}))
    } else {
        None
    }
}
