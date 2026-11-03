//! First-run setup: check for Claude Code, sign in to a Claude plan, and/or add
//! an API key. The UI drives it through /api/setup/*.

use crate::config::Settings;
use serde_json::{json, Value};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
}

/// A command for the Claude Code CLI that uses only its own login (never the API key).
fn claude(settings: &Settings) -> Command {
    let mut cmd = Command::new(settings.claude_exe());
    for k in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_CUSTOM_HEADERS", "CLAUDECODE"] {
        cmd.env_remove(k);
    }
    cmd
}

/// Is Claude Code installed, and is it signed in to a Claude plan?
pub fn status(settings: &Settings) -> Value {
    let exe = settings.claude_exe();
    let version = quiet(&mut claude(settings).arg("--version"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let login = if version.is_some() {
        quiet(&mut claude(settings).args(["auth", "status"]))
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
    } else {
        None
    };
    let method = login.as_ref().and_then(|l| l.get("authMethod")).and_then(Value::as_str).unwrap_or("").to_string();
    json!({
        "setup_done": settings.setup_done,
        "cli": {"found": version.is_some(), "version": version, "path": exe.to_string_lossy()},
        "login": {
            "logged_in": login.as_ref().and_then(|l| l.get("loggedIn")).and_then(Value::as_bool).unwrap_or(false),
            "method": method,
            "plan": method == "claude.ai",
            "subscription": login.as_ref().and_then(|l| l.get("subscriptionType")).cloned(),
            "email": login.as_ref().and_then(|l| l.get("email")).cloned(),
        },
        "api_key_set": !settings.api_key.trim().is_empty(),
    })
}

/// Open a console window running Anthropic's official Claude Code installer.
pub fn install_cli() -> Result<(), String> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        "Write-Host 'Installing Claude Code (claude.ai/install.ps1)...'; irm https://claude.ai/install.ps1 | iex; \
         Write-Host ''; Write-Host 'Done. You can close this window and go back to Orchestrator.'; Read-Host 'Press Enter to close'",
    ]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NEW_CONSOLE);
    cmd.spawn().map(|_| ()).map_err(|e| format!("couldn't start the installer: {e}"))
}

/// Open a console window running Claude Code's sign-in (it opens the browser).
pub fn login(settings: &Settings) -> Result<(), String> {
    let exe = settings.claude_exe();
    let mut cmd = Command::new("cmd.exe");
    // Passed raw: cmd.exe parses its own command line, and Rust's quoting would break the `&` chain.
    #[cfg(windows)]
    cmd.raw_arg(format!(
        "/c title Sign in to Claude & \"{}\" auth login & echo. & echo When it says you're signed in, close this window and go back to Orchestrator. & pause",
        exe.display()
    ));
    for k in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDECODE"] {
        cmd.env_remove(k);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NEW_CONSOLE);
    cmd.spawn().map(|_| ()).map_err(|e| format!("couldn't start Claude Code ({}): {e}", exe.display()))
}
