//! Orchestrator installer: installs for the current user (no admin rights), adds
//! Start menu and desktop shortcuts and an "Installed apps" entry, then opens the
//! app, whose first-run setup handles signing in and API keys.

#![windows_subsystem = "windows"]

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP: &[u8] = include_bytes!("../../target/release/orchestrator.exe");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Orchestrator";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hidden(program: &str) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

fn dialog(text: &str, level: MessageLevel, buttons: MessageButtons) -> MessageDialogResult {
    MessageDialog::new().set_title(&format!("Orchestrator {VERSION} setup")).set_description(text).set_level(level).set_buttons(buttons).show()
}

fn fail(text: &str) -> ! {
    dialog(text, MessageLevel::Error, MessageButtons::Ok);
    std::process::exit(1);
}

fn install_dir() -> PathBuf {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    local.join("Programs").join("Orchestrator")
}

/// The Evergreen WebView2 runtime (the window's renderer) is registered under one of these.
fn has_webview2() -> bool {
    const ID: &str = r"{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    [
        format!(r"HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{ID}"),
        format!(r"HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients\{ID}"),
        format!(r"HKCU\Software\Microsoft\EdgeUpdate\Clients\{ID}"),
    ]
    .iter()
    .any(|k| hidden("reg").args(["query", k, "/v", "pv"]).output().is_ok_and(|o| o.status.success()))
}

fn running_installed(exe: &Path) -> bool {
    let filter = format!("$p = Get-Process orchestrator -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -eq '{}' }}; if ($p) {{ 'yes' }}", exe.display());
    hidden("powershell.exe").args(["-NoProfile", "-Command", &filter]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("yes"))
}

fn stop_installed(exe: &Path) {
    let cmd = format!("Get-Process orchestrator -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -eq '{}' }} | Stop-Process -Force", exe.display());
    let _ = hidden("powershell.exe").args(["-NoProfile", "-Command", &cmd]).status();
    std::thread::sleep(std::time::Duration::from_millis(800));
}

fn shortcuts(exe: &Path) -> bool {
    // Recreated rather than overwritten: Start doesn't notice a shortcut rewritten in place.
    let script = format!(
        "$exe = '{exe}'; $shell = New-Object -ComObject WScript.Shell; \
         foreach ($d in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {{ \
           $p = Join-Path $d 'Orchestrator.lnk'; \
           if (Test-Path -LiteralPath $p) {{ Remove-Item -LiteralPath $p -Force; Start-Sleep -Milliseconds 600 }}; \
           $l = $shell.CreateShortcut($p); $l.TargetPath = $exe; $l.WorkingDirectory = Split-Path $exe; \
           $l.IconLocation = \"$exe,0\"; $l.Description = 'Orchestrator: parallel Claude Code threads'; $l.Save() }}",
        exe = exe.display()
    );
    hidden("powershell.exe").args(["-NoProfile", "-Command", &script]).status().is_ok_and(|s| s.success())
}

fn register(dir: &Path, exe: &Path) {
    let size_kb = (APP.len() / 1024).to_string();
    let uninstall = format!("\"{}\" --uninstall", exe.display());
    let icon = format!("{},0", exe.display());
    let location = dir.display().to_string();
    let values: [(&str, &str, &str); 9] = [
        ("DisplayName", "REG_SZ", "Orchestrator"),
        ("DisplayVersion", "REG_SZ", VERSION),
        ("Publisher", "REG_SZ", "Orchestrator"),
        ("DisplayIcon", "REG_SZ", &icon),
        ("InstallLocation", "REG_SZ", &location),
        ("UninstallString", "REG_SZ", &uninstall),
        ("EstimatedSize", "REG_DWORD", &size_kb),
        ("NoModify", "REG_DWORD", "1"),
        ("NoRepair", "REG_DWORD", "1"),
    ];
    for (name, kind, data) in values {
        let _ = hidden("reg").args(["add", UNINSTALL_KEY, "/v", name, "/t", kind, "/d", data, "/f"]).status();
    }
}

fn main() {
    // --silent: no dialogs and don't launch (scripted installs, testing).
    // --dir <path>: install somewhere else; --no-shortcuts / --no-register skip those steps.
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let silent = flag("--silent");
    let dir = args.iter().position(|a| a == "--dir").and_then(|i| args.get(i + 1)).map(PathBuf::from).unwrap_or_else(install_dir);
    let exe = dir.join("orchestrator.exe");
    let upgrading = exe.exists();

    let intro = format!(
        "{} Orchestrator {VERSION} for your Windows account?\n\n\
         • Installs to {}\n• Adds Start menu and desktop shortcuts\n• No admin rights needed\n\n\
         When it opens, a short setup lets you sign in to your Claude plan and/or add an Anthropic API key.",
        if upgrading { "Update to" } else { "Install" },
        dir.display()
    );
    if !silent {
        let go = dialog(&intro, MessageLevel::Info, MessageButtons::OkCancelCustom(if upgrading { "Update".into() } else { "Install".into() }, "Cancel".into()));
        let accepted = go == MessageDialogResult::Ok || matches!(go, MessageDialogResult::Custom(ref s) if s == "Update" || s == "Install");
        if !accepted {
            return;
        }
    }

    if !has_webview2() {
        if silent {
            std::process::exit(2);
        }
        let r = dialog(
            "Orchestrator needs the Microsoft Edge WebView2 Runtime, which isn't installed.\n\n\
             Windows 11 includes it. Choose OK to open Microsoft's download page, install it, then run this setup again.",
            MessageLevel::Warning,
            MessageButtons::OkCancel,
        );
        if r == MessageDialogResult::Ok {
            let _ = hidden("cmd").args(["/c", "start", "", "https://developer.microsoft.com/microsoft-edge/webview2/"]).status();
        }
        return;
    }

    if running_installed(&exe) {
        if silent {
            std::process::exit(3); // never stop a running copy without asking
        }
        let r = dialog(
            "Orchestrator is running (it may be in the system tray). Close it to continue?\n\n\
             Running threads stop; you can continue them after the update.",
            MessageLevel::Warning,
            MessageButtons::OkCancel,
        );
        if r != MessageDialogResult::Ok {
            return;
        }
        stop_installed(&exe);
    }

    if let Err(e) = std::fs::create_dir_all(&dir) {
        fail(&format!("Couldn't create {}:\n{e}", dir.display()));
    }
    let tmp = dir.join("orchestrator.exe.new");
    if let Err(e) = std::fs::write(&tmp, APP).and_then(|_| std::fs::rename(&tmp, &exe)) {
        let _ = std::fs::remove_file(&tmp);
        fail(&format!("Couldn't write {}:\n{e}", exe.display()));
    }
    if !flag("--no-shortcuts") && !shortcuts(&exe) && !silent {
        dialog(
            &format!("Installed, but the shortcuts couldn't be created. You can start Orchestrator from\n{}", exe.display()),
            MessageLevel::Warning,
            MessageButtons::Ok,
        );
    }
    if !flag("--no-register") {
        register(&dir, &exe);
    }
    if silent {
        return;
    }

    let open = dialog(
        &format!("Orchestrator {VERSION} is installed.\n\nOpen it now?"),
        MessageLevel::Info,
        MessageButtons::YesNo,
    );
    if open == MessageDialogResult::Yes {
        let _ = Command::new(&exe).current_dir(&dir).spawn();
    }
}
