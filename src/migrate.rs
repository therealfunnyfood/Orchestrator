//! One-time rescue of data written from inside a packaged (MSIX) app.
//!
//! Programs started from a packaged app such as the Claude desktop app see a
//! redirected AppData: their writes land in
//! %LOCALAPPDATA%\Packages\<package>\LocalCache\{Local,Roaming}\... instead of
//! the real folders. If Orchestrator was first run that way, its threads and
//! settings are stranded there and a normal launch starts empty. On startup we
//! look for such a copy and, if it's newer than ours, move it over (keeping a
//! backup of what was here).

use crate::config::Paths;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.len() == y.len() && x.modified().ok() == y.modified().ok(),
        _ => false,
    }
}

/// Copy `src` into `dst` recursively, overwriting only older or missing files.
fn merge_dir(src: &Path, dst: &Path) {
    let Ok(entries) = std::fs::read_dir(src) else { return };
    let _ = std::fs::create_dir_all(dst);
    for e in entries.filter_map(Result::ok) {
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if s.is_dir() {
            merge_dir(&s, &d);
        } else if mtime(&d).is_none_or(|dt| mtime(&s).is_some_and(|st| st > dt)) {
            let _ = std::fs::copy(&s, &d);
        }
    }
}

fn backup(file: &Path, dir: &Path) {
    if file.exists() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::copy(file, dir.join(file.file_name().unwrap()));
    }
}

/// When the work in a state.json last changed: its newest thread update (0 if none).
fn activity(state: &Path) -> f64 {
    std::fs::read_to_string(state)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("threads").and_then(|t| t.as_array()).cloned())
        .map(|threads| threads.iter().filter_map(|t| t.get("updated_at").and_then(|u| u.as_f64())).fold(0.0, f64::max))
        .unwrap_or(0.0)
}

/// Returns a description of what was moved, if anything.
pub fn rescue_packaged_data(paths: &Paths) -> Option<String> {
    if std::env::var_os("ORCH_HOME").is_some() {
        return None;
    }
    let marker = paths.data_dir.join(".packaged-data-checked");
    if marker.exists() {
        return None;
    }
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    // (data folder, settings folder, when its state was written) of the newest packaged copy
    let mut found: Option<(PathBuf, PathBuf, SystemTime)> = None;
    for pkg in std::fs::read_dir(local.join("Packages")).ok()?.filter_map(Result::ok) {
        let cache = pkg.path().join("LocalCache");
        let state = cache.join("Local").join("Orchestrator").join("state.json");
        if let Some(t) = mtime(&state) {
            if found.as_ref().is_none_or(|(_, _, best)| t > *best) {
                found = Some((cache.join("Local").join("Orchestrator"), cache.join("Roaming").join("Orchestrator"), t));
            }
        }
    }
    let _ = std::fs::create_dir_all(&paths.data_dir);
    let _ = std::fs::write(&marker, b"");
    let (src_data, src_config, _) = found?;

    let state = paths.data_dir.join("state.json");
    // Running inside the package ourselves (same view of the files), or ours has the
    // more recent work: nothing to do. (File times don't work here: every launch
    // re-saves state.json, even an empty one.)
    if same_file(&src_data.join("state.json"), &state) || activity(&state) >= activity(&src_data.join("state.json")) {
        return None;
    }

    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let backup_dir = paths.data_dir.join(format!("backup-before-rescue-{stamp}"));
    backup(&state, &backup_dir);
    backup(&paths.settings_file(), &backup_dir);

    let _ = std::fs::copy(src_data.join("state.json"), &state);
    for sub in ["transcripts", "media", "uploads"] {
        merge_dir(&src_data.join(sub), &paths.data_dir.join(sub));
    }
    let src_settings = src_config.join("settings.json");
    if mtime(&src_settings).is_some_and(|t| mtime(&paths.settings_file()).is_none_or(|ours| t > ours)) {
        let _ = std::fs::create_dir_all(&paths.config_dir);
        let _ = std::fs::copy(&src_settings, paths.settings_file());
    }
    Some(format!("moved Orchestrator data from {} (previous data backed up to {})", src_data.display(), backup_dir.display()))
}
