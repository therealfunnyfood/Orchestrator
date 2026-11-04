# Orchestrator

**A desktop app for running a team of [Claude Code](https://claude.com/claude-code)
agents on one project.**

You give the work to the **orchestrator**, a lead agent. It breaks the work into
tasks and hands each one to a **thread**. A thread is a full Claude Code session
working in your project folder, and several threads run at once. The orchestrator
talks with its threads while they work: it answers their questions, redirects them,
passes results from one to another, and checks what they did before it reports back
to you. It works like a Claude Project for Claude Code, on your own Windows PC.

### Why use it

- **Parallel work with a lead.** Big jobs get split up and run at the same time,
  and one agent keeps track of all of it and reviews the results.
- **Everything happens live.** Watch every thread stream in real time and jump into
  any of them. Messages you send while an agent is working go straight into its
  current turn, the same as in Claude desktop.
- **Threads and the orchestrator keep talking.** Threads report milestones, ask
  questions and flag blockers. When a thread finishes, its report goes straight to
  the orchestrator.
- **Your plan, your API key, or both.** A slider per project sets how much runs on
  your Claude Pro or Max plan and how much on API credits. When the plan hits its
  usage limit, turns can switch to the API.
- **Timelines that learn.** The orchestrator says how long work will take, and the
  app checks every estimate against the real time so the next ones are more realistic.
- **The right model for each task.** The orchestrator picks Haiku, Sonnet or Opus
  and an effort level for each thread. It learns which choices worked from each
  thread's time and cost.
- **You decide what agents may do.** Choose a permission level per project, from
  full access to read-only. Approval requests show up as cards in the chat.
- **It's your real Claude Code.** Threads use your own Claude Code login, settings,
  `CLAUDE.md` files, MCP servers and permission rules. `/import` brings in an
  existing Claude Code project with its history.
- **Local and private.** No telemetry and no account with this app. Your work goes
  only to Anthropic, through Claude Code.

At *Full access*, threads can run commands and edit files in the workspace without
asking. Only use that level on folders you're comfortable with.

## Requirements

- Windows 10 or 11 (64-bit) with the Microsoft Edge WebView2 Runtime (built into
  Windows 11).
- [Claude Code](https://claude.com/claude-code). The first-run setup installs it if
  it's missing.
- A Claude Pro or Max plan, an Anthropic API key, or both.

## Install

1. Download `Orchestrator-Setup-<version>.exe` from [Releases](../../releases).
2. Run it. It installs for your Windows account (no admin rights) and adds Start
   menu and desktop shortcuts.
3. On first launch, a short setup asks how you want to pay for Claude:
   - **Your Claude plan** (Pro or Max): it checks for Claude Code, installs it if
     needed, and signs you in through your browser.
   - **An API key** from [console.anthropic.com](https://console.anthropic.com/settings/keys).
   - **Both**, with a slider for how much runs on each.

The installer is unsigned, so Windows SmartScreen may warn you. Choose
*More info → Run anyway*. It needs the Microsoft Edge WebView2 Runtime, which is
built into Windows 11; the installer links to it if it's missing.

To update, click **⬆ update** in the status bar when a new version is out. It
downloads the installer, checks it against the release's checksum, and restarts into
the new version. You can also run a newer installer by hand.

To uninstall, use Settings → Apps → Installed apps → Orchestrator.

## Using it

- **Orchestrator** (Ctrl+1): describe the work. It plans, starts threads, waits for
  them, checks their results and reports back with links to each thread.
- **Threads**: every thread shows live status in the sidebar. Click a thread to
  open it, click a link to open it beside the chat, or pop it out into its own
  window (⧉). Each thread has a task list, and threads can ask you multiple-choice
  questions and show you images, video, audio or PDFs.
- **Files**: click a file path in any message to show it in Explorer (Ctrl+click
  opens it in your editor). Markdown files open rendered in a tab inside the app.
- **Composer**: messages stream in as they're written. A message you send while a
  turn runs goes straight into it (Claude reads it at its next step, like in Claude
  desktop); Alt+Enter queues it for after the turn instead. You can edit or retry
  earlier messages (the conversation branches from that point), and paste or drop
  images and files.
- **Resolve**: mark a thread you're done with as resolved (✓ resolve in its header,
  or right-click it). The orchestrator resolves threads when their work is finished.
  Resolved threads sit in a collapsed group at the bottom of the sidebar and reopen
  when they get new work.
- **Handoff**: when a conversation gets long, click **⇄ handoff** in any chat's header
  (or type `/handoff`). The agent writes a handoff and the chat continues in a fresh
  session that starts from it.
- **Workspace**: the project folder it works in. Each workspace has its own
  orchestrator conversation, threads, brief and rules. `/import` brings in a Claude
  Code project, including its history, your rules and its unfinished work.
- **Tray**: closing the window keeps everything running in the system tray.
  Right-click the tray icon → Quit to stop.

Type `/` for commands, or press Ctrl+Shift+P for the command palette.

### Permission levels

Set per project (🛡 in the status bar, `/permissions`, or Settings). Each thread can
override it from the Threads tab.

| Level | What threads can do without asking |
|---|---|
| Full access | everything |
| Auto | routine actions; Claude Code's safety check asks you about risky ones |
| Edits OK, ask for the rest | edit files; commands and other actions need your OK |
| Ask for everything | read; anything that changes something asks first |
| Plan only | read and plan; nothing can be changed |

An approval shows up as a card in the thread's chat with the tool and its input, and
the sidebar marks the thread with **?**. You can choose **Allow**, **Always allow**
(that tool, in that thread) or **Deny**, and add a note telling the agent what to do
instead. Your own Claude Code permission rules (`allow` and `deny` in
`~/.claude/settings.json` and project settings) still apply on top of the level. For
example, if you've allowed `Bash`, shell commands won't ask.

### Usage and cost

- **Plan/API mix**: a 0–10 slider per project (⚖ in the status bar). 0 runs
  everything on your Claude plan and 10 runs everything on API credits. It applies
  to every thread and the orchestrator.
- **Plan usage**: the status bar shows how much of your plan's 5-hour window is
  used. Click it to see the 5-hour and weekly limits and when they reset.
- **Models and effort**: the orchestrator uses Haiku for simple work, Sonnet for
  routine coding and Opus for hard problems. It uses medium effort by default and
  high only when needed, and it asks you before using xhigh or max.
- **Timelines**: the orchestrator tells you how long work should take and gives each
  thread task an estimate. The app times the real work and shows the orchestrator how
  its estimates compare by kind of work, so its timelines get more realistic over time.
  Running work shows how long is left; see Threads → Timelines for the history.
- **Self-tuning**: each thread turn records its model, effort, time and cost. The
  orchestrator rates the fit and keeps tuning notes, which you can see under
  Threads → Efficiency and on the 📋 brief page.

## Build from source

Requires Rust (MSVC toolchain) and Windows 10/11.

```powershell
.\install.ps1     # build and install for yourself (Start menu + desktop shortcut)
.\release.ps1     # build dist\Orchestrator-Setup-<version>.exe
```

`orchestrator.exe --serve --port 8000` runs only the local server, for use in a browser.

### Layout

- `src/runner.rs`: runs a turn by driving `claude -p --output-format stream-json`,
  with plan or API auth per turn.
- `src/app.rs`: threads, message queues, the per-workspace orchestrator, and the
  plan/API mix.
- `src/mcp.rs`: the tools each session gets over MCP: task list, questions, media,
  plus thread management, past sessions and efficiency for the orchestrator.
- `src/claude_projects.rs`: reads Claude Code's saved conversations for `/import`.
- `src/web.rs`: a local server on 127.0.0.1 with a random per-launch token.
- `src/main.rs`: windows, tray icon, single instance.
- `src/setup.rs`, `ui/`: the first-run setup and the interface, compiled into the exe.
- `installer/`: the installer, which embeds the release build.

Settings are stored in `%APPDATA%\Orchestrator`, and threads and transcripts in
`%LOCALAPPDATA%\Orchestrator`.

## Privacy

Orchestrator has no telemetry. Everything stays on your computer except:

- the work itself, which Claude Code sends to Anthropic under your plan or API key;
- an update check that asks GitHub for the latest release, and the installer
  download when you choose to update. You can turn the check off in Settings → About.

The local server listens only on 127.0.0.1 and requires a random token that changes
every launch. Your API key is stored in `%APPDATA%\Orchestrator\settings.json`.

## Troubleshooting

- **SmartScreen warning when installing**: the installer isn't code-signed. Choose
  *More info → Run anyway*.
- **The plan shows an API key instead of your subscription**: Claude Code is signed in
  with a Console key. Use Settings → Run setup again → *My Claude plan* → Sign in, and
  choose the account that has your plan.
- **"must include the anthropic-workspace-id header"**: your API key isn't scoped to a
  workspace. Add the workspace ID (from the same organization as the key) in Settings.
- **Your threads are missing after reinstalling**: if you first started Orchestrator
  from inside another app, such as a Claude Code session in the Claude desktop app,
  Windows saved its data to that app's private copy of AppData. Version 0.3.1 and
  later move that data back automatically on startup. To avoid it, start Orchestrator
  from the Start menu.
- **The window disappeared**: it's in the system tray. Click the icon, or launch
  Orchestrator again.
- **A turn was cut off** (closed or crashed): the conversation shows "interrupted".
  Click ↻ continue.

## Notes

- The `$` figures are estimates of what the work would cost at API rates, covering
  both plan and API turns. Only API turns use credits; check your real balance in
  the [Anthropic Console](https://console.anthropic.com).
- Anthropic's terms limit how a subscription login may be used by automated
  tools. This app drives your own Claude Code login on your own machine; check the
  current terms if you run it heavily.
- Orchestrator is an independent project. It isn't made or endorsed by Anthropic.

## License

[MIT](LICENSE) © 2026 therealfunnyfood
