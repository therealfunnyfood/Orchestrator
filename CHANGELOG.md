# Changelog

## 0.6.4 (2026-11-28)

- **Shared lessons are scoped by workspace type.** There are now three shared lists, plus each
  project's own:
  - **lessons about you**, for every workspace;
  - **development lessons** (models and reviews, testing, QA, deploys, git), for development
    workspaces only;
  - **business lessons**, for business workspaces only.

  Each orchestrator sees only the lists for its workspace type, and `save_lessons` refuses the
  other type's list.
- **Order of precedence.** The orchestrator's instructions now say which guidance wins: your
  newer words, then project rules, then project lessons, then lessons for this workspace type,
  then lessons about you, then its defaults.
- **A one-time sort, done for you.** On the next message in a development workspace, the
  orchestrator sorts the old shared lessons:
  - Lessons about you stay where they are.
  - Lessons about development work move to the development lessons.
  - Lessons about one project move to that project's lessons.

  A backup is saved first (`notes-backup-before-lesson-sort.json`). The app refuses the save if
  any line is dropped or reworded. The chat shows the before and after counts, with every moved
  lesson in a collapsible list. Business workspaces never run the sort. Until it has run, they
  see the old list marked as unsorted.
- **Settings → Workspace → Shared lessons:** by workspace type (the default), only lessons
  about you, or none. Project lessons always apply.
- **📋 brief page.** It shows which lists reach this workspace, and every list can be edited
  there, so you can move a lesson by hand.

## 0.6.3 (2026-11-28)

- **Edit, then approve, from a paired phone.** The phone can draft a revised payload for a
  pending item. The draft changes nothing until the phone signs it, and the approval then
  applies exactly the signed revision. The audit log names the revision.
- **Pause and resume from the phone.** Pausing outbound (the kill switch) or the whole
  workspace needs only the app's token, because stopping is always safe. Resuming needs a
  signature from a paired phone.
- **Manual tasks from the phone.** The phone can add images or PDFs (up to 25 MB) to a manual
  task, then mark it done with a signature over the exact list of files. If a file is added or
  removed after signing, the Done is refused.
- **Feed.** Items now carry what a phone needs to recompute their hash (attachment paths,
  hash version, secret names), plus drafts and dropped files. New `pause_changed` and
  `manual_file` events.

## 0.6.2 (2026-11-28)

- **Business lines are data (business workspaces).** Each line, or venture, has an id, a
  status (testing, active, paused or killed), a playbook, a weekly budget, kill criteria,
  and start and end dates.
  - You add and change lines in the new **$ Money** tab.
  - The orchestrator proposes new ventures with `propose_line`, and asks to pause, kill or
    revive one with `update_line_status`. Both become queue items, applied exactly as
    approved.
  - `list_lines` gives the orchestrator every line with its numbers.
- **Per-line caps.** `spawn_thread` takes a `tag` naming a line. A tagged thread's usage counts
  against that line's weekly budget. A line over its budget is paused automatically, you and
  the orchestrator are told, and only paid-order threads start for it. Threads for paused or
  killed lines are refused too.
- **Ledger booking.** When a tagged thread is resolved, its agent usage is written to
  `ledger/ledger.csv` as a `tokens` row, if that file exists.
- **Money tab.** It shows revenue, fees, costs, agent usage and net for this week, last week
  and all time. Per line, it adds profit per hour of your time (from `hours` rows in the
  ledger) and per dollar of agent usage, an 8-week chart of weekly net, the week's usage
  against the budget, and days left against the kill criteria for lines in testing.
- **Phone feed.** The phone's snapshot now includes the lines and the new ledger fields.

## 0.6.1 (2026-11-28)

- **Approve and reject from your phone, signed with Face ID (business workspaces).** This is
  phase 2 of the Atlas Business tab, and it's off by default.
  - **Pairing.** Pair a phone in **Settings → Phone → Paired phones**: paste the bundle it
    shows, check that the fingerprint matches the one on the phone, and confirm. You can
    revoke a phone at any time.
  - **Turning it on.** Then tick "Allow approve / reject from paired phones" for the workspace.
- **Orchestrator checks every phone decision itself.** A decision counts only with a
  signature from a phone you paired, over that exact item's **current** content, made in
  the last 2 minutes, and with a one-time nonce. The local API token alone can never decide
  anything, because agents have it.
- A phone decision then goes through exactly the same path as a click in the window, and
  the audit log records which phone made it. Offers that failed the capacity check still
  need your second click on the PC.
- **Item hashes cover more.** An item's hash now also covers its type, summary, risk and
  urgency, so what you're shown is what you approve. Items you'd already approved keep
  their approval.
- **Approvals record what you saw.** An approval, from the window or a phone, now records
  the hash of what you saw. Content swapped in afterwards (an attachment file, say) makes
  the item void instead of approved. "Approve all low-risk" approves only the cards you
  were shown.

## 0.6.0 (2026-11-28)

- **Read-only business feed for your phone (business workspaces).** This is the first step
  of the Atlas iPhone app's Business tab. It's off by default; turn it on per workspace in
  **Settings → Phone access**. A companion on this PC (the Atlas relay) can then read, over
  the local API, everything the tab shows:
  - pending items with their exact payloads, hashes and attachment details;
  - today's reservations, deadlines, waiting items and plan usage;
  - budget and ledger totals (this week, last week, all time, per line);
  - schedules with their next run.
- **Live events.** A stream of changes: items created, decided or completed, pauses, usage
  gates, budget alerts, and orders at risk. Push hints mark the events worth a
  notification: a buyer waiting over 4 hours, an order at 50% or 75% of its time, usage
  gates and budget.
- **Thumbnails.** Attachments are available as 512 px JPEG thumbnails, or in full on request.
- The feed can't approve or change anything. Decisions from the phone will need a Face ID
  signature that Orchestrator checks itself (0.6.1). The API is documented for the Atlas
  side in the business workspace's `integrations/orchestrator-api.md`.

## 0.5.10 (2026-11-28)

- **Manual tasks with a file drop (business workspaces).** This is for queue items you do by
  hand in another app (`manual_image`, or anything with `who_executes: user`), for example
  generating images in ChatGPT.
- Their card shows each prompt (each code block, or each paragraph) with a **Copy** button.
- A drop zone on the card saves the files you drag in to `inbox/images/<item-id>/`, and
  shows image previews.
- **Done** marks the item done and tells the orchestrator which files are in, so it can pick
  them up. Manual tasks are left out of "Approve all low-risk".

## 0.5.9 (2026-11-28)

- **Notifications (business workspaces).** Windows notifications for urgent queue items,
  finished scheduled runs (including standing threads), budget alerts, orders at risk of
  missing their deadline, and usage gates.
- Choose which events notify you in **Settings → Notifications**, and use **Send a test** to
  check it works.
- **Phone push (optional).** Get the same notifications on your phone through ntfy.sh or your
  own Telegram bot. Add `NTFY_TOPIC`, or `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID`, under
  Secrets.
- Only a one-line summary is sent, with secrets redacted, never an item's payload.

## 0.5.8 (2026-11-28)

- **Inbox capture (business workspaces).** Paste text or drop files into the new Inbox box
  at the top of the Queue tab, or click **📋 From clipboard**.
- **Ctrl+Alt+I** works anywhere, even with the window in the tray. It saves what's on the
  clipboard, text or an image, to the business workspace you're looking at, or else to the
  first open one.
- Each capture is saved as `<workspace>/inbox/YYYYMMDD-HHMM-<slug>.md`, with images and
  files saved next to it, and the orchestrator is told so it can triage it.
- The clipboard is only read when you press the hotkey or click the button, never through
  the app's local API.

## 0.5.7 (2026-11-28)

- **Business mode for the orchestrator's instructions.** In a business workspace, the
  development-specific parts of its built-in instructions are swapped for business ones.
  - It picks a mode per task as the workspace's `pipelines/models.md` describes: a script,
    Haiku, Sonnet (the default), Opus planning with Sonnet building, Opus alone, or Fable only
    to rescue a paid order.
  - It uses business kinds of work and examples.
  - A new "Running the business" section has it follow the playbooks, judge work by expected
    profit, put paid orders first, send everything outbound through the queue, run the
    weekly review, and turn rejected items into lessons.
  - Everything else is the same text in both modes, and a test checks that it stays that
    way. Development workspaces get exactly the same instructions as before.
- Fixed a capacity note that could read "reserved -0.0".

## 0.5.6 (2026-11-28)

- **Usage-aware admission control (business workspaces).** A workspace that runs on your
  Claude plan no longer takes paid work it can't finish. Usage is measured in weekly points,
  meaning percentage points of the plan's weekly limit.
- **`usage_status` tool.** It shows the 5-hour and weekly usage, reset times, usage burned
  in the last hour, and what's projected to be left before a given time. The projection
  counts weekly resets only for the time they actually leave. It also shows the usage per
  kind of work and model, learned from the app's own usage readings and recorded costs.
  Readings older than 15 minutes are refreshed with a tiny probe. If usage is still
  unknown, it says so.
- **Capacity checks on offers.** `send_offer`, `accept_order` and `publish_gig` items must
  give an order, an estimate and a deadline. The app checks that the reserved orders plus
  the new one fit in 50% of the usage projected to be left before the deadline. If they
  don't, the card shows it in red, and approving it takes a second, explicit click, which is
  logged. Accepted orders are held with `reserve_capacity` and `release_capacity`.
- **Usage gates.** At 70% of the 5-hour window or 60% of the week, only threads tagged
  `paid-order` start. At 80% of the week, running non-paid threads are asked to checkpoint
  and stop, and the orchestrator is told. Paid-order threads are never refused.
- **Deadline watchdog.** Every 30 minutes, the app checks each reserved order against the
  usage projected to be left before 75% of its deadline. If an order is at risk, you and the
  orchestrator get an urgent warning.
- **API fallback for paid orders only.** Business workspaces stay on the plan. With an API
  key and the setting on, paid-order threads (and only those) may finish on the API when
  the plan runs out, up to a dollar cap per order.
- All thresholds are in **Settings → Usage**, and can only be changed in the app window.

## 0.5.5 (2026-11-28)

- **Standing threads (business workspaces).** A schedule can now wake a named thread
  directly instead of the orchestrator, for example `market-watch` every day at 06:00.
  Pick the thread under **Settings → Schedules → send to**, or have the orchestrator
  propose it with `propose_schedule(thread=...)`.
- When a standing thread finishes a scheduled run, its report goes to the orchestrator.
- Standing threads show 🔁 in the sidebar with their next run time and this week's usage.
  The orchestrator can't resolve them, but you still can.

## 0.5.4 (2026-11-28)

- **Scheduled routines (business workspaces).** In **Settings → Schedules**, have the app send
  the orchestrator a message at set local times, for example "08:00 every day: morning run"
  or "Sunday 18:00: weekly review". Choose daily, weekdays, weekends or single days.
- The orchestrator can propose a schedule with the new `propose_schedule` tool, but it only
  runs once you confirm it.
- A run is skipped when the workspace is paused, its budget is spent, or the previous run
  is still going. Each skip is shown in the chat and written to the audit log. Missed times
  aren't caught up later. If a run hits the plan's usage limit, it continues after the
  reset as usual.
- **Start at sign-in.** Settings → About has a new option to start Orchestrator minimized to
  the tray when you sign in to Windows, so routines keep running.

## 0.5.3 (2026-11-28)

- **Budget caps (business workspaces).** Set daily and weekly dollar caps in **Settings →
  Budget**. They count each turn's cost at API rates, on the plan or the API, from the costs
  the app already records. Caps can only be changed in the app window.
- At **80%** of a cap you get a notification, the taskbar flashes, and the orchestrator is
  told. At **100%**, new threads are refused unless their task is tagged as a paid order
  (`spawn_thread` with `priority: "paid-order"`). The orchestrator is told why, and running
  work carries on. Each alert is sent once per day or week, and written to the audit log.
- The status bar shows today's spend against the cap.

## 0.5.2 (2026-11-28)

- **Secrets vault (business workspaces).** Add API keys and tokens in **Settings →
  Secrets**. They're encrypted with your Windows account (DPAPI) in their own file, never
  stored in plain text or in the app's state, and the window only ever shows their names.
  They can only be added or removed in the app window.
- **Secrets are only used for approved actions by default.** An agent proposes a queue
  item with a PowerShell `command` that reads `$env:NAME` and lists the secrets it needs.
  Once you approve the item, the new `run_approved` tool runs exactly that command, once,
  with only those secrets set. Each run and secret access is recorded in the audit log.
  For harmless keys you can choose **all sessions** instead, which sets the secret as an
  environment variable in every thread.
- **Redaction.** Secret values are replaced with `[secret:NAME]` in transcripts, the live
  window, run output, queue results and the audit log. A queue item that contains a secret's
  value is refused. Claude Code's own session files (`~/.claude/projects`) are outside the
  app and aren't redacted.

## 0.5.1 (2026-11-28)

- **Kill switch (business workspaces).** **⏸ Pause outbound**, in the status bar or the
  Queue tab, stops agents from getting any approved item to execute until you resume. They
  get a clear "paused, don't work around this" message instead. The tray menu has **Pause
  outbound (all business workspaces)**.
- **Pause workspace** stops every running turn in a business workspace right away. Agents
  can't start or message threads, and thread news and automatic turns (such as continuing
  after a usage limit) wait until you resume. Your own messages still go through.
- Only you can flip these switches, from the app window.
- **Tamper-evident audit log.** `queue/audit.log` now also records pauses and resumes, and
  each line ends with a hash chained to the line before. The file is kept read-only. If
  it's edited outside the app, the next entry records a `TAMPER:` line naming the altered
  lines. A log written by 0.5.0 is marked `LEGACY` once, and chaining starts from there.

## 0.5.0 (2026-11-28)

- **Business workspaces.** A new setting, Settings → Workspace type, can switch a workspace
  to **Business**. That turns on the features for running work that reaches the outside
  world. Development workspaces work exactly as before.
- **Approval queue.** In a business workspace, agents propose anything outbound
  (publishing, messages, offers, spending) with the new `queue_item` tool, and you decide in
  the new **✓ Queue** tab. Each item shows its type, line, expected value, risk, exact
  payload and image previews. You can **Approve**, **Edit, then approve** or **Reject**
  (with a one-line reason), or approve all low-risk items at once. The orchestrator gets
  every decision with your reason, so it learns from rejections. Agents execute only
  what `list_approved` returns and record results with `complete_item`.
- **Only you can approve.** Decisions travel from the app window to the app over a private
  channel that agents can't reach. An approval covers the exact payload and the contents
  of every attachment. If either changes afterwards, the item is void and never executed.
- Items are also written as read-only Markdown files in `<workspace>/queue/`, plus an
  append-only `queue/audit.log`. The sidebar and the tray icon show how many items are
  waiting for you.

## 0.4.27 (2026-11-24)

- **Lessons are split in two.** "Lessons about you" are shared by every project; "lessons for
  this project" only reach that project's orchestrator. On its next message, the orchestrator
  sorts your existing lessons into the right places (once). Both lists are on the 📋 brief page.
- **Size limits.** The brief and rules (5,000 characters each), tuning notes (3,000) and lessons
  (5,000, or 4,000 per project) are read on every turn, so the orchestrator now keeps them under
  those limits; anything already over gets tidied on its next message. A copy of all notes and
  lessons is saved first, as `notes-backup-<date>.json` in `%LOCALAPPDATA%\Orchestrator`.
- **Shorter built-in instructions.** The app's own instructions to the orchestrator are about a
  third shorter, with the same rules, grouped by topic.
- **Fixed: notes and lessons not reaching a running conversation.** Newer Claude Code versions
  keep a conversation's system prompt from its first message, so changes to the brief, rules,
  lessons and timing history weren't seen until the conversation was compacted. The app now
  turns that off where Claude Code supports it.

## 0.4.26 (2026-11-24)

- Fixed "could not start Claude Code ... The filename or extension is too long (os error 206)".
  An orchestrator's instructions (project brief, rules, lessons, timing history) were passed
  on the command line, which Windows limits to about 32,000 characters; a project with a lot
  of history went over it. They're now passed in a temporary file, deleted after each turn.

## 0.4.25 (2026-11-18)

- Threads waiting out a usage limit still continue when it resets if you install an update
  in the meantime. Before, the update's restart forgot them. Closing and reopening the app
  yourself still cancels them.
- The orchestrator now knows the current time: every message it gets starts with it. Before,
  it guessed from old messages, and once told you the limit would reset "in about 4 hours"
  after it already had.
- Usage-limit notes give the clock time ("continues by itself at 8:31 PM") instead of
  "in about 4h", which went stale.

## 0.4.24 (2026-11-18)

- A thread that hits the plan's usage limit while a background command is running now
  stops and continues by itself after the limit resets. Since 0.4.21 it stayed "working"
  instead, waiting for the background command, so it never came back.

## 0.4.23 (2026-11-12)

- Saving lessons no longer shows the same line every time. The orchestrator saves its whole
  list of lessons each time, and the chat used to preview the list's first line, which never
  changes. Now the row shows how many lessons there are, and its result lists the lines that
  were added or removed.

## 0.4.22 (2026-11-12)

- The orchestrator no longer sits in a turn for an hour waiting on its threads. Once
  threads are working, it tells you what's running and ends its turn; each thread's messages
  and final report start a new turn, as do your messages. "Working..." now means it is
  actually doing something.
- Waiting on threads inside a turn is limited to 5 minutes, for short waits before it acts on
  a result.

## 0.4.21 (2026-11-11)

- Messages to a thread that is waiting on a background command (a long copy, a build)
  now reach it right away. Before, once the thread said "I'll wait", messages from you
  or the orchestrator were queued until the command finished.
- A thread that finishes its work now ends its turn and reports to the orchestrator.
  Threads can no longer schedule a wake-up for later, which kept a finished thread
  "running" with nothing to do, and a session that lingers after its last reply is
  stopped after 30 seconds.

## 0.4.20 (2026-11-11)

- **Fixed: pickers that open a second picker closed it right away**, e.g. "New folder..." when opening a workspace.

## 0.4.19 (2026-11-11)

- **Fixed: "+ open another workspace..." closed its picker right away.** It now stays open.

## 0.4.18 (2026-11-11)

- **Several workspaces at once.** Open workspaces are listed at the top of the sidebar.
  Each one has its own orchestrator and threads, and they all keep working while you look
  at another: switching no longer waits for the orchestrator to be idle. A dot shows which
  ones are working, "?" marks one waiting for your answer, and x closes one (its
  conversations stay saved). "+ open another workspace..." adds one.
- Thread reports reach their own orchestrator right away, even when you're looking at a
  different workspace. Before, they waited until you switched back.
- Each workspace keeps its own permission level, plan/API mix, brief, rules and timing
  history, as before; work running in the background now uses its own workspace's settings.

## 0.4.17 (2026-11-07)

- **Fixed: the orchestrator "answered" you in its head.** When you messaged it mid-run, it wrote
  its reply in its private thinking, which you never see, and went back to waiting. It now has a
  tell_user tool that puts a message straight into your chat, and wait_for_threads takes a
  message to show you before it waits. Its instructions and the app's reminders point it there.

## 0.4.16 (2026-11-07)

- **Fixed: the app froze when you messaged the orchestrator while it was waiting on threads.**
  Since 0.4.14, a message that arrived during a wait locked up the app, so the orchestrator never
  answered. Now it answers within a couple of seconds.
- **The orchestrator answers first.** When you message it, it replies right away from what it
  knows, before checking anything. If it needs to look something up or wait on a thread, it says
  so in one line, then does it.

## 0.4.15 (2026-11-07)

- **No more "your message didn't come through".** When a thread's report arrived while the
  orchestrator was waiting, the app told it that *you* had sent a message. It then went looking
  for a message from you and asked you to send it again. Now it's told the report came from a
  thread.
- **Automatic restarts are labelled.** The message that restarts work after a usage limit resets
  now shows in the chat as "↻ automatic", not as a message from you. The orchestrator and threads
  are told it came from the app, not from you.

## 0.4.14 (2026-11-07)

- **The orchestrator learns from its mistakes and from how you talk.** It keeps a list of
  lessons that it reads at the start of every conversation, in all your projects. When you
  correct it ("I told you…", "that's not what I meant", "no, …"), it has to fix the problem and
  save a lesson before it can wait on threads again. Every 25 messages it also looks back at
  how you phrase things and what you meant, and updates the list.
- **See and edit the lessons** on the brief page, under "Lessons".
- The orchestrator now knows that work stopped by a usage limit continues by itself when the
  limit resets.

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
