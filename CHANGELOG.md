# Changelog

## 0.4.13 (2026-11-07)

- **Work continues by itself after a usage limit.** When the orchestrator or a thread is
  stopped by your plan's usage limit, it now picks up where it left off a minute after the
  limit resets. Its chat shows when that will be. Sending it a message or pressing stop
  first cancels the automatic restart. Restarting the app also cancels it.

## 0.4.12 (2026-11-07)

- **The orchestrator now follows up when a thread reports back.** Before, if it said
  "wait for my message" and a thread then finished the step, it could read the report and
  go straight back to waiting, so you never heard. Now any thread report means it has to
  write to you before it can wait again.
- If it still won't pass something on, the app shows you the thread's report itself in
  the orchestrator's chat.

## 0.4.11 (2026-11-07)

- **Threads can flag things you need to do, and the orchestrator has to pass them on.**
  Before, a thread could report "you can start the test now" and the orchestrator would
  read it, say nothing, and keep waiting, so you never heard. Threads now mark these
  messages as for you, and the orchestrator can't wait on threads again until it has told
  you what to do.
- **Regular status during long work.** If the orchestrator hasn't written anything you
  can see for 20 minutes, its next wait is refused until it gives you a short status:
  what's done, what's running, and anything you need to do or that it said it would tell
  you.
- Whenever thread reports arrive, the orchestrator is reminded that you can't see them,
  and that anything you need to know has to go to you as a message.
- Builds no longer contain local folder paths.

## 0.4.10 (2026-11-05)

- **The orchestrator has to reply before it waits again.** In 0.4.9 your mid-turn
  message reached it, but it could still go straight back to waiting without saying
  anything. Now, after you message it, `wait_for_threads` refuses to wait until it has
  written you a reply.

## 0.4.9 (2026-11-05)

- **The orchestrator answers messages you send while it works.** A message sent while
  it was waiting on threads used to end the wait, but the orchestrator often went
  straight back to waiting without reading it or replying. Your message is now handed to
  it in the wait's result, with an instruction to reply to you before doing anything
  else, and its instructions now say to always answer you before carrying on.
- A message sent just as a wait ended is no longer lost: it runs as the next turn.

## 0.4.8 (2026-11-05)

- **Resolve threads you're done with.** Mark a thread resolved with ✓ resolve in its
  header, Resolve in its right-click menu, or `/resolve`. The orchestrator resolves
  threads too, with a short note, once their work is finished and checked or handed to
  another thread.
- Resolved threads move to a **resolved** group at the bottom of the sidebar that stays
  collapsed until you open it, so finished work doesn't take up space.
- Sending a resolved thread new work reopens it. ↺ reopen or Reopen does it by hand.
- Resolved threads are left out of the orchestrator's `wait_for_threads` and
  `list_threads`, which list them by name only.

## 0.4.7 (2026-11-04)

- **Realistic timelines that improve with use.** The orchestrator now tells you how long
  a request should take, as a range, and gives each thread task its own estimate. The
  app times the real work: a thread task until its turn ends, a whole request until the
  orchestrator and all its threads are done. Every turn, the orchestrator sees how long
  each kind of work (bugfix, feature, tests and so on) has really taken in this project
  and how far off its past estimates were, and adjusts. If its bugfixes keep running
  twice as long as it said, its next bugfix estimate is about twice as long.
- Running work shows its estimate: time in and about how long is left in the working
  line, the chat header and the sidebar, marked when it's running over. A finished
  request posts "finished in 12m (estimated 10–20m)".
- Threads → Timelines shows typical and 80th-percentile times by kind of work, and how
  the estimates compared with what really happened. The orchestrator can read the same
  numbers with `timing_report`.

## 0.4.6 (2026-11-03)

- **Handoff to a new session**: every chat, the orchestrator and each thread, has a
  ⇄ handoff button in its header (or type `/handoff`). The agent writes a handoff
  covering the goal, what's done, what's next, decisions, key files and your
  preferences. Then the chat carries on in a fresh Claude Code session that starts
  from that handoff, so a long conversation gets a clean context without losing the
  thread. If the agent is working, the handoff waits until its turn ends.
- The handoff turn can only read files, so writing the handoff never changes anything.
- Messages from before a handoff can't be edited or retried, since that session is gone.

## 0.4.5 (2026-11-03)

- **File paths in messages are clickable.** Paths in links and in `code` (for example
  `src/app.rs:42` or `C:\Projects\notes.txt`) open in Explorer, with the file
  selected. Ctrl+click opens the file in your editor at that line instead. Relative
  paths are taken from the thread's folder.
- **Markdown files open in a viewer inside the app.** Clicking a `.md` file shows it
  rendered in its own tab, with headings, tables, lists, images and links. Links in
  the document work too. Use ↻ Reload after the file changes, or Edit to open it in
  your editor. Shift+click a `.md` link to show it in Explorer instead.

## 0.4.4 (2026-11-02)

- **Fixed: costs were counted far too high.** Claude Code reports a session's cost as a
  running total that includes every earlier turn, and the app added that total up again
  on every turn. Thread, orchestrator and plan/API mix numbers are now each turn's own
  cost, so they match Claude Code. Because the old numbers were inflated, saved cost
  history (cost totals, the 24h mix ledger and the efficiency table) restarts from zero
  on this update. Your project notes and tuning notes are kept.
- When a plan turn hits a usage limit and falls back to the API, the plan part is now
  counted as plan.
- Cost labels now say they're estimates at API prices covering plan and API turns.
  Only API turns use credits.

## 0.4.3 (2026-11-02)

- **The orchestrator and its threads now talk throughout the work**, the way a Claude Code
  project's sessions do:
  - When a thread finishes a task the orchestrator gave it, its final report goes to the
    orchestrator right away. An idle orchestrator wakes up to act on it, a working one
    gets it mid-turn, and one in `wait_for_threads` gets it as that call's result.
  - Thread messages (`message_orchestrator`) also reach a working orchestrator mid-turn
    now, instead of waiting for its next step.
  - Threads are told to report milestones, ask questions and flag blockers. The
    orchestrator is told to reply, redirect busy threads, relay results between threads
    and check in on long-running ones.
- Turns you drive yourself in a thread still don't wake the orchestrator; it sees them
  as updates with your next message.

## 0.4.2 (2026-11-02)

- **Messages reach a working agent right away**: send a message while a thread or the
  orchestrator is mid-turn and it goes into that turn. Claude reads it at its next step
  and adjusts, the same way Claude desktop works, instead of waiting for the turn to
  finish. Press Alt+Enter to queue a message for after the turn instead. "▶ send now"
  in the queue panel now delivers mid-turn too, rather than interrupting.
- The orchestrator's `send_to_thread` also delivers mid-turn by default, so it can
  redirect a busy thread (`when: "after"` queues instead). A message from you ends the
  orchestrator's `wait_for_threads` early so it sees the message straight away.
- Mid-turn messages are marked "↪ mid-turn" in the transcript.

## 0.4.1 (2026-11-02)

- Threads can now message their orchestrator mid-turn with `message_orchestrator`, for
  example to say a shared resource is free or that they're blocked. If the orchestrator
  is idle, the message starts a turn right away. If it's waiting on threads, the wait
  returns early with the message. Otherwise it gets the message at its next step.
- Claude Code's own cross-session tools (`SendMessage`, `ListAgents`) are now
  turned off inside Orchestrator. Before, a thread that wanted to reach its orchestrator
  could use them and message one of the user's other Claude sessions by mistake.

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
