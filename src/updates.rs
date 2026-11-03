//! Update check and one-click update from GitHub releases. Nothing else is sent anywhere.

use serde_json::{json, Value};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const REPO: &str = "therealfunnyfood/Orchestrator";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn parse(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split(['.', '-']).map(|p| p.parse().unwrap_or(0)).collect()
}

fn powershell(script: &str) -> Option<String> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script]);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = cmd.output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Latest release on GitHub, if it's newer than this build.
pub fn check() -> Option<Value> {
    let script = format!(
        "$ProgressPreference='SilentlyContinue'; try {{ $r = Invoke-RestMethod -UseBasicParsing -TimeoutSec 15 \
         -Headers @{{'User-Agent'='Orchestrator/{VERSION}'}} https://api.github.com/repos/{REPO}/releases/latest; \
         $exe = $r.assets | Where-Object {{ $_.name -like 'Orchestrator-Setup-*.exe' }} | Select-Object -First 1; \
         $sums = $r.assets | Where-Object {{ $_.name -eq 'SHA256SUMS.txt' }} | Select-Object -First 1; \
         [pscustomobject]@{{tag=$r.tag_name; url=$r.html_url; name=$r.name; installer=$exe.browser_download_url; \
           installer_name=$exe.name; sums=$sums.browser_download_url}} | ConvertTo-Json -Compress }} catch {{ }}"
    );
    let v: Value = serde_json::from_str(&powershell(&script)?).ok()?;
    let tag = v.get("tag")?.as_str()?.to_string();
    if parse(&tag) <= parse(VERSION) {
        return None;
    }
    Some(json!({
        "version": tag.trim_start_matches('v'), "url": v.get("url"), "name": v.get("name"),
        "installer": v.get("installer"), "installer_name": v.get("installer_name"), "sums": v.get("sums"),
    }))
}

/// Download the release's installer, check it against SHA256SUMS.txt, and start
/// it in update mode (it waits for this app to exit, updates, and reopens it).
pub fn download_and_launch(update: &Value) -> Result<(), String> {
    let installer = update.get("installer").and_then(Value::as_str).filter(|u| u.starts_with("https://github.com/")).ok_or("this release has no installer to download")?;
    let sums = update.get("sums").and_then(Value::as_str).filter(|u| u.starts_with("https://github.com/")).ok_or("this release has no checksum file")?;
    let name = update.get("installer_name").and_then(Value::as_str).ok_or("missing installer name")?;
    if name.contains(['\'', '"', '/', '\\']) {
        return Err("unexpected installer name".into());
    }
    let script = format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         $dir = Join-Path $env:TEMP 'OrchestratorUpdate'; New-Item -ItemType Directory -Force $dir | Out-Null; \
         $exe = Join-Path $dir '{name}'; $h = @{{'User-Agent'='Orchestrator/{VERSION}'}}; \
         try {{ Invoke-WebRequest -UseBasicParsing -Headers $h -Uri '{installer}' -OutFile $exe; \
           $sums = (Invoke-WebRequest -UseBasicParsing -Headers $h -Uri '{sums}').Content; \
           if ($sums -is [byte[]]) {{ $sums = [Text.Encoding]::UTF8.GetString($sums) }}; \
           $want = ($sums -split \"`n\" | Where-Object {{ $_ -match '{name}' }} | Select-Object -First 1) -split '\\s+' | Select-Object -First 1; \
           $got = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLower(); \
           if (-not $want -or $want.ToLower() -ne $got) {{ Remove-Item $exe -Force; 'ERR checksum mismatch' }} else {{ 'OK ' + $exe }} \
         }} catch {{ 'ERR ' + $_.Exception.Message }}"
    );
    let out = powershell(&script).ok_or("couldn't run the download")?;
    let path = out.strip_prefix("OK ").ok_or_else(|| out.trim_start_matches("ERR ").to_string())?;
    let mut cmd = Command::new(path);
    cmd.arg("--update");
    cmd.spawn().map(|_| ()).map_err(|e| format!("couldn't start the installer: {e}"))
}
