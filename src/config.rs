//! Settings and app folders.
//!
//! Settings live in %APPDATA%\Orchestrator\settings.json (edited from the app's
//! Settings tab). Threads, transcripts and the WebView profile live in
//! %LOCALAPPDATA%\Orchestrator. ORCH_HOME overrides both, for testing.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const BACKENDS: [&str; 3] = ["plan", "api", "auto"];
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Claude Code permission modes offered as permission levels, most to least access.
pub const PERMISSIONS: [&str; 5] = ["bypassPermissions", "auto", "acceptEdits", "manual", "plan"];

pub fn check_permission(p: &str) -> Result<(), String> {
    if PERMISSIONS.contains(&p) {
        Ok(())
    } else {
        Err(format!("permission level must be one of {}", PERMISSIONS.join(", ")))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Anthropic API key, used only by the `api` backend (and `auto` fallbacks).
    pub api_key: String,
    /// For keys that aren't scoped to a workspace.
    pub api_workspace_id: String,
    pub orch_backend: String,
    pub thread_backend: String,
    /// Empty means "Claude Code's default".
    pub orch_model: String,
    pub thread_model: String,
    pub orch_effort: String,
    pub thread_effort: String,
    /// Empty means "find claude.exe on PATH or in ~/.local/bin".
    pub cli_path: String,
    /// "dark", "light" or "system".
    pub theme: String,
    /// How much `auto` work should go to API credits: 0 = all plan, 10 = all API.
    pub api_mix: u8,
    /// Highest effort the orchestrator may give a thread (you can still set more yourself).
    pub max_effort: String,
    /// The first-run setup wizard has been completed.
    pub setup_done: bool,
    /// Check GitHub for a newer release (at startup and twice a day).
    pub check_updates: bool,
    /// Permission level for projects that haven't set one.
    pub permission_default: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            api_key: String::new(),
            api_workspace_id: String::new(),
            orch_backend: "plan".into(),
            thread_backend: "auto".into(),
            orch_model: String::new(),
            thread_model: String::new(),
            orch_effort: String::new(),
            thread_effort: String::new(),
            cli_path: String::new(),
            theme: "dark".into(),
            api_mix: 0,
            max_effort: "high".into(),
            setup_done: false,
            check_updates: true,
            permission_default: "bypassPermissions".into(),
        }
    }
}

pub fn check_backend(b: &str) -> Result<(), String> {
    if BACKENDS.contains(&b) {
        Ok(())
    } else {
        Err(format!("backend must be one of {}", BACKENDS.join(", ")))
    }
}

pub fn check_effort(e: &str) -> Result<(), String> {
    if e.is_empty() || EFFORTS.contains(&e) {
        Ok(())
    } else {
        Err(format!("effort must be default or one of {}", EFFORTS.join(", ")))
    }
}

#[derive(Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Paths {
        let (config_dir, data_dir) = match std::env::var_os("ORCH_HOME") {
            Some(home) => (PathBuf::from(&home), PathBuf::from(&home)),
            None => {
                let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home_dir);
                let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home_dir);
                (roaming.join("Orchestrator"), local.join("Orchestrator"))
            }
        };
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = std::fs::create_dir_all(data_dir.join("transcripts"));
        Paths { config_dir, data_dir }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    pub fn default_workspace(&self) -> PathBuf {
        let ws = home_dir().join("Orchestrator Workspace");
        let _ = std::fs::create_dir_all(&ws);
        ws
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

impl Settings {
    pub fn load(paths: &Paths) -> Settings {
        let file = paths.settings_file();
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(s) = serde_json::from_str(&text) {
                return s;
            }
        }
        let s = Settings::default();
        let _ = s.save(paths);
        s
    }

    pub fn save(&self, paths: &Paths) -> std::io::Result<()> {
        let tmp = paths.settings_file().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap())?;
        std::fs::rename(tmp, paths.settings_file())
    }

    /// Path to the Claude Code CLI.
    pub fn claude_exe(&self) -> PathBuf {
        if !self.cli_path.trim().is_empty() {
            return PathBuf::from(self.cli_path.trim());
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let exe = dir.join("claude.exe");
                if exe.is_file() {
                    return exe;
                }
            }
        }
        let local = home_dir().join(".local").join("bin").join("claude.exe");
        if local.is_file() {
            return local;
        }
        PathBuf::from("claude")
    }
}

pub fn effort_rank(e: &str) -> usize {
    EFFORTS.iter().position(|x| *x == e).map(|i| i + 1).unwrap_or(0)
}
