//! `orchestrator.exe --uninstall`: what Windows' "Installed apps" runs. It only
//! removes this copy: its own folder, shortcuts that point at it, and the
//! "Installed apps" entry if it describes this folder.
//!
//!   --yes          don't ask (keeps your data unless --delete-data)
//!   --delete-data  also delete settings, conversations and project notes

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::path::Path;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Orchestrator";

fn hidden(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

fn ask(text: &str) -> bool {
    MessageDialog::new()
        .set_title("Uninstall Orchestrator")
        .set_description(text)
        .set_level(MessageLevel::Warning)
        .set_buttons(MessageButtons::YesNo)
        .show()
        == MessageDialogResult::Yes
}

/// Quote a path for a single-quoted PowerShell string.
fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

pub fn run(args: &[String]) {
    let yes = args.iter().any(|a| a == "--yes");
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent().map(Path::to_path_buf) else { return };

    if !yes && !ask("Remove Orchestrator from this computer?\n\nAny running threads are stopped.") {
        return;
    }
    let delete_data = args.iter().any(|a| a == "--delete-data")
        || (!yes
            && ask(
                "Also delete your Orchestrator settings (including a saved API key), conversations and project notes?\n\n\
                 Choose No to keep them for a later reinstall. Your projects' files are never touched.",
            ));

    // Stop other copies running from this folder (e.g. the one in the tray), and
    // remove shortcuts that point at this exe.
    let script = format!(
        "$exe = {exe}; Get-Process orchestrator -ErrorAction SilentlyContinue | \
           Where-Object {{ $_.Path -eq $exe -and $_.Id -ne {me} }} | Stop-Process -Force; \
         $shell = New-Object -ComObject WScript.Shell; \
         foreach ($d in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {{ \
           $p = Join-Path $d 'Orchestrator.lnk'; \
           if ((Test-Path -LiteralPath $p) -and ($shell.CreateShortcut($p).TargetPath -eq $exe)) {{ Remove-Item -LiteralPath $p -Force }} }}",
        exe = ps_quote(&exe),
        me = std::process::id()
    );
    let _ = hidden("powershell.exe").args(["-NoProfile", "-Command", &script]).status();

    // The "Installed apps" entry, if it's ours.
    let ours = hidden("reg")
        .args(["query", UNINSTALL_KEY, "/v", "InstallLocation"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .is_some_and(|out| out.contains(&dir.display().to_string().to_lowercase()));
    if ours {
        let _ = hidden("reg").args(["delete", UNINSTALL_KEY, "/f"]).status();
    }

    if delete_data {
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let _ = std::fs::remove_dir_all(Path::new(&base).join("Orchestrator"));
            }
        }
    }

    // The program folder can only go once this process has exited.
    let mut cmd = hidden("cmd.exe");
    #[cfg(windows)]
    cmd.raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()));
    let _ = cmd.spawn();

    if !yes {
        MessageDialog::new()
            .set_title("Uninstall Orchestrator")
            .set_description("Orchestrator has been removed.")
            .set_buttons(MessageButtons::Ok)
            .show();
    }
}
