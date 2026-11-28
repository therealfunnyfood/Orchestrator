//! Notifications for business workspaces: Windows toasts and, optionally, a push to the
//! user's phone (ntfy.sh or a Telegram bot, with the topic or token kept in the vault).
//! Only a one-line summary is ever sent, redacted, never a payload. Text and secrets reach
//! PowerShell as environment variables, never on its command line.

use std::process::{Command, Stdio};

/// (id, label) of the events the user can choose to be notified about.
pub const EVENTS: [(&str, &str); 5] = [
    ("urgent", "Urgent queue items"),
    ("schedule", "Finished scheduled runs"),
    ("budget", "Budget alerts"),
    ("deadline", "Orders at risk of missing their deadline"),
    ("usage", "Usage gates"),
];

/// Phone channels and the vault secrets each one needs.
pub fn phone_secrets(channel: &str) -> &'static [&'static str] {
    match channel {
        "ntfy" => &["NTFY_TOPIC"],
        "telegram" => &["TELEGRAM_BOT_TOKEN", "TELEGRAM_CHAT_ID"],
        _ => &[],
    }
}

/// One line, redacted and short: what a notification may carry.
pub fn summary_line(title: &str, body: &str) -> (String, String) {
    let clean = |s: &str, max: usize| -> String {
        let one = crate::vault::redact(s).split_whitespace().collect::<Vec<_>>().join(" ");
        if one.chars().count() > max { one.chars().take(max - 1).collect::<String>() + "…" } else { one }
    };
    (clean(title, 80), clean(body, 200))
}

fn powershell(script: &str, env: &[(&str, String)]) {
    // Tests never pop real toasts or push to a phone.
    if cfg!(test) {
        return;
    }
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let _ = c.spawn();
}

const TOAST: &str = r#"
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
$x = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$t = $x.GetElementsByTagName('text')
$t.Item(0).AppendChild($x.CreateTextNode($env:ORCH_TITLE)) > $null
$t.Item(1).AppendChild($x.CreateTextNode($env:ORCH_BODY)) > $null
$n = New-Object Windows.UI.Notifications.ToastNotification $x
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe').Show($n)
"#;

/// A Windows toast.
pub fn toast(title: &str, body: &str) {
    let (t, b) = summary_line(title, body);
    powershell(TOAST, &[("ORCH_TITLE", format!("Orchestrator: {t}")), ("ORCH_BODY", b)]);
}

/// A push to the user's phone through `channel`, with its secrets from the vault.
pub fn push(channel: &str, secrets: &[(String, String)], title: &str, body: &str) {
    let (t, b) = summary_line(title, body);
    let get = |k: &str| secrets.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    match channel {
        "ntfy" => powershell(
            "Invoke-RestMethod -Method Post -Uri ('https://ntfy.sh/' + $env:ORCH_TOPIC) -Headers @{ Title = $env:ORCH_TITLE } -Body ([Text.Encoding]::UTF8.GetBytes($env:ORCH_BODY)) | Out-Null",
            &[("ORCH_TOPIC", get("NTFY_TOPIC")), ("ORCH_TITLE", format!("Orchestrator: {t}")), ("ORCH_BODY", b)],
        ),
        "telegram" => powershell(
            "Invoke-RestMethod -Method Post -Uri ('https://api.telegram.org/bot' + $env:ORCH_TOKEN + '/sendMessage') -ContentType 'application/json; charset=utf-8' -Body ([Text.Encoding]::UTF8.GetBytes((@{ chat_id = $env:ORCH_CHAT; text = ($env:ORCH_TITLE + [char]10 + $env:ORCH_BODY) } | ConvertTo-Json))) | Out-Null",
            &[("ORCH_TOKEN", get("TELEGRAM_BOT_TOKEN")), ("ORCH_CHAT", get("TELEGRAM_CHAT_ID")), ("ORCH_TITLE", format!("Orchestrator: {t}")), ("ORCH_BODY", b)],
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_carry_one_short_line() {
        let long = "word ".repeat(100);
        let (t, b) = summary_line("Urgent:\nnew   item", &format!("line one\nline two {long}"));
        assert_eq!(t, "Urgent: new item");
        assert!(!b.contains('\n'));
        assert!(b.chars().count() <= 200);
        assert!(b.ends_with('…'));
    }

    #[test]
    fn phone_channels_name_their_vault_secrets() {
        assert_eq!(phone_secrets("ntfy"), &["NTFY_TOPIC"]);
        assert_eq!(phone_secrets("telegram"), &["TELEGRAM_BOT_TOKEN", "TELEGRAM_CHAT_ID"]);
        assert!(phone_secrets("").is_empty());
    }
}
