//! `orchestrator.exe --uninstall`: what Windows' "Installed apps" runs.

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Orchestrator";

fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

fn ask(title: &str, text: &str) -> bool {
    MessageDialog::new()
        .set_title(title)
        .set_description(text)
        .set_level(MessageLevel::Warning)
        .set_buttons(MessageButtons::YesNo)
        .show()
        == MessageDialogResult::Yes
}

pub fn run() {
    if !ask("Uninstall Orchestrator", "Remove Orchestrator from this computer?\n\nAny running threads are stopped.") {
        return;
    }
    let delete_data = ask(
        "Uninstall Orchestrator",
        "Also delete your Orchestrator settings (including a saved API key), conversations and project notes?\n\n\
         Choose No to keep them for a later reinstall. Your projects' files are never touched.",
    );

    // Stop any other running copy (e.g. the one in the tray).
    let me = std::process::id();
    let _ = hidden(Command::new("taskkill").args(["/F", "/IM", "orchestrator.exe", "/FI", &format!("PID ne {me}")])).status();

    let _ = hidden(Command::new("powershell.exe").args([
        "-NoProfile",
        "-Command",
        "foreach ($d in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) { \
           Remove-Item -LiteralPath (Join-Path $d 'Orchestrator.lnk') -Force -ErrorAction SilentlyContinue }",
    ]))
    .status();
    let _ = hidden(Command::new("reg").args(["delete", UNINSTALL_KEY, "/f"])).status();

    if delete_data {
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let dir = std::path::Path::new(&base).join("Orchestrator");
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    // The program folder can only go once this process has exited.
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let mut cmd = Command::new("cmd.exe");
        #[cfg(windows)]
        cmd.raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()));
        let _ = hidden(&mut cmd).spawn();
    }
    MessageDialog::new()
        .set_title("Uninstall Orchestrator")
        .set_description("Orchestrator has been removed.")
        .set_buttons(MessageButtons::Ok)
        .show();
}
