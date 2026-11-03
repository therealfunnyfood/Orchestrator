# Changelog

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
