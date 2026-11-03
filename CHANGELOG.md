# Changelog

## 0.4.0 (2026-11-02)

- **Permission levels**: Full access, Auto, Edits OK (ask for the rest), Ask for
  everything, or Plan only (read-only), set per project with an optional override per
  thread. When a thread needs approval, a card shows the exact tool and input with
  Allow, Always allow (for that thread) or Deny with a reason. Your Claude Code
  allow and deny rules still apply. The first-run setup now asks which level to use.
- **One-click updates**: when a new release is out, ⬆ update in the status bar
  downloads it, checks its SHA-256 against the release's checksum file, closes the app,
  updates it and reopens it.
- Long waits (approvals, questions, `wait_for_threads`) no longer time out after five
  minutes: tool calls now stream progress while they wait.

## 0.3.1 (2026-11-02)

- If Orchestrator was first run from inside a packaged app such as the Claude
  desktop app, Windows redirected its data to that app's private copy of AppData,
  so a normal launch started empty. On startup it now finds that data and moves it
  to the real folders, backing up what was there first.

## 0.3.0 (2026-11-02)

First public release.

- **Orchestrator and threads**: an orchestrator agent plans the work and runs Claude Code
  threads in parallel, checks their results and links to them in its replies.
- **Live view**: streaming transcripts, a sidebar with each thread's status, tabs, a side
  panel and pop-out windows, a task list for each thread, multiple-choice questions, and
  inline images, video, audio and PDFs.
- **Composer**: a message queue (send now, edit, remove), edit and retry earlier
  messages (forks the Claude Code session), image and file attachments, slash commands
  with autocomplete, and a command palette.
- **Projects**: one workspace per project folder, `/import` for Claude Code projects
  (history, your rules and unfinished work), and a project brief and rules shown to every
  thread.
- **Usage**: a plan/API mix slider for each project, plan usage limits in the status bar,
  model and effort chosen for each thread (asks before using xhigh or max), and
  efficiency stats with the orchestrator's tuning notes.
- **Desktop**: system tray, single instance, an "interrupted" notice with ↻ continue
  after a crash, an update check, and VS Code-style themes.
- **Setup**: an installer (per user, no admin rights), a first-run setup to sign in to a
  Claude plan and/or add an API key, and uninstall from Installed apps.
