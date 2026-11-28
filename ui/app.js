"use strict";

// ================================================================ basics

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
function h(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const EFFORTS = ["low", "medium", "high", "xhigh", "max"];
const PERMS = [
  ["bypassPermissions", "Full access", "Never asks. Threads can run any command and edit any file."],
  ["auto", "Auto", "Claude Code's safety check approves routine actions and asks you about risky ones."],
  ["acceptEdits", "Edits OK, ask for the rest", "File edits run freely; commands and other actions need your OK."],
  ["manual", "Ask for everything", "Reading is free; anything that changes something asks you first."],
  ["plan", "Plan only", "Read-only: threads can look and plan but not change anything."],
];
const permLabel = (p) => (PERMS.find((x) => x[0] === p) || [p, p])[1];
const mixLabel = (m) => m === 0 ? "all plan" : m === 10 ? "all API" : `${10 - m} plan · ${m} API`;
const MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"];
const SPINNER = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"];

const PARAMS = new URLSearchParams(location.search);
const VIEW = PARAMS.get("view");            // pop-out window or side panel: one conversation
// Pane window (⧉ Pop out): only the Queue or Needs you pane, for the shown workspace.
const PANE = ["queue", "needs"].includes(PARAMS.get("pane")) ? PARAMS.get("pane") : null;
const SOLO = !!VIEW || !!PANE;
if (PANE) document.body.classList.add("pane-only");
const EMBEDDED = SOLO && window.parent !== window;
if (SOLO) document.body.classList.add("solo");
if (EMBEDDED) document.body.classList.add("embedded");

const S = {
  state: null,
  tabs: [],          // {type: 'chat'|'manage'|'settings', key?}
  active: 0,
  unread: new Set(),
  toolEls: {},
  lastTool: null,
  manageScope: "ws",
  manageFilter: "",
  suggest: { items: [], i: 0 },
  attachments: [],   // {name, path, url?, uploading}
  skipTools: new Set(),
  planOpen: true,
  seen: new Set(),   // events already rendered in the current log (history + live can overlap)
  side: null,        // thread shown in the side panel
};
const APP_TOOLS = new Set(["mcp__app__update_plan", "mcp__app__ask_user", "mcp__app__show_media", "mcp__app__tell_user"]);
const fileUrl = (p) => `/file?t=${encodeURIComponent(ORCH.token)}&path=${encodeURIComponent(p)}`;
const IMG_EXT = /\.(png|jpe?g|gif|webp|svg|bmp)$/i;

async function api(path, opts = {}) {
  const res = await fetch(path, {
    method: opts.method || (opts.body !== undefined ? "POST" : "GET"),
    headers: { "X-Orch-Token": ORCH.token, "Content-Type": "application/json" },
    body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `${res.status} ${res.statusText}`);
  return data;
}

// The user's own actions (sending, editing, answering, new threads, permission levels, the
// effort cap, the API key) go over the window's private IPC channel, never HTTP: agents can
// read the HTTP token. The app replies with {"kind":"ipc_reply","req",...} on the websocket.
const IPC_WAIT = new Map();
let ipcSeq = 0;
function ipcCall(op, body = {}) {
  return new Promise((resolve, reject) => {
    if (!window.ipc?.postMessage) return reject(new Error("This only works in the Orchestrator window, not in a browser"));
    const req = `r${Date.now()}-${++ipcSeq}`;
    const t = setTimeout(() => { IPC_WAIT.delete(req); reject(new Error("the app didn't answer")); }, 30000);
    IPC_WAIT.set(req, { resolve, reject, t });
    window.ipc.postMessage(JSON.stringify({ ...body, op, req }));
  });
}
function ipcReply(ev) {
  const w = IPC_WAIT.get(ev.req);
  if (!w) return;
  IPC_WAIT.delete(ev.req);
  clearTimeout(w.t);
  if (ev.ok) w.resolve(ev.result || {}); else w.reject(new Error(ev.error || "failed"));
}
const USER_ONLY_SETTINGS = ["max_effort", "permission_default", "api_key", "clear_api_key"];
function saveSettings(patch) {
  return Object.keys(patch).some((k) => USER_ONLY_SETTINGS.includes(k)) ? ipcCall("settings_user", { patch }) : api("/api/settings", { body: patch });
}

function toast(text, err) {
  const t = $("#toast");
  t.textContent = text;
  t.className = "toast" + (err ? " err" : "");
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.add("hidden"), err ? 5000 : 2400);
}
const fail = (e) => toast(e.message || String(e), true);

const baseName = (p) => (p || "").replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
const money = (v) => "$" + (v || 0).toFixed((v || 0) >= 1 ? 2 : 4);
const orchKey = () => S.state?.orchestrator.name;
const isOrch = (k) => !!k && k.startsWith("orchestrator");
const threadRow = (n) => S.state?.threads.find((t) => t.name === n);
const effortLabel = (e) => e || "default";
const modelLabel = (m) => (m || "default").replace(/^claude-/, "");

function relTime(ts) {
  const s = Date.now() / 1000 - ts;
  if (s < 60) return "now";
  if (s < 3600) return Math.floor(s / 60) + "m";
  if (s < 86400) return Math.floor(s / 3600) + "h";
  return Math.floor(s / 86400) + "d";
}

function info(key) {
  if (isOrch(key)) return { ...S.state.orchestrator, isOrch: true };
  const r = threadRow(key);
  return r ? { ...r, isOrch: false } : null;
}
const cur = () => S.tabs[S.active];
const curKey = () => (cur()?.type === "chat" ? cur().key : null);

// ================================================================ markdown (small, escape-first)

// Looks like a file or folder path: absolute (C:\..., \\server, ~/...), or relative with a
// folder separator and a file extension or trailing slash. An optional :line may follow.
function looksLikePath(s) {
  if (!s || s.length > 400 || /:\/\/|[<>"|*?\n]/.test(s)) return false;
  const p = s.replace(/(?::\d+){1,2}$/, "");
  if (/^([A-Za-z]:[\\/]|\\\\|~[\\/])/.test(p)) return true;
  if (p.includes(":") || /\s/.test(p)) return false;
  return /[\\/]/.test(p) && (/\.[A-Za-z0-9]{1,8}$/.test(p) || /[\\/]$/.test(p));
}
const isDoc = (p) => /\.(md|markdown|mdx|txt)(?::\d+)*$/i.test(p);
const FILE_TIP = "Show in Explorer · Ctrl+click to open in the editor";
const DOC_TIP = "View here · Shift+click to show in Explorer · Ctrl+click to open in the editor";
const fileAttrs = (p) => `data-path="${p}" title="${isDoc(p) ? DOC_TIP : FILE_TIP}"`;

function inline(s) {
  const codes = [];
  s = s.replace(/`([^`\n]+)`/g, (_, c) => { codes.push(c); return `\u0000${codes.length - 1}\u0000`; });
  s = esc(s);
  s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
       .replace(/(^|[^*\w])\*([^*\n]+)\*(?!\w)/g, "$1<em>$2</em>")
       .replace(/!\[([^\]]*)\]\(([^)\s]+)\)/g, (_, alt, src) => {
         const url = /^https?:/.test(src) ? src : fileUrl(src.replace(/^file:\/\/\/?/, "").replace(/&amp;/g, "&"));
         return `<img src="${url}" alt="${alt}">`;
       })
       .replace(/\[([^\]]+)\]\(thread:([A-Za-z0-9_-]+)\)/g, '<a class="tlink" data-thread="$2">$1</a>')
       .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
       .replace(/\[([^\]]+)\]\(&lt;(.+?)&gt;\)|\[([^\]]+)\]\(([^\s)]+)\)/g, (m, l1, t1, l2, t2) => {
         const label = l1 ?? l2;
         let target = t1 ?? t2;
         if (/^#/.test(target)) return label;
         if (/^(https?|mailto|thread):/.test(target)) return m;
         target = target.replace(/^file:\/\/\/?/, "").replace(/#L(\d+)(?:-L?\d+)?$/, ":$1");
         return `<a class="flink" ${fileAttrs(target)}>${label}</a>`;
       });
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => {
    const c = codes[+i];
    return looksLikePath(c.trim()) ? `<code class="flink" ${fileAttrs(esc(c.trim()))}>${esc(c)}</code>` : `<code>${esc(c)}</code>`;
  });
}

function md(src) {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const out = [];
  let i = 0;
  const isTableSep = (l) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(l);
  const cells = (l) => l.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
  while (i < lines.length) {
    const l = lines[i];
    const fence = l.match(/^\s*```(\S*)/);
    if (fence) {
      const buf = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i++]);
      i++;
      out.push(`<pre><code>${esc(buf.join("\n"))}</code></pre>`);
      continue;
    }
    if (!l.trim()) { i++; continue; }
    const hd = l.match(/^(#{1,4})\s+(.*)/);
    if (hd) { out.push(`<h${hd[1].length}>${inline(hd[2])}</h${hd[1].length}>`); i++; continue; }
    if (/^\s*(-{3,}|\*{3,})\s*$/.test(l)) { out.push("<hr>"); i++; continue; }
    if (l.includes("|") && i + 1 < lines.length && isTableSep(lines[i + 1])) {
      const head = cells(l);
      i += 2;
      const rows = [];
      while (i < lines.length && lines[i].includes("|") && lines[i].trim()) rows.push(cells(lines[i++]));
      out.push("<table><tr>" + head.map((c) => `<th>${inline(c)}</th>`).join("") + "</tr>" +
        rows.map((r) => "<tr>" + r.map((c) => `<td>${inline(c)}</td>`).join("") + "</tr>").join("") + "</table>");
      continue;
    }
    if (/^\s*>/.test(l)) {
      const buf = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) buf.push(lines[i++].replace(/^\s*>\s?/, ""));
      out.push(`<blockquote>${inline(buf.join(" "))}</blockquote>`);
      continue;
    }
    const li = l.match(/^\s*([-*+]|\d+[.)])\s+/);
    if (li) {
      const ordered = /\d/.test(li[1]);
      const items = [];
      while (i < lines.length) {
        const m = lines[i].match(/^\s*([-*+]|\d+[.)])\s+(.*)/);
        if (m) { items.push(m[2]); i++; }
        else if (lines[i].trim() && /^\s{2,}/.test(lines[i]) && items.length) { items[items.length - 1] += " " + lines[i].trim(); i++; }
        else break;
      }
      const tag = ordered ? "ol" : "ul";
      out.push(`<${tag}>` + items.map((t) => `<li>${inline(t)}</li>`).join("") + `</${tag}>`);
      continue;
    }
    const buf = [];
    while (i < lines.length && lines[i].trim() && !/^\s*(```|#{1,4}\s|>|[-*+]\s|\d+[.)]\s)/.test(lines[i])) buf.push(lines[i++]);
    if (!buf.length) buf.push(lines[i++]);
    out.push(`<p>${buf.map(inline).join("<br>")}</p>`);
  }
  return out.join("");
}

// ================================================================ state

let stateTimer = null;
function scheduleState() { clearTimeout(stateTimer); stateTimer = setTimeout(loadState, 80); }

async function loadState() {
  try { S.state = await api("/api/state"); } catch (e) { return fail(e); }
  document.documentElement.dataset.theme = theme(S.state.defaults.theme);
  if (PANE) {
    // It follows the main window's workspace (the state's shown workspace).
    S.tabs = [{ type: PANE }];
    S.active = 0;
    document.title = (PANE === "queue" ? "Queue" : "Needs you") + " — Orchestrator";
    renderChrome();
    if (PANE === "queue") renderQueuePane(); else renderNeedsPane();
    return;
  }
  // keep tabs valid: orchestrator tab follows the workspace, dead threads close
  if (SOLO) {
    const key = isOrch(VIEW) ? orchKey() : VIEW;
    S.tabs = [{ type: "chat", key }];
    S.active = 0;
    document.title = (isOrch(key) ? "orchestrator" : key) + " — Orchestrator";
    if (!isOrch(key) && !threadRow(key)) {
      $("#panes").innerHTML = `<div class="solo-missing">Thread “${esc(key)}” isn't in this workspace any more.</div>`;
      return;
    }
    renderChrome();
    return;
  }
  S.tabs = S.tabs.filter((t) => t.type !== "chat" || isOrch(t.key) || threadRow(t.key));
  for (const t of S.tabs) if (t.type === "chat" && isOrch(t.key)) t.key = orchKey();
  if (!S.tabs.length || !(S.tabs[0].type === "chat" && isOrch(S.tabs[0].key))) S.tabs.unshift({ type: "chat", key: orchKey() });
  if (S.side && !threadRow(S.side)) closeSide();
  S.active = Math.min(S.active, S.tabs.length - 1);
  renderChrome();
  if (cur().type === "manage") renderManage();
  if (cur().type === "queue") renderQueuePane();
  renderNeedsPane();
}

function theme(t) {
  if (t === "light" || t === "dark") return t;
  return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function renderChrome() {
  renderTree();
  renderTabs();
  renderStatus();
  renderCrumbs();
  renderChips();
  renderPlan();
  renderQueue();
  renderNeedsStrip();
  updateWorking();
  const busy = curKey() && info(curKey())?.busy;
  $("#stop").classList.toggle("hidden", !busy);
  for (const a of $$(".act")) {
    const t = cur()?.type;
    const on = (a.dataset.act === "orch" && t === "chat" && isOrch(curKey())) || a.dataset.act === t ||
      (a.dataset.act === "explorer" && !$("#side").classList.contains("collapsed") && t === "chat" && !isOrch(curKey()));
    a.classList.toggle("on", !!on);
  }
  const running = S.state.running;
  const mb = $('.act[data-act="manage"]');
  mb.querySelector(".badge")?.remove();
  if (running) { const b = h("span", "badge", String(running)); mb.append(b); }
  const qb = $('.act[data-act="queue"]');
  const business = S.state.project?.kind === "business";
  qb.classList.toggle("hidden", !business);
  qb.querySelector(".badge")?.remove();
  const qn = S.state.queue?.pending?.length || 0;
  if (business && qn) qb.append(h("span", "badge warn", String(qn)));
  $('.act[data-act="money"]').classList.toggle("hidden", !business);
  const nb = $('.act[data-act="needs"]');
  nb.querySelector(".badge")?.remove();
  const nn = S.state.needs?.count || 0;
  if (nn) nb.append(h("span", "badge" + (S.state.needs.urgent ? " urgent" : ""), String(nn)));
}

// ================================================================ sidebar tree

function stClass(t) { return t.busy ? (t.status === "queued" ? "queued" : "running") : t.status; }

const lastSig = {};
// A pane rebuilt from a state push must not lose the reader's place: remember the first
// card in view ([data-anchor]) and how far down it sat, then put it back there, so cards
// added or removed above don't move what they're reading. Falls back to scrollTop.
function scrollState(pane) {
  const page = $(".page", pane);
  if (!page) return null;
  const box = page.getBoundingClientRect();
  const a = [...page.querySelectorAll("[data-anchor]")].find((e) => e.getBoundingClientRect().bottom > box.top + 1);
  return { top: page.scrollTop, anchor: a?.dataset.anchor, offset: a ? a.getBoundingClientRect().top - box.top : 0 };
}

function restoreScroll(pane, s) {
  const page = $(".page", pane);
  if (!s || !page) return;
  const a = s.anchor && [...page.querySelectorAll("[data-anchor]")].find((e) => e.dataset.anchor === s.anchor);
  if (!a) { page.scrollTop = s.top; return; }
  // Parts that fill in later (thumbnails, file lists) grow the cards above: keep the card
  // pinned as they do, for a few seconds or until the user scrolls themselves.
  let mine = false;
  for (const ev of ["wheel", "mousedown", "keydown", "touchstart"]) page.addEventListener(ev, () => { mine = true; }, { once: true });
  const pin = () => { if (!mine && a.isConnected) page.scrollTop += a.getBoundingClientRect().top - page.getBoundingClientRect().top - s.offset; };
  pin();
  const ro = new ResizeObserver(pin);
  for (const el of page.children) ro.observe(el);
  for (const el of page.querySelectorAll("[data-anchor]")) ro.observe(el);
  setTimeout(() => ro.disconnect(), 3000);
}

function changed(part, data) {
  const sig = JSON.stringify(data);
  if (lastSig[part] === sig) return false;
  lastSig[part] = sig;
  return true;
}

function renderTree() {
  if (!changed("tree", [S.state.workspace, S.state.open_workspaces, S.state.orchestrator, S.state.threads.map((t) => [t.name, stClass(t), t.model, t.effort, t.updated_at, t.needs_input, t.plan, t.estimate, t.resolved]), curKey(), [...S.unread]])) return;
  renderWorkspaces();
  const tree = $("#tree");
  tree.textContent = "";
  const o = S.state.orchestrator;
  tree.append(node({ key: o.name, name: "orchestrator", cls: "orch", status: stClass(o), meta: effortLabel(o.effort), row: o }));
  const all = [...S.state.threads].sort((a, b) => (b.busy - a.busy) || (b.updated_at - a.updated_at));
  const rows = all.filter((t) => !t.resolved);
  const done = all.filter((t) => t.resolved).sort((a, b) => b.resolved_at - a.resolved_at);
  const meta = (t) => [modelLabel(t.model) === "default" ? "" : modelLabel(t.model).replace(/-\d.*/, ""), t.effort].filter(Boolean).join("·");
  tree.append(h("div", "group", `threads · ${rows.length}`));
  if (!rows.length) tree.append(h("div", "empty-tree", done.length ? "all resolved — ask the orchestrator for more, or + to add one" : "none yet — ask the orchestrator, or + to add one"));
  for (const t of rows) tree.append(node({ key: t.name, name: t.name, status: stClass(t), meta: meta(t), row: t }));
  if (done.length) {
    const open = resolvedOpen();
    const g = h("div", "group resolved-group" + (open ? " open" : ""));
    g.append(h("span", "chev", open ? "⌄" : "›"), h("span", null, `resolved · ${done.length}`));
    g.title = open ? "Hide resolved threads" : "Show resolved threads";
    g.onclick = () => { resolvedOpen(!open); lastSig.tree = null; renderTree(); };
    tree.append(g);
    if (open) for (const t of done) {
      const n = node({ key: t.name, name: t.name, cls: "resolved", status: stClass(t), meta: meta(t), row: t });
      if (t.resolved_note) n.title = `${t.name} · resolved: ${t.resolved_note}`;
      tree.append(n);
    }
  }
}

// Whether the sidebar's resolved group is expanded (remembered per browser).
function resolvedOpen(set) {
  try {
    if (set !== undefined) localStorage.setItem("orch.resolvedOpen", set ? "1" : "");
    return !!localStorage.getItem("orch.resolvedOpen");
  } catch { return !!set; }
}

async function setResolved(name, resolved) {
  try {
    await api(`/api/threads/${encodeURIComponent(name)}`, { method: "PATCH", body: { resolved } });
    toast(resolved ? `resolved ${name}` : `reopened ${name}`);
    await loadState();
  } catch (e) { fail(e); }
}

function planProgress(plan) {
  if (!plan?.length) return "";
  return `${plan.filter((p) => p.status === "completed").length}/${plan.length}`;
}

// Open workspaces: each runs its own orchestrator and threads, all at once. The
// selected one is shown below; the others keep working in the background.
function renderWorkspaces() {
  const list = $("#ws-list");
  list.textContent = "";
  const open = S.state.open_workspaces?.length ? S.state.open_workspaces : [{ path: S.state.workspace, current: true }];
  list.append(h("div", "group", `workspaces · ${open.length}`));
  for (const w of open) {
    const working = w.busy || w.running_threads > 0;
    const r = h("div", "node ws-tab" + (w.current ? " on" : ""));
    r.append(h("span", "st " + (working ? "running" : "idle")), h("span", "nm", baseName(w.path)));
    if (w.needs_input) { const q = h("span", "q-badge", "?"); q.title = "Waiting for your answer"; r.append(q); }
    if (w.queue_pending) { const q = h("span", "qn-badge", String(w.queue_pending)); q.title = `${w.queue_pending} waiting for your approval`; r.append(q); }
    if (w.needs_count) { const q = h("span", "nd-badge" + (w.needs_urgent ? " urgent" : ""), String(w.needs_count)); q.title = `${w.needs_count} need you${w.needs_urgent ? ", " + w.needs_urgent + " urgent" : ""}`; r.append(q); }
    if (w.running_threads) r.append(h("span", "meta", `${w.running_threads} running`));
    if (open.length > 1) {
      const x = h("button", "ib ws-close", "×");
      x.title = "Close this workspace (its conversations stay saved)";
      x.onmousedown = (e) => { e.stopPropagation(); e.preventDefault(); };
      x.onclick = (e) => { e.stopPropagation(); closeWorkspace(w.path); };
      r.append(x);
    }
    r.title = `${w.path}\n${w.busy ? "orchestrator working" : "orchestrator idle"}` + (w.running_threads ? ` · ${w.running_threads} threads running` : "") + (w.current ? "" : "\nclick to show it");
    r.onmousedown = (e) => { if (e.button === 0 && !w.current) switchWorkspace(w.path); };
    list.append(r);
  }
  const add = h("div", "node ws-add");
  add.append(h("span", "st"), h("span", "nm", "+ open another workspace…"));
  add.title = "Open a folder as another workspace, with its own orchestrator (the others keep running)";
  // On click, not mousedown: the rest of a mousedown's click would take focus from the
  // picker's input and close it.
  add.onclick = () => pickWorkspace();
  list.append(add);
}

// Editor tabs are remembered per workspace while the app is open.
const tabsByWs = {};
async function switchWorkspace(path, create = false) {
  try {
    if (S.state?.workspace) tabsByWs[S.state.workspace.toLowerCase()] = { tabs: S.tabs, active: S.active };
    const r = await api("/api/workspace", { body: { path, create } });
    restoreTabs(r.workspace);
    await loadState();
    await activate(S.active);
    lastSig.tree = null;
    renderTree();
  } catch (e) { fail(e); }
}

function restoreTabs(ws) {
  const saved = tabsByWs[(ws || "").toLowerCase()];
  S.tabs = saved ? saved.tabs : [];
  S.active = saved ? saved.active ?? 0 : 0;
}

async function closeWorkspace(path) {
  try {
    const wasShown = S.state.workspace.toLowerCase() === path.toLowerCase();
    const r = await api("/api/workspace/close", { body: { path } });
    delete tabsByWs[path.toLowerCase()];
    if (wasShown) restoreTabs(r.workspace);
    await loadState();
    if (wasShown) await activate(S.active);
    lastSig.tree = null;
    renderTree();
    toast("closed " + baseName(path));
  } catch (e) { fail(e); }
}

function node({ key, name, cls, status, meta, row }) {
  const n = h("div", "node" + (cls ? " " + cls : ""));
  n.classList.toggle("on", curKey() === key);
  n.classList.toggle("unread", S.unread.has(key));
  n.append(h("span", "st " + status), h("span", "nm", (row?.standing ? "🔁 " : "") + name));
  if (row?.needs_input) { const q = h("span", "q-badge", "?"); q.title = "Waiting for your answer"; n.append(q); }
  const prog = row?.busy && planProgress(row.plan);
  if (prog) n.append(h("span", "prog", prog));
  if (row?.estimate) n.append(estBadge(row.estimate, true));
  else if (row?.standing && !row.busy) {
    const next = row.next_run ? new Date(row.next_run * 1000) : null;
    const sameDay = next && next.toDateString() === new Date().toDateString();
    const when = next ? (sameDay ? "" : next.toLocaleDateString([], { weekday: "short" }) + " ") + next.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "no run scheduled";
    n.append(h("span", "meta", `${when} · $${(row.week_cost || 0).toFixed(2)}/wk`));
  } else n.append(h("span", "meta", meta));
  n.title = `${name} · ${status}` + (row?.standing ? `\nstanding thread: woken by a schedule${row.next_run ? `, next ${new Date(row.next_run * 1000).toLocaleString()}` : ""}; $${(row.week_cost || 0).toFixed(2)} at API rates this week` : "");
  n.onmousedown = (e) => {
    if (e.button !== 0) return;
    if (SOLO) return;
    if (e.altKey && !isOrch(key)) openSide(key);
    else if (e.shiftKey) popOut(key);
    else openChat(key);
  };
  n.onauxclick = (e) => { if (e.button === 1 && !isOrch(key)) openSide(key); };
  if (S.side === key) n.classList.add("focus");
  n.oncontextmenu = (e) => { e.preventDefault(); if (!isOrch(key)) threadMenu(key); };
  return n;
}

// ================================================================ tabs

function tabTitle(t) {
  if (t.type === "manage") return ["☰", "Threads"];
  if (t.type === "settings") return ["⚙", "Settings"];
  if (t.type === "brief") return ["📋", "Project brief"];
  if (t.type === "queue") return ["✓", "Queue"];
  if (t.type === "needs") return ["📌", "Needs you" + (S.state?.needs?.count ? ` (${S.state.needs.count})` : "")];
  if (t.type === "money") return ["$", "Money"];
  if (t.type === "doc") return ["▤", t.name];
  if (isOrch(t.key)) return ["◈", "orchestrator"];
  return ["›", t.key];
}

function renderTabs() {
  if (!changed("tabs", [S.tabs, S.active, S.state?.needs?.count, S.tabs.map((t) => t.type === "chat" && !!info(t.key)?.busy)])) return;
  const bar = $("#tabs");
  bar.textContent = "";
  S.tabs.forEach((t, i) => {
    const [icon, title] = tabTitle(t);
    const el = h("div", "tab" + (i === S.active ? " on" : ""));
    const inf = t.type === "chat" ? info(t.key) : null;
    const ic = h("span", "ti", icon);
    if (inf?.busy) { ic.className = "st running"; ic.textContent = ""; }
    el.append(ic, h("span", "tn", title));
    const pinned = i === 0;
    if (t.type === "chat") {
      const pop = h("button", "pop", "⧉");
      pop.title = "Open in a new window";
      pop.onmousedown = (e) => e.stopPropagation();
      pop.onclick = (e) => { e.stopPropagation(); popOut(t.key); if (!pinned) closeTab(i); };
      el.append(pop);
    }
    const x = h("button", "x" + (pinned ? " pin" : ""), "×");
    x.title = "Close (Ctrl+W)";
    x.onclick = (e) => { e.stopPropagation(); closeTab(i); };
    el.append(x);
    el.onmousedown = (e) => { if (e.button === 0 && !e.target.closest(".x")) activate(i); };
    el.onauxclick = (e) => { if (e.button === 1 && !pinned) closeTab(i); };
    bar.append(el);
  });
}

function openTab(tab) {
  let i = S.tabs.findIndex((t) => t.type === tab.type && (!["chat", "doc"].includes(t.type) || t.key.toLowerCase() === tab.key.toLowerCase()));
  if (i === -1) { S.tabs.push(tab); i = S.tabs.length - 1; }
  return activate(i);
}

function closeTab(i) {
  if (i === 0) return;
  S.tabs.splice(i, 1);
  if (S.active >= i) S.active = Math.max(0, S.active - 1);
  activate(S.active);
}

async function activate(i) {
  S.active = i;
  const t = cur();
  $("#pane-chat").classList.toggle("hidden", t.type !== "chat");
  $("#pane-manage").classList.toggle("hidden", t.type !== "manage");
  $("#pane-settings").classList.toggle("hidden", t.type !== "settings");
  $("#pane-brief").classList.toggle("hidden", t.type !== "brief");
  $("#pane-queue").classList.toggle("hidden", t.type !== "queue");
  $("#pane-needs").classList.toggle("hidden", t.type !== "needs");
  $("#pane-money").classList.toggle("hidden", t.type !== "money");
  $("#pane-doc").classList.toggle("hidden", t.type !== "doc");
  if (t.type === "chat") { S.unread.delete(t.key); await loadLog(t.key); }
  if (t.type === "manage") renderManage();
  if (t.type === "settings") renderSettings();
  if (t.type === "brief") renderBrief();
  if (t.type === "queue") { lastSig.queue = null; renderQueuePane(); }
  if (t.type === "needs") { lastSig.needs = null; renderNeedsPane(); }
  if (t.type === "money") renderMoney();
  if (t.type === "doc") renderDoc();
  renderChrome();
  if (t.type === "chat") $("#input").focus();
}

const openChat = (key) => openTab({ type: "chat", key });
const openManage = () => { lastSig.manage = null; openTab({ type: "manage" }); };
const openSettings = () => openTab({ type: "settings" });
const openBrief = () => openTab({ type: "brief" });
const openQueue = () => openTab({ type: "queue" });
const openMoney = () => openTab({ type: "money" });

// ================================================================ transcript

function renderCrumbs() {
  const c = $("#crumbs");
  c.textContent = "";
  const k = curKey();
  if (!k) return;
  const inf = info(k);
  if (!inf) return;
  c.append(h("span", null, baseName(S.state.workspace)), h("span", "sep", "›"), h("span", null, inf.isOrch ? "orchestrator" : inf.name));
  const cwd = h("span", "path lnk", inf.cwd);
  cwd.title = "Open folder in Explorer";
  cwd.onclick = () => openPath(inf.cwd, "reveal");
  c.append(h("span", "sep", "·"), cwd);
  c.append(h("span", "sp"));
  if (inf.estimate) c.append(estBadge(inf.estimate));
  c.append(h("span", "path", `${inf.turns || 0} turns · ${money(inf.cost_usd)}`));
  if (inf.isOrch && S.state.project?.brief) {
    const br = h("button", null, "📋 brief");
    br.title = "Project brief and rules";
    br.onclick = openBrief;
    c.append(br);
  }
  const ho = h("button", null, "⇄ handoff");
  ho.title = "Write a handoff and continue in a new session (after the current turn)";
  ho.onclick = () => handoff(k);
  c.append(ho);
  if (inf.isOrch) {
    const b = h("button", null, "new conversation");
    b.onclick = () => runCommand("/reset").catch(fail);
    c.append(b);
  }
  if (!inf.isOrch) {
    const rs = h("button", null, inf.resolved ? "↺ reopen" : "✓ resolve");
    rs.title = inf.resolved ? "Move back to the open threads" : "Done with this thread: tuck it away in Resolved";
    rs.onclick = () => setResolved(inf.name, !inf.resolved);
    c.append(rs);
  }
  if (!SOLO && !inf.isOrch) {
    const side = h("button", null, "⫽ to the side");
    side.title = "Open beside the orchestrator";
    side.onclick = () => { const k = curKey(); openChat(orchKey()); openSide(k); };
    c.append(side);
  }
  if (EMBEDDED) {
    const tab = h("button", "solo-btn", "open as tab");
    tab.onclick = () => window.parent.postMessage({ orch: "open-tab", key: curKey() }, location.origin);
    const pop = h("button", "solo-btn", "⧉ pop out");
    pop.onclick = () => window.parent.postMessage({ orch: "pop-out", key: curKey() }, location.origin);
    const x = h("button", "solo-btn", "✕");
    x.title = "Close side panel";
    x.onclick = () => window.parent.postMessage({ orch: "close-side" }, location.origin);
    c.append(tab, pop, x);
  } else if (!SOLO) {
    const pop = h("button", null, "⧉");
    pop.title = "Open in a new window";
    pop.onclick = () => popOut(curKey());
    c.append(pop);
  }
}

let logGen = 0;
async function loadLog(key) {
  const gen = ++logGen;
  // Live events that arrive while the history is loading are held and replayed after it.
  S.loading = { key, pending: [] };
  let events = [];
  try { events = await api("/api/events/" + encodeURIComponent(key)); } catch (e) { fail(e); }
  // A newer load (or a switch to another conversation) wins.
  if (gen !== logGen || curKey() !== key) return;
  const pending = S.loading?.key === key ? S.loading.pending : [];
  S.loading = null;
  events = events.concat(pending);
  const log = $("#log");
  log.textContent = "";
  S.toolEls = {};
  S.skipTools = new Set();
  S.seen = new Set();
  if (!events.length) welcome(key);
  for (const ev of events) renderEvent(ev);
  // Questions left open in the history but no longer waiting (app restarted, turn stopped).
  if (!info(key)?.needs_input) for (const q of $$(".ev.question:not(.done)")) markQuestion(q, null, true);
  updateWorking();
  scrollEnd(true);
}

function welcome(key) {
  const w = h("div", "welcome");
  if (isOrch(key)) {
    w.innerHTML = `<div class="w-title">✻ Orchestrator</div>
      <div class="w-sub">${esc(S.state.workspace)}</div>
      <div class="w-tips">
        <div>Describe what you want built; the orchestrator splits it into parallel threads.</div>
        <div>Open any thread in the sidebar to watch it and talk to it directly.</div>
        <div><code>/</code> for commands · <code>Ctrl+Shift+P</code> palette · <code>Ctrl+P</code> go to thread · <code>Esc</code> stop</div>
      </div>
      <div class="w-actions"><button class="btn" data-act="import">⇢ Import a Claude Code project…</button>
      ${S.state.project?.brief ? '<button class="btn" data-act="brief">📋 Project brief</button>' : ""}</div>`;
    setTimeout(() => {
      $('.welcome [data-act="import"]')?.addEventListener("click", importProject);
      $('.welcome [data-act="brief"]')?.addEventListener("click", openBrief);
    });
  } else {
    w.innerHTML = `<div class="w-title">› ${esc(key)}</div><div class="w-sub">${esc(info(key)?.cwd || "")}</div>
      <div class="w-tips"><div>No messages yet. Anything you type goes straight to this thread.</div></div>`;
  }
  $("#log").append(w);
}

function nearEnd() { const s = $("#scroller"); return s.scrollHeight - s.scrollTop - s.clientHeight < 140; }
function scrollEnd(force) { const s = $("#scroller"); if (force || nearEnd()) s.scrollTop = s.scrollHeight; }

function toolArg(name, input) {
  let o;
  try { o = JSON.parse(input); } catch { return input.slice(0, 120); }
  if (!o || typeof o !== "object") return String(o);
  if (name.endsWith("spawn_thread")) return `${o.name}${o.effort ? ", " + o.effort : ""}`;
  if (name.endsWith("send_to_thread")) return o.name;
  // These save a whole list; its first line is the same every time, so show its size
  // (the result lists what changed).
  if (name.endsWith("save_lessons")) {
    const n = (s) => (typeof s === "string" ? s.split("\n").filter((l) => l.trim()).length : null);
    const parts = [];
    if (n(o.lessons) !== null) parts.push(`${n(o.lessons)} lessons`);
    if (n(o.dev_lessons) !== null) parts.push(`${n(o.dev_lessons)} dev lessons`);
    if (n(o.business_lessons) !== null) parts.push(`${n(o.business_lessons)} business lessons`);
    if (n(o.project_lessons) !== null) parts.push(`${n(o.project_lessons)} project lessons${o.project ? " for " + o.project.split(/[\\/]/).pop() : ""}`);
    return parts.join(", ");
  }
  if (name.endsWith("save_tuning_notes") || name.endsWith("save_project_notes")) return "";
  for (const k of ["command", "file_path", "path", "pattern", "url", "query", "name", "description", "prompt"]) {
    if (typeof o[k] === "string") return o[k].split("\n")[0].slice(0, 140);
  }
  return JSON.stringify(o).slice(0, 120);
}
const toolName = (n) => n.replace(/^mcp__(app|threads)__/, "").replace(/^mcp__/, "");

function firstLines(text, n = 3) {
  const lines = (text || "").replace(/\s+$/, "").split("\n");
  const shown = lines.slice(0, n).join("\n");
  return lines.length > n ? `${shown}\n… +${lines.length - n} lines (click to expand)` : shown || "(no output)";
}

function attachmentView(paths) {
  const box = h("div", "thumbs");
  for (const p of paths || []) {
    if (IMG_EXT.test(p)) {
      const img = h("img");
      img.src = fileUrl(p);
      img.title = p;
      img.onclick = () => lightbox(img.src);
      box.append(img);
    } else {
      const f = h("span", "att-file lnk", "📄 " + baseName(p));
      f.title = p;
      f.onclick = () => openPath(p, "editor");
      box.append(f);
    }
  }
  return box;
}

function decorateText(e, text) {
  e.innerHTML = md(text);
  for (const pre of $$("pre", e)) {
    const b = h("button", "copy", "copy");
    b.onclick = (x) => { x.stopPropagation(); copy(pre.querySelector("code")?.textContent ?? pre.textContent); };
    pre.append(b);
  }
  for (const img of $$("img", e)) img.onclick = () => lightbox(img.src);
  const names = new Set((S.state?.threads || []).map((t) => t.name));
  for (const code of $$("code", e)) {
    if (!code.closest("pre") && names.has(code.textContent)) { code.classList.add("tlink"); code.dataset.thread = code.textContent; }
  }
  const c = h("button", "copy", "copy");
  c.onclick = () => copy(text);
  e.append(c);
}

function renderEvent(ev) {
  const log = $("#log");
  const sig = `${ev.ts}|${ev.kind}|${ev.id ?? ""}`;
  if (ev.ts && S.seen.has(sig)) return;
  S.seen.add(sig);
  if (ev.kind === "fork") return applyFork(ev.at);
  const before = log.lastElementChild;
  renderEventInner(ev, log);
  const last = log.lastElementChild;
  if (last && last !== before && !last.dataset.ts) last.dataset.ts = String(ev.ts);
}

/// An edit or retry branched the conversation: drop the replaced message and everything after it.
function applyFork(at) {
  const nodes = [...$("#log").children];
  const i = nodes.findIndex((n) => n.dataset.ts && +n.dataset.ts >= at - 1e-6);
  if (i === -1) return;
  for (const n of nodes.slice(i)) if (n.id !== "working" && n.id !== "draft") n.remove();
}

function renderEventInner(ev, log) {
  $(".welcome", log)?.remove();
  const k = ev.kind;
  if (k === "lessons_sorted") {
    const e = h("div", "ev user setup");
    e.append(h("span", "who", "📋 lessons sorted"), h("span", "sum", `${ev.text} Click to see every moved lesson.`));
    e.append(h("div", "body", (ev.moves || []).map((m) => `→ ${m.to}: ${m.lesson}`).join("\n") || "(nothing moved)"));
    e.onclick = () => e.classList.toggle("open");
    log.append(e);
    return;
  }
  if (k === "user") {
    if (ev.sender === "handoff" || ev.sender === "handoff-resume") {
      const req = ev.sender === "handoff";
      const e = h("div", "ev user setup");
      e.append(h("span", "who", req ? "⇄ handoff" : "⇄ new session"),
        h("span", "sum", req ? "Writing a handoff for the next session." : "Started from the handoff. Click to see it."));
      e.append(h("div", "body", ev.text));
      e.onclick = () => e.classList.toggle("open");
      e.dataset.ts = String(ev.ts);
      log.append(e);
      return;
    }
    if (ev.sender === "app") {
      const e = h("div", "ev user setup");
      e.append(h("span", "who", "↻ automatic"), h("span", "sum", "Sent by the app, not you: the usage limit reset, so it's carrying on. Click to see the message."));
      e.append(h("div", "body", ev.text));
      e.onclick = () => e.classList.toggle("open");
      e.dataset.ts = String(ev.ts);
      log.append(e);
      return;
    }
    if (ev.sender === "setup") {
      const e = h("div", "ev user setup");
      e.append(h("span", "who", "project import"), h("span", "sum", "Set up this project from its Claude Code history, carry over every rule, and continue unfinished work."));
      e.append(h("div", "body", ev.text));
      e.onclick = () => e.classList.toggle("open");
      e.title = "Click to see the full setup instructions";
      log.append(e);
      return;
    }
    const e = h("div", "ev user" + (ev.sender === "orchestrator" ? " from-orch" : "") + (ev.imported ? " imported" : ""));
    if (ev.sender === "orchestrator" && !isOrch(ev.thread)) e.append(h("span", "who", "orchestrator →"));
    if (ev.sender === "threads") {
      // A thread's report: one calm line, the full text on click.
      const first = (ev.text.split("\n").find((x) => x.trim() && !/^\[from [^\]]*\]$/.test(x.trim())) || "").replace(/\s+/g, " ").trim();
      const from = (ev.from || []).map((x) => x.replace(/^@/, "")).join(", ");
      const c = h("div", "ev user setup thread-report");
      c.append(h("span", "who", from ? "from " + from : "from threads"), h("span", "sum", first.length > 110 ? first.slice(0, 110) + "…" : first));
      c.append(h("div", "body", ev.text));
      c.onclick = () => c.classList.toggle("open");
      c.title = "Click to read the whole report";
      c.dataset.ts = String(ev.ts);
      log.append(c);
      return;
    }
    if (ev.imported) e.append(h("span", "who", "from Claude Code"));
    if (ev.injected) {
      const tag = h("span", "who injected", "↪ mid-turn");
      tag.title = "Sent while it was working; it read this at its next step";
      e.append(tag);
    }
    e.append(document.createTextNode(ev.text));
    if (ev.attachments?.length) e.append(attachmentView(ev.attachments));
    e.dataset.ts = String(ev.ts);
    e.dataset.text = ev.text;
    if ("resume_at" in ev) {
      const acts = h("div", "acts");
      const ed = h("button", null, "✎ edit");
      ed.title = "Edit and resend (the conversation branches from here)";
      ed.onclick = () => startEdit(e);
      const re = h("button", null, "↻ retry");
      re.title = "Run this message again from here";
      re.onclick = () => resend(ev.ts, ev.text);
      const cp = h("button", null, "copy");
      cp.onclick = () => copy(ev.text);
      acts.append(ed, re, cp);
      e.append(acts);
    }
    log.append(e);
  } else if (k === "text") {
    $("#draft")?.remove();
    const e = h("div", "ev text md" + (ev.sub ? " sub" : ""));
    decorateText(e, ev.text);
    log.append(e);
  } else if (k === "xws_request") {
    log.append(xwsRequestCard(ev));
  } else if (k === "xws_result") {
    xwsResult(ev, log);
  } else if (k === "question") {
    log.append(questionCard(ev));
  } else if (k === "answer") {
    const card = log.querySelector(`.ev.question[data-id="${ev.id}"]`);
    if (card) markQuestion(card, ev.answers, ev.cancelled);
  } else if (k === "media") {
    log.append(mediaView(ev));
  } else if (k === "plan") {
    // shown in the plan panel above the composer
  } else if (k === "tool_use" && APP_TOOLS.has(ev.name)) {
    S.skipTools.add(ev.id);
  } else if (k === "tool_result" && S.skipTools.has(ev.id)) {
    // result of an app tool, already shown as a widget
  } else if (k === "tool_use") {
    const e = h("div", "ev tool pending" + (ev.sub ? " sub" : ""));
    const call = h("div", "call");
    call.innerHTML = `<b>${esc(toolName(ev.name))}</b><span class="a">(${esc(toolArg(ev.name, ev.input))})</span>`;
    const tn = /(spawn_thread|send_to_thread|read_thread|configure_thread|stop_thread)$/.test(ev.name) && (() => { try { return JSON.parse(ev.input).name; } catch { return null; } })();
    if (tn) {
      const a = $(".a", call);
      a.innerHTML = `(<span class="tlink" data-thread="${esc(tn)}">${esc(tn)}</span>${esc(toolArg(ev.name, ev.input).slice(tn.length))})`;
    }
    const fp = filePathOf(ev.input);
    if (fp) {
      const a = $(".a", call);
      a.classList.add("lnk");
      a.title = "Open in editor";
      a.onclick = (x) => { x.stopPropagation(); openPath(fp, "editor"); };
    }
    const res = h("div", "res", "running…");
    const full = h("div", "full");
    full.append(h("div", "lbl", "input"), h("pre", null, ev.input));
    e.append(call, res, full);
    e.onclick = (x) => { if (!x.target.closest(".full")) e.classList.toggle("open"); };
    log.append(e);
    S.toolEls[ev.id] = e;
  } else if (k === "tool_result") {
    const e = S.toolEls[ev.id];
    if (!e) return;
    e.classList.remove("pending");
    if (ev.is_error) e.classList.add("err");
    $(".res", e).textContent = firstLines(ev.text);
    $(".full", e).append(h("div", "lbl", ev.is_error ? "error" : "output"), h("pre", null, ev.text || "(no output)"));
    if (ev.images?.length) e.append(attachmentView(ev.images));
  } else if (k === "result") {
    const secs = ev.duration_ms ? ` · ${(ev.duration_ms / 1000).toFixed(1)}s` : "";
    const e = h("div", "ev done" + (ev.is_error ? " err" : ""),
      `${ev.is_error ? (ev.subtype || "error") + " · " : ""}${ev.turns ?? "?"} steps${secs} · ${money(ev.cost)}`);
    const acts = h("span", "acts");
    const re = h("button", null, "↻ retry");
    re.title = "Run the last message again";
    re.onclick = () => {
      const users = $$("#log .ev.user[data-ts]");
      const u = users[users.length - 1];
      if (u) resend(+u.dataset.ts, u.dataset.text);
    };
    acts.append(re);
    e.append(acts);
    log.append(e);
  } else if (k === "handoff") {
    log.append(h("div", "ev handoff", "⇄ " + ev.text));
  } else if (k === "notice") {
    const n = h("div", "ev notice" + (ev.is_error || ev.interrupted ? " err" : ""), ev.text);
    if (ev.interrupted) {
      const acts = h("span", "acts");
      acts.style.display = "inline-flex";
      acts.style.marginLeft = "10px";
      const go = h("button", null, "↻ continue");
      go.title = "Pick up where it stopped";
      go.onclick = () => {
        acts.remove();
        ipcCall("send", { thread: ev.thread, text: "Continue where you left off. The app closed in the middle of your last turn, so check what was already done before redoing anything." }).catch(fail);
      };
      acts.append(go);
      n.append(acts);
    }
    log.append(n);
  }
  // only the newest result line keeps its retry button
  const dones = $$("#log .ev.done .acts");
  dones.slice(0, -1).forEach((a) => a.remove());
}

function filePathOf(input) {
  try {
    const o = JSON.parse(input);
    const p = o?.file_path || o?.notebook_path;
    return typeof p === "string" ? p : null;
  } catch { return null; }
}

// ---------------------------------------------------------------- first-run setup

const SETUP = { mode: null, step: "welcome", mix: 3, poll: null };
const LOGO = `<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="2.6"/><circle cx="5" cy="6" r="1.7"/><circle cx="19" cy="6" r="1.7"/><circle cx="5" cy="18" r="1.7"/><circle cx="19" cy="18" r="1.7"/><path d="M6.4 7.2 10 10.4M17.6 7.2 14 10.4M6.4 16.8 10 13.6M17.6 16.8 14 13.6"/></svg>`;

function openSetup() {
  SETUP.step = "welcome";
  $("#setup").classList.remove("hidden");
  drawSetup();
}

function closeSetup() {
  clearInterval(SETUP.poll);
  $("#setup").classList.add("hidden");
  loadState();
}

function setupSteps() {
  const s = ["welcome"];
  if (SETUP.mode === "plan" || SETUP.mode === "both") s.push("plan");
  if (SETUP.mode === "api" || SETUP.mode === "both") s.push("api");
  if (SETUP.mode === "both") s.push("mix");
  s.push("perms", "done");
  return s;
}

function setupNav(box, { next = "Continue", canNext = true, onNext } = {}) {
  const steps = setupSteps();
  const i = steps.indexOf(SETUP.step);
  const nav = h("div", "nav");
  if (i > 0) {
    const back = h("button", "btn", "Back");
    back.onclick = () => { SETUP.step = steps[i - 1]; drawSetup(); };
    nav.append(back);
  }
  const skip = h("button", "skip", "Skip setup");
  skip.title = "You can run it later from Settings";
  skip.onclick = async () => { await api("/api/settings", { body: { setup_done: true } }).catch(fail); closeSetup(); };
  nav.append(skip, h("span", "sp"));
  const go = h("button", "btn primary", next);
  go.disabled = !canNext;
  go.onclick = async () => {
    if (onNext && (await onNext()) === false) return;
    SETUP.step = steps[i + 1];
    drawSetup();
  };
  nav.append(go);
  box.append(nav);
}

async function drawSetup() {
  clearInterval(SETUP.poll);
  const box = $("#setup .setup");
  box.textContent = "";
  const steps = setupSteps();
  const bar = h("div", "steps");
  steps.forEach((s, i) => bar.append(h("span", i <= steps.indexOf(SETUP.step) ? "on" : "")));
  const logo = h("div", "logo");
  logo.innerHTML = LOGO;
  box.append(logo, bar);

  if (SETUP.step === "welcome") {
    box.append(h("h1", null, "Welcome to Orchestrator"));
    box.append(h("p", "lead", "An orchestrator agent that splits your work into Claude Code threads running in parallel. First, how do you want to pay for Claude? You can change this any time."));
    const opts = [
      ["plan", "◔", "My Claude plan", "Use your Pro or Max subscription. You'll sign in to Claude in your browser."],
      ["api", "⚿", "An API key", "Pay per use with Anthropic API credits from console.anthropic.com."],
      ["both", "⚖", "Both", "Use your plan and API credits together, and choose the split. API credits also pick up the work when your plan hits a usage limit."],
    ];
    for (const [mode, ic, title, desc] of opts) {
      const c = h("button", "choice" + (SETUP.mode === mode ? " sel" : ""));
      c.innerHTML = `<span class="ic">${ic}</span><span><b>${esc(title)}</b><span>${esc(desc)}</span></span>`;
      c.onclick = () => { SETUP.mode = mode; drawSetup(); };
      box.append(c);
    }
    setupNav(box, { canNext: !!SETUP.mode });
  }

  if (SETUP.step === "plan") {
    box.append(h("h1", null, "Sign in to your Claude plan"));
    box.append(h("p", "lead", "Orchestrator runs Claude Code, Anthropic's coding agent, and uses its sign-in."));
    const rows = h("div");
    box.append(rows);
    let st = null;
    const render = () => {
      rows.textContent = "";
      const row = (ok, title, desc, btn) => {
        const r = h("div", "check-row " + (ok ? "ok" : "bad"));
        r.innerHTML = `<span class="mk">${ok ? "✓" : "!"}</span><span class="txt"><b>${esc(title)}</b><span>${esc(desc)}</span></span>`;
        if (btn) r.append(btn);
        rows.append(r);
      };
      if (!st) { rows.append(h("div", "note", "Checking…")); return; }
      if (st.cli.found) row(true, "Claude Code is installed", st.cli.version || "");
      else {
        const b = h("button", "btn primary", "Install Claude Code");
        b.onclick = () => api("/api/setup/install-cli", { body: {} }).then(() => toast("Follow the installer window, then come back here")).catch(fail);
        row(false, "Claude Code isn't installed", "It installs in a minute from claude.ai and doesn't need admin rights.", b);
      }
      if (st.cli.found) {
        if (st.login.plan) row(true, "Signed in to Claude", [st.login.email, st.login.subscription && `${st.login.subscription} plan`].filter(Boolean).join(" · "));
        else {
          const b = h("button", "btn primary", "Sign in to Claude");
          b.onclick = () => api("/api/setup/login", { body: {} }).then(() => toast("Finish signing in in your browser")).catch(fail);
          row(false, st.login.method === "api_key" ? "Claude Code is using an API key, not a plan" : "Not signed in yet",
            "Opens a sign-in window and your browser. Use the account with your Claude plan.", b);
        }
      }
      const done = st.cli.found && st.login.plan;
      const navOld = $(".nav", box);
      if (navOld) navOld.remove();
      setupNav(box, { canNext: done, next: done ? "Continue" : "Waiting for sign-in…" });
    };
    const refresh = async () => { try { st = await api("/api/setup/status"); render(); } catch (e) { fail(e); } };
    render();
    await refresh();
    SETUP.poll = setInterval(refresh, 3000);
  }

  if (SETUP.step === "api") {
    box.append(h("h1", null, "Add your API key"));
    const lead = h("p", "lead");
    lead.innerHTML = `Create a key at <a href="https://console.anthropic.com/settings/keys" target="_blank" rel="noopener">console.anthropic.com</a> → API keys, then paste it here. It's stored only on this computer.`;
    box.append(lead);
    const keyL = h("label", null, "API key");
    const key = h("input");
    key.type = "password";
    key.placeholder = S.state.defaults.api_key_set ? "a key is saved; paste to replace" : "sk-ant-…";
    keyL.append(key);
    const det = h("details");
    det.innerHTML = "<summary>My key isn't scoped to a workspace</summary>";
    const wsL = h("label", null, "Workspace ID (only if the API asks for one; from the same organization as the key)");
    const ws = h("input");
    ws.type = "text";
    ws.placeholder = "wrkspc_…";
    wsL.append(ws);
    det.append(wsL);
    const result = h("div", "result");
    box.append(keyL, det, result);
    setupNav(box, {
      next: "Save and check",
      onNext: async () => {
        const body = {};
        if (key.value.trim()) body.api_key = key.value.trim();
        if (ws.value.trim()) body.api_workspace_id = ws.value.trim();
        if (!body.api_key && !S.state.defaults.api_key_set) { result.className = "result bad"; result.textContent = "Paste your API key first."; return false; }
        result.className = "result"; result.textContent = "Checking the key…";
        try {
          await saveSettings(body);
          await loadState();
          const r = await api("/api/auth-check", { body: {} });
          const a = r.results.find((x) => x.backend === "api");
          if (a?.ok) { result.className = "result ok"; result.textContent = "✓ The key works."; return true; }
          result.className = "result bad";
          result.textContent = "✕ " + (a?.detail || "The key didn't work.");
          return false;
        } catch (e) { result.className = "result bad"; result.textContent = "✕ " + e.message; return false; }
      },
    });
  }

  if (SETUP.step === "mix") {
    box.append(h("h1", null, "Split between plan and API"));
    box.append(h("p", "lead", "How much of the work should run on API credits? 0 keeps everything on your plan (API only when the plan hits a limit, from 1 up); 10 puts everything on the API. Each project can have its own setting later."));
    const ctl = mixControl(SETUP.mix, (v) => { SETUP.mix = v; });
    box.append(ctl);
    $("input[type=range]", ctl).oninput = (e) => { SETUP.mix = Number(e.target.value); $(".mix-val", ctl).textContent = `${SETUP.mix}/10`; };
    setupNav(box);
  }

  if (SETUP.step === "perms") {
    box.append(h("h1", null, "How much can threads do on their own?"));
    box.append(h("p", "lead", "Threads are Claude Code agents. Choose how often they should stop and ask you. You can change this per project or per thread later."));
    SETUP.perm = SETUP.perm || "acceptEdits";
    for (const [v, l, d] of PERMS) {
      const c = h("button", "choice" + (SETUP.perm === v ? " sel" : ""));
      c.innerHTML = `<span class="ic">${v === "bypassPermissions" ? "⚡" : v === "plan" ? "◇" : "🛡"}</span><span><b>${esc(l)}${v === "acceptEdits" ? " (recommended)" : ""}</b><span>${esc(d)}</span></span>`;
      c.onclick = () => { SETUP.perm = v; drawSetup(); };
      box.append(c);
    }
    setupNav(box);
  }

  if (SETUP.step === "done") {
    box.append(h("h1", null, "You're set up"));
    const lines = {
      plan: "Everything runs on your Claude plan.",
      api: "Everything runs on your API credits.",
      both: `About ${SETUP.mix}/10 of the work runs on API credits, the rest on your plan.`,
    };
    box.append(h("p", "lead", `${lines[SETUP.mode] || ""} Pick a project folder as your workspace (bottom of the window), or import a Claude Code project with /import, then tell the orchestrator what to build.`));
    const note = h("div", "note", `Permission level: ${permLabel(SETUP.perm)}. ${SETUP.perm === "bypassPermissions" ? "Threads can run commands and edit files in the workspace without asking, so choose folders you're comfortable with that." : "When a thread needs your OK, a card appears in its chat and the sidebar shows a ?."}`);
    box.append(note);
    setupNav(box, {
      next: "Start using Orchestrator",
      onNext: async () => {
        await api("/api/setup/finish", { body: { mode: SETUP.mode, mix: SETUP.mix } }).catch(fail);
        if (SETUP.perm) await ipcCall("permission", { level: SETUP.perm, default: true }).catch(fail);
        closeSetup();
        return false;
      },
    });
  }
}

// ---------------------------------------------------------------- plan/API mix

function mixControl(value, onCommit) {
  const box = h("div");
  const row = h("div", "mix-row");
  const r = h("input");
  r.type = "range";
  r.min = "0";
  r.max = "10";
  r.step = "1";
  r.value = String(value);
  const val = h("span", "mix-val", `${value}/10`);
  row.append(r, val);
  const ends = h("div", "mix-ends");
  ends.append(h("span", null, "0 · all plan"), h("span", null, "all API · 10"));
  const u = S.state.usage;
  const bar = h("div", "mix-bar");
  const fill = h("i");
  fill.style.width = `${Math.round(u.api_share * 100)}%`;
  bar.append(fill);
  const now = h("div", "mix-now", u.plan_cost + u.api_cost > 0
    ? `last 24h: ${Math.round(u.api_share * 100)}% API (${money(u.api_cost)} API · ${money(u.plan_cost)} plan)`
    : "no usage in the last 24h yet");
  box.append(row, ends, bar, now);
  if (!S.state.defaults.api_key_set && value > 0) box.append(h("div", "mix-warn", "No API key saved, so everything runs on the plan until you add one in Settings."));
  r.oninput = () => { val.textContent = `${r.value}/10`; };
  r.onchange = () => onCommit(Number(r.value));
  return box;
}

async function setMix(v) {
  await api("/api/mix", { body: { value: v } });
  await loadState();
  toast(`this project: ${mixLabel(v)}`);
}

function mixPopover(anchor) {
  const pop = $("#mixpop");
  if (!pop.classList.contains("hidden") && pop.dataset.kind === "mix") { pop.classList.add("hidden"); return; }
  pop.dataset.kind = "mix";
  pop.textContent = "";
  pop.append(h("div", "mix-title", "Plan / API mix"), h("div", "mix-sub", "For this whole project: every thread and the orchestrator."));
  pop.append(mixControl(S.state.usage.mix, (v) => setMix(v).catch(fail)));
  pop.append(h("div", "mix-title", "Plan usage"));
  pop.append(planUsageView(() => { pop.classList.add("hidden"); mixPopover(anchor); }));
  pop.classList.remove("hidden");
  const a = anchor.getBoundingClientRect();
  const w = 340;
  pop.style.left = Math.max(8, Math.min(innerWidth - w - 8, a.left)) + "px";
  pop.style.top = "";
  pop.style.bottom = (innerHeight - a.top + 6) + "px";
}

document.addEventListener("mousedown", (e) => {
  const pop = $("#mixpop");
  if (pop && !pop.classList.contains("hidden") && !pop.contains(e.target) && !e.target.closest(".chip, .sb")) pop.classList.add("hidden");
});

// ---------------------------------------------------------------- plan usage

function untilText(ts) {
  const s = ts - Date.now() / 1000;
  if (s <= 0) return "now";
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60);
  if (h >= 24) return new Date(ts * 1000).toLocaleString(undefined, { weekday: "short", hour: "numeric", minute: "2-digit" });
  return h ? `in ${h}h ${m}m` : `in ${m}m`;
}

function planUsageView(onRefreshed) {
  const box = h("div", "pu");
  const pu = S.state.plan_usage;
  const win = (label, w) => {
    if (!w) return;
    const pct = Math.round((w.utilization || 0) * 100);
    const row = h("div", "pu-row");
    row.innerHTML = `<span>${esc(label)}</span><span><b>${pct}%</b> used · resets ${esc(untilText(w.resetsAt))}</span>`;
    const bar = h("div", "pu-bar" + (pct >= 100 ? " full" : pct >= 80 ? " warn" : ""));
    const fill = h("i");
    fill.style.width = Math.min(100, pct) + "%";
    bar.append(fill);
    box.append(row, bar, h("div", null, ""));
    box.lastChild.style.height = "8px";
  };
  if (pu?.unifiedWindows) {
    win("Current 5-hour window", pu.unifiedWindows.five_hour);
    win("This week", pu.unifiedWindows.seven_day);
    for (const [k, w] of Object.entries(pu.unifiedWindows)) if (k !== "five_hour" && k !== "seven_day") win(k.replace(/_/g, " "), w);
  } else {
    box.append(h("div", "mix-now", "No plan usage seen yet. Refresh to check, or it appears after the next plan turn."));
  }
  const meta = h("div", "pu-meta");
  const ago = pu?.seen_at ? relTime(pu.seen_at) : "";
  meta.append(h("span", null, ago ? `as of ${ago === "now" ? "just now" : ago + " ago"}` : ""), h("span", "sp"));
  const btn = h("button", "btn sm", "Refresh");
  btn.title = "Check now (runs a one-word Haiku turn on your plan)";
  btn.onclick = async () => {
    btn.disabled = true;
    btn.textContent = "checking…";
    try { await api("/api/plan-usage/refresh", { body: {} }); await loadState(); onRefreshed?.(); }
    catch (e) { fail(e); btn.disabled = false; btn.textContent = "Refresh"; }
  };
  meta.append(btn);
  box.append(meta);
  return box;
}

function usagePopover(anchor) {
  const pop = $("#mixpop");
  if (!pop.classList.contains("hidden") && pop.dataset.kind === "usage") { pop.classList.add("hidden"); return; }
  pop.dataset.kind = "usage";
  const draw = () => {
    pop.textContent = "";
    pop.append(h("div", "mix-title", "Plan usage"), h("div", "mix-sub", "Your Claude plan's limits, as Claude Code reports them."));
    pop.append(planUsageView(draw));
  };
  draw();
  pop.classList.remove("hidden");
  const a = anchor.getBoundingClientRect();
  pop.style.left = Math.max(8, Math.min(innerWidth - 348, a.left)) + "px";
  pop.style.bottom = (innerHeight - a.top + 6) + "px";
}

// ---------------------------------------------------------------- import & project brief

async function importProject() {
  let projects;
  try { projects = (await api("/api/claude-projects")).projects; } catch (e) { return fail(e); }
  if (!projects.length) return toast("No Claude Code projects found in ~/.claude/projects", true);
  const items = projects.map((p) => ({
    label: baseName(p.path),
    desc: `${p.sessions} conversations · ${relTime(p.last_active)} ago${p.imported ? " · imported before" : ""} · ${p.path}`,
    value: p.path,
    current: p.path.toLowerCase() === S.state.workspace.toLowerCase(),
  }));
  const it = await quickPick(items, "Import which Claude Code project? Orchestrator sets it up and continues the work");
  if (!it) return;
  try {
    await api("/api/import", { body: { path: it.value } });
    S.tabs = [];
    await loadState();
    await activate(0);
    toast(`importing ${baseName(it.value)} — the orchestrator is setting it up`);
  } catch (e) { fail(e); }
}

function renderBrief() {
  const pane = $("#pane-brief");
  pane.textContent = "";
  const page = h("div", "page brief-page");
  const p = S.state.project || {};
  page.append(h("h1", null, "Project brief"), h("div", "sub", S.state.workspace + (p.updated_at ? ` · updated ${relTime(p.updated_at)} ago` : "")));
  const biz = p.kind === "business";
  const scope = p.lesson_scope || "";
  const names = { lessons: "lessons about you", [biz ? "business_lessons" : "dev_lessons"]: biz ? "business lessons" : "development lessons", project_lessons: "lessons for this project" };
  const reaches = (f) => f === "project_lessons" || (scope !== "none" && (f === "lessons" || (scope === "" && f === (biz ? "business_lessons" : "dev_lessons"))));
  const section = (title, field, desc) => {
    const off = ["lessons", "dev_lessons", "business_lessons"].includes(field) && !reaches(field);
    page.append(h("div", "set-section", title + (off ? " (doesn't reach this workspace)" : "")));
    page.append(h("div", "sub", desc));
    const view = h("div", "md");
    view.innerHTML = md(p[field] || "_(empty)_");
    const ta = h("textarea");
    ta.value = p[field] || "";
    ta.classList.add("hidden");
    const row = h("div", "row");
    const edit = h("button", "btn", "Edit");
    const saveB = h("button", "btn primary hidden", "Save");
    edit.onclick = () => { view.classList.toggle("hidden"); ta.classList.toggle("hidden"); saveB.classList.toggle("hidden"); edit.textContent = ta.classList.contains("hidden") ? "Edit" : "Cancel"; };
    saveB.onclick = async () => {
      if (field === "tuning") await api("/api/tuning", { body: { notes: ta.value } }).catch(fail);
      else if (["lessons", "dev_lessons", "business_lessons", "project_lessons"].includes(field)) await api("/api/lessons", { body: { [field]: ta.value } }).catch(fail);
      else await api("/api/notes", { body: { [field]: ta.value } }).catch(fail);
      await loadState();
      renderBrief();
      toast("saved");
    };
    row.append(edit, saveB);
    page.append(view, ta, row);
  };
  page.append(h("div", "sub", "Reaching this workspace's orchestrator: " + Object.keys(names).filter(reaches).map((f) => names[f]).join(", ") + "." +
    (scope ? " (Overridden in Settings → Workspace → Shared lessons.)" : "") +
    (p.lessons_scoped ? "" : " The shared lessons haven't been sorted by type yet; that happens on the next message in a development workspace.") +
    " To move a lesson, cut it from one list and paste it into another."));
  section("Lessons about you", "lessons", "About you as a person: how you talk, what you want reported, privacy, your setup. Every workspace gets these. The orchestrator updates them itself; you can edit them too.");
  if (biz) section("Business lessons", "business_lessons", "How you like business work done. Only business workspaces get these.");
  else section("Development lessons", "dev_lessons", "How you like development work done: models and reviews, testing, QA, deploys, git. Only development workspaces get these.");
  section("Lessons for this project", "project_lessons", "Lessons that only matter in this workspace.");
  section("Rules", "rules", "Added to every thread's instructions in this workspace.");
  section("Tuning notes", "tuning", "The orchestrator's own lessons about which model and effort fit which work in this project. It updates these from the efficiency stats; you can edit them too.");
  section("Brief", "brief", "Shown to the orchestrator in every conversation in this workspace.");
  if (biz) section("Development lessons", "dev_lessons", "How you like development work done. Only development workspaces get these.");
  else section("Business lessons", "business_lessons", "How you like business work done. Only business workspaces get these.");
  pane.append(page);
}

// ---------------------------------------------------------------- approval queue

// Decisions go to the app over the window's private IPC channel, never over HTTP:
// agents can reach the HTTP API, so it can't be what approves things.
// A pane's heading, with "⧉ Pop out" (opens the pane in its own window) outside pane windows.
function paneTitle(text, pane) {
  const t = h("h1", "pane-title", text);
  if (!PANE) {
    const b = h("button", "btn popout-btn", "⧉ Pop out");
    b.title = "Open this in its own window (it stays live; clicking again brings it to the front)";
    b.onclick = () => sendIpc({ op: "pop_out_pane", pane });
    t.append(b);
  }
  return t;
}

function sendIpc(msg) {
  if (!window.ipc?.postMessage) { toast("This only works in the Orchestrator window, not in a browser", true); return false; }
  window.ipc.postMessage(JSON.stringify(msg));
  return true;
}

function renderQueuePane() {
  const pane = $("#pane-queue");
  const q = S.state.queue;
  // Don't wipe a card the user is editing or rejecting.
  if ($(".qcard.editing", pane)) return;
  if (!changed("queue", [q, S.state.workspace, S.state.project?.outbound_paused, S.state.project?.paused, S.state.needs, SW.active && SW.pane === "queue", SW.rev]) && $(".page", pane)) return;
  if (SW.active && SW.pane === "queue" && q) { renderSweep(pane); return; }
  const keep = scrollState(pane);
  pane.textContent = "";
  const page = h("div", "page queue-page");
  page.append(paneTitle("Approval queue", "queue"));
  if (!q) {
    page.append(h("div", "sub", "The approval queue is only on in business workspaces (Settings → Workspace type)."));
    pane.append(page);
    return;
  }
  page.append(h("div", "sub", `${S.state.workspace} · nothing leaves this computer until you approve it here`));
  const pj = S.state.project || {};
  {
    const ctl = h("div", "row qbar");
    if (pj.outbound_paused) page.append(h("div", "qbanner", pj.outbound_paused_all
      ? "⏸ Outbound is paused for all business workspaces (tray menu). Agents get no approved items until you resume."
      : "⏸ Outbound is paused. Agents get no approved items until you resume."));
    if (pj.paused) page.append(h("div", "qbanner", "⏸ This workspace is paused. Nothing runs unless you message it; thread news and automatic turns wait until you resume."));
    if (!pj.outbound_paused_all) {
      const ob = h("button", "btn" + (pj.outbound_paused ? " primary" : ""), pj.outbound_paused ? "Resume outbound" : "⏸ Pause outbound");
      ob.title = "Kill switch: while paused, agents can't get approved items to execute";
      ob.onclick = () => { if (sendIpc({ op: "set_outbound_paused", paused: !pj.outbound_paused })) ob.disabled = true; };
      ctl.append(ob);
    }
    const wb = h("button", "btn" + (pj.paused ? " primary" : " danger"), pj.paused ? "Resume workspace" : "Pause workspace");
    wb.title = pj.paused ? "Let this workspace's orchestrator and threads run again" : "Stop every running turn here and hold automatic work until you resume";
    wb.onclick = () => {
      if (!pj.paused && !confirm("Pause this workspace? Every running turn here stops now, and thread news and automatic turns wait until you resume.")) return;
      if (sendIpc({ op: "set_workspace_paused", paused: !pj.paused })) wb.disabled = true;
    };
    ctl.append(wb);
    page.append(ctl);
  }
  const sweep = swButton("queue");
  if (sweep) { const sb = h("div", "row qbar"); sb.append(sweep); page.append(sb); }
  const low = q.pending.filter((i) => i.risk === "low" && !i.manual && !(i.capacity && !i.capacity.ok));
  if (low.length > 1) {
    const b = h("button", "btn", `Approve all low-risk (${low.length})`);
    b.onclick = () => {
      if (!confirm(`Approve all ${low.length} low-risk items?\n\n` + low.map((i) => `• ${i.id}: ${i.summary}`).join("\n"))) return;
      if (sendIpc({ op: "queue_approve_low_risk", items: low.map((i) => ({ id: i.id, hash: i.payload_hash })) })) b.disabled = true;
    };
    const bar = h("div", "row qbar");
    bar.append(b);
    page.append(bar);
  }
  page.append(inboxBox());
  page.append(h("div", "set-section", `Waiting for you · ${q.pending.length}`));
  if (!q.pending.length) page.append(h("div", "sub", "Nothing waiting."));
  for (const i of q.pending) page.append(queueCard(i));
  if (q.decided.length) {
    page.append(h("div", "set-section", "Recent decisions"));
    for (const i of q.decided) {
      const r = h("div", "qrow");
      r.append(h("span", "qchip st-" + i.status, i.status + (i.edited ? " (edited)" : "")), h("span", "qid", i.id), h("span", "qsum", `${i.type} · ${i.summary}`));
      if (i.void || i.status === "void") { const v = h("span", "qchip risk-high", "changed after approval"); v.title = "The text or an attachment changed after you approved it, so it won't be executed."; r.insertBefore(v, r.children[1]); }
      const more = i.status === "rejected" ? i.note : i.status === "done" ? i.result : i.note;
      if (more) r.append(h("div", "qmore", more));
      r.title = `${i.id} · ${i.type} · line ${i.line || "-"}\nby ${i.created_by}`;
      r.dataset.anchor = i.id;
      page.append(r);
    }
  }
  pane.append(page);
  restoreScroll(pane, keep);
}

// Put text on the clipboard: in the app window the app writes it (the browser clipboard
// API is refused when the page isn't focused); in a browser, the clipboard API.
async function copyText(text) {
  if (window.ipc?.postMessage) { window.ipc.postMessage(JSON.stringify({ op: "copy", text })); return true; }
  try { await navigator.clipboard.writeText(text); return true; } catch (e) { fail(e); return false; }
}

// A manual task (done by the user in another app): each prompt with a Copy button, and a
// drop zone that saves files to inbox/images/<item-id>/.
function manualTask(i) {
  const box = h("div", "manual");
  const text = i.payload || "";
  const fenced = [...text.matchAll(/```[^\n]*\n([\s\S]*?)```/g)].map((m) => m[1].trim());
  const prompts = (fenced.length ? fenced : text.split(/\n\s*\n/).map((p) => p.trim())).filter(Boolean);
  if (prompts.length > 1 || fenced.length) {
    prompts.forEach((p, n) => {
      const row = h("div", "prompt-row");
      const copy = h("button", "btn", "Copy");
      copy.onclick = async () => { if (await copyText(p)) { copy.textContent = "Copied"; setTimeout(() => (copy.textContent = "Copy"), 1200); } };
      row.append(h("span", "qid", `#${n + 1}`), h("pre", "prompt", p), copy);
      box.append(row);
    });
  } else {
    const copy = h("button", "btn", "Copy the text");
    copy.onclick = async () => { if (await copyText(text)) { copy.textContent = "Copied"; setTimeout(() => (copy.textContent = "Copy the text"), 1200); } };
    box.append(copy);
  }
  const drop = h("div", "drop");
  const list = () => {
    drop.textContent = "";
    drop.append(h("div", "sub", `Drop the files here: they're saved to inbox/images/${i.id}/`));
    for (const f of i.dropped || []) {
      if (/\.(png|jpe?g|gif|webp)$/i.test(f)) {
        const img = h("img");
        img.src = fileUrl(`${i.drop_dir}\\${f}`);
        img.title = f;
        img.onclick = () => lightbox(img.src);
        drop.append(img);
      } else drop.append(h("span", "qchip", f));
    }
  };
  list();
  drop.ondragover = (e) => { e.preventDefault(); e.stopPropagation(); drop.classList.add("over"); };
  drop.ondragleave = () => drop.classList.remove("over");
  drop.ondrop = async (e) => {
    e.preventDefault(); e.stopPropagation(); drop.classList.remove("over");
    for (const f of e.dataTransfer.files) {
      const data = await new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(String(r.result).split(",")[1] || ""); r.onerror = rej; r.readAsDataURL(f); });
      try { await api("/api/queue/drop", { body: { id: i.id, name: f.name, data } }); } catch (err) { fail(err); }
    }
    lastSig.queue = null;
    await loadState();
  };
  box.append(drop);
  return box;
}

// ---------------------------------------------------------------- needs you
// Every change goes over the window's private IPC (sendIpc), never HTTP. `need` takes the
// entry's workspace (`ws`) so other open workspaces can be acted on in place; decisions
// (queue_decide) only work in the shown workspace, so those cards open the workspace first.
const ND = { all: false, later: false, done: false, more: new Set(), menu: null, dismiss: null, reject: null, player: null, draft: {} };
const ND_KIND = { question: "Question", decision: "Decision", manual: "Do by hand", action: "To do", heads_up: "Heads-up", message: "Message" };
const ndSameWs = (v) => !v.ws || v.ws.toLowerCase() === (S.state.workspace || "").toLowerCase();
const ndCan = (v) => ndSameWs(v) || v.kind !== "decision";
function needIpc(v, msg) { return sendIpc(ndSameWs(v) ? { op: "need", ...msg } : { op: "need", ws: v.ws, ...msg }); }
function ndFind(id) {
  const groups = [S.state.needs, ...(S.state.needs_all?.workspaces || [])];
  for (const g of groups) for (const v of [...(g?.open || []), ...(g?.snoozed || [])]) if (v.id === id) return v;
  return null;
}
const ndRefresh = () => { lastSig.needs = null; renderNeedsPane(true); };

function ndSteps(v) { return v.plan?.steps || []; }
function ndPaused(v) { const n = ndSteps(v).length; return n && v.step > 0 && v.step < n ? `paused at step ${v.step + 1} of ${n}` : ""; }

function ndJump(v) {
  const go = async () => {
    await openChat(v.source && !isOrch(v.source) ? v.source : orchKey());
    const nodes = $$("#log [data-ts]");
    const t = nodes.find((x) => +x.dataset.ts >= v.msg_ts - 1e-6) || nodes[nodes.length - 1];
    if (t) { t.scrollIntoView({ block: "center" }); t.classList.add("flash"); setTimeout(() => t.classList.remove("flash"), 2000); }
  };
  if (!ndSameWs(v)) switchWorkspace(v.ws).then(go); else go();
}

function ndChips(v, showWs) {
  const row = h("div", "nd-chips");
  row.append(h("span", "qchip", ND_KIND[v.kind] || v.kind));
  if (v.priority === "urgent") row.append(h("span", "qchip urgent", "urgent"));
  if (v.minutes) row.append(h("span", "qchip", `~${v.minutes} min`));
  if (v.pin === 1) row.append(h("span", "qchip you", "📌 pinned"));
  if (v.careful) row.append(h("span", "qchip risk-high", "check carefully"));
  if (v.kind === "question" && v.waiting) row.append(h("span", "qchip you", "agent is waiting"));
  const p = ndPaused(v);
  if (p) row.append(h("span", "qchip", p));
  if (showWs) row.append(h("span", "qchip", baseName(v.ws)));
  return row;
}

function ndQuestion(v) {
  const box = h("div", "nd-q");
  const qs = Array.isArray(v.questions) ? v.questions : [];
  const wrap = h("div", "question");
  const send = () => {
    const answers = $$(".q", wrap).map((b) => ({ selected: $$(".q-opt.sel", b).map((x) => x.dataset.label), other: $(".q-other", b).value.trim() }));
    if (answers.some((a) => !a.selected.length && !a.other)) return toast("Answer each question first", true);
    if (needIpc(v, { action: "answer", id: v.id, answers })) $$("button", wrap).forEach((b) => { b.disabled = true; });
  };
  qs.forEach((q) => {
    const multi = !!q.multi_select;
    const b = h("div", "q" + (multi ? " multi" : ""));
    const title = h("div", "q-title");
    if (q.header) title.append(h("span", "q-tag", q.header));
    title.append(document.createTextNode(q.question || ""));
    b.append(title);
    const opts = h("div", "q-opts");
    (q.options || []).forEach((o) => {
      const btn = h("button", "q-opt");
      btn.dataset.label = o.label;
      const t = h("span");
      t.append(h("div", "ol", o.label));
      if (o.description) t.append(h("div", "od", o.description));
      btn.append(h("span", "box"), t);
      btn.onclick = () => {
        if (multi) btn.classList.toggle("sel");
        else { $$(".q-opt", opts).forEach((x) => x.classList.toggle("sel", x === btn)); $(".q-other", b).value = ""; if (qs.length === 1) send(); }
      };
      opts.append(btn);
    });
    b.append(opts);
    const other = h("input", "q-other");
    other.placeholder = "Or type your own answer…";
    other.onkeydown = (e) => { if (e.key === "Enter") send(); };
    b.append(other);
    wrap.append(b);
  });
  const sb = h("button", "btn primary", "Send answer");
  sb.onclick = send;
  const row = h("div", "nd-acts");
  row.append(sb);
  wrap.append(row);
  box.append(wrap);
  return box;
}

function ndDecision(v, acts, c) {
  const q = v.queue || {};
  const overCap = q.capacity && !q.capacity.ok;
  if (q.capacity) c.append(h("div", "nd-note", (q.capacity.ok ? "✓ " : "✗ ") + q.capacity.note));
  if (v.careful || ND.more.has(v.id)) c.append(h("pre", "qpayload", q.payload || ""));
  if (v.kind === "manual") {
    const cp = h("button", "btn", "Copy the text");
    cp.onclick = async () => { if (await copyText(q.payload || "")) toast("Copied"); };
    const done = h("button", "btn primary", "I did it");
    done.onclick = () => { if (needIpc(v, { action: "done", id: v.id })) done.disabled = true; };
    acts.append(done, cp);
    return;
  }
  const rec = v.recommend;
  const approve = h("button", "btn" + (rec === "approve" && !v.careful ? " primary" : ""), overCap ? "Approve anyway…" : "Approve");
  approve.onclick = () => {
    if (overCap && !confirm(`This might not fit in your usage:\n\n${q.capacity.note}\n\nApprove anyway?`)) return;
    if (v.careful && !confirm("You are approving exactly what is shown above. Go ahead?")) return;
    if (sendIpc({ op: "queue_decide", id: v.queue_item, action: "approve", force: !!overCap, hash: q.payload_hash })) $$("button", acts).forEach((b) => { b.disabled = true; });
  };
  const reject = h("button", "btn" + (rec === "reject" ? " primary" : " danger"), "Reject…");
  reject.onclick = () => { ND.reject = v.id; ndRefresh(); };
  const edit = h("button", "btn", "Edit…");
  edit.title = "Edit it in the Queue";
  edit.onclick = openQueue;
  acts.append(approve, reject, edit);
}

function ndSnoozeMenu(v) {
  const m = h("div", "nd-menu");
  m.append(h("span", "sub", "Ask me again:"));
  for (const [l, u] of [["in 1 hour", "1h"], ["tonight", "tonight"], ["tomorrow", "tomorrow"]]) {
    const b = h("button", "btn", l);
    b.onclick = () => { ND.menu = null; needIpc(v, { action: "snooze", id: v.id, until: u }); ndRefresh(); };
    m.append(b);
  }
  const x = h("button", "btn quiet", "cancel");
  x.onclick = () => { ND.menu = null; ndRefresh(); };
  m.append(x);
  return m;
}

function ndInputRow(v, key, ph, label, onGo, required) {
  const m = h("div", "nd-menu");
  const inp = h("input", "nd-in");
  inp.placeholder = ph;
  inp.value = ND.draft[key + v.id] || "";
  inp.oninput = () => { ND.draft[key + v.id] = inp.value; };
  const ok = h("button", "btn danger", label);
  const cancel = h("button", "btn quiet", "cancel");
  const go = () => { if (required && !inp.value.trim()) return toast("Add a short reason", true); onGo(inp.value.trim()); delete ND.draft[key + v.id]; };
  ok.onclick = go;
  inp.onkeydown = (e) => { if (e.key === "Enter") go(); if (e.key === "Escape") cancel.onclick(); };
  cancel.onclick = () => { ND.dismiss = ND.reject = null; ndRefresh(); };
  m.append(inp, ok, cancel);
  setTimeout(() => inp.focus());
  return m;
}

function needCard(v, o = {}) {
  const c = h("div", "ndcard" + (v.priority === "urgent" ? " urgent" : "") + (v.careful ? " careful" : ""));
  c.dataset.anchor = v.id;
  c.append(ndChips(v, o.showWs));
  c.append(h("div", "nd-tldr", v.tldr || v.title));
  if (v.recommend && v.kind !== "question") {
    const r = h("div", "nd-rec");
    r.append(document.createTextNode("I'd suggest: "), h("b", null, ({ approve: "approve it", reject: "reject it", edit: "edit it first", do: "do it", skip: "skip it" })[v.recommend] || v.recommend));
    c.append(r);
  }
  if (v.kind === "message" && v.xws) {
    if (!ndSameWs(v)) { const b = h("button", "btn primary", "Open " + baseName(v.ws) + " to decide"); b.onclick = () => switchWorkspace(v.ws).then(() => openNeeds()); c.append(b); return c; }
    c.append(xwsBlock(v.xws, v.id));
    if (v.msg_ts) { const j = h("button", "btn quiet", "💬 Chat"); j.onclick = () => ndJump(v); c.append(j); }
    return c;
  }
  if (v.why) c.append(h("div", "nd-why", v.why));
  if (!ndCan(v)) {
    const acts = h("div", "nd-acts");
    const b = h("button", "btn primary", "Open " + baseName(v.ws) + " to decide");
    b.onclick = () => switchWorkspace(v.ws).then(() => openNeeds());
    acts.append(b);
    c.append(acts);
    return c;
  }
  if (v.kind === "question" && v.timed_out) c.append(h("div", "nd-note", `The agent didn't wait and went ahead with “${v.default_answer}”. Answer to tell it what you really want.`));
  const acts = h("div", "nd-acts");
  if (v.kind === "question") c.append(ndQuestion(v));
  else if (v.kind === "decision" || v.kind === "manual") ndDecision(v, acts, c);
  else if (v.kind === "action") {
    if (v.plan) {
      const n = ndSteps(v).length;
      const b = h("button", "btn primary", v.step > 0 ? `Continue at step ${Math.min(v.step, n - 1) + 1} of ${n}` : `Start · ${n} step${n === 1 ? "" : "s"}`);
      b.onclick = () => ndStart(v);
      acts.append(b);
    } else {
      if (v.steps) c.append(h("div", "nd-more", v.steps));
      if (v.link) { const lb = h("button", "btn", "Open"); lb.onclick = () => (/^https?:/i.test(v.link) ? openUrl(v.link) : openPath(v.link, "default")); acts.append(lb); }
    }
    const d = h("button", "btn" + (v.plan ? "" : " primary"), "I did it");
    d.onclick = () => { if (needIpc(v, { action: "done", id: v.id })) d.disabled = true; };
    acts.append(d);
  } else if (v.kind === "heads_up") {
    const g = h("button", "btn primary", "Got it");
    g.onclick = () => { if (needIpc(v, { action: "done", id: v.id })) g.disabled = true; };
    acts.append(g);
  }
  acts.append(h("span", "sp"));
  const sn = h("button", "btn quiet", "💤 Later");
  sn.onclick = () => { ND.menu = ND.menu === v.id ? null : v.id; ndRefresh(); };
  acts.append(sn);
  const more = h("div", "nd-acts");
  const pin = h("button", "btn quiet", v.pin === 1 ? "📌 Unpin" : "📌 Pin to Today");
  pin.onclick = () => needIpc(v, { action: "pin", id: v.id, pin: v.pin === 1 ? 0 : 1 });
  more.append(pin);
  if (o.today && v.pin !== 1) {
    const nt = h("button", "btn quiet", "Not today");
    nt.onclick = () => needIpc(v, { action: "pin", id: v.id, pin: -1 });
    more.append(nt);
  } else if (v.pin === -1) {
    const bk = h("button", "btn quiet", "Put back");
    bk.onclick = () => needIpc(v, { action: "pin", id: v.id, pin: 0 });
    more.append(bk);
  }
  if (v.kind !== "decision" && v.kind !== "manual") {
    const ds = h("button", "btn quiet", "Dismiss…");
    ds.onclick = () => { ND.dismiss = v.id; ndRefresh(); };
    more.append(ds);
  }
  if (v.msg_ts) { const j = h("button", "btn quiet", "💬 Chat"); j.title = "Jump to where this came up"; j.onclick = () => ndJump(v); more.append(j); }
  const mb = h("button", "btn quiet", ND.more.has(v.id) ? "Less" : "More");
  mb.onclick = () => { ND.more.has(v.id) ? ND.more.delete(v.id) : ND.more.add(v.id); ndRefresh(); };
  acts.append(mb);
  c.append(acts);
  if (ND.more.has(v.id)) c.append(more);
  if (ND.menu === v.id) c.append(ndSnoozeMenu(v));
  if (ND.dismiss === v.id) c.append(ndInputRow(v, "d", "Why? (optional, the agent sees it)", "Dismiss", (note) => { ND.dismiss = null; needIpc(v, { action: "dismiss", id: v.id, note }); ndRefresh(); }, false));
  if (ND.reject === v.id) c.append(ndInputRow(v, "r", "Why? (one line: how the agents learn your taste)", "Reject", (reason) => { ND.reject = null; sendIpc({ op: "queue_decide", id: v.queue_item, action: "reject", reason }); ndRefresh(); }, true));
  if (ND.more.has(v.id)) {
    const parts = [];
    if (v.details) parts.push(v.details);
    if (v.steps && v.plan) parts.push(v.steps);
    if (v.created_at) parts.push(`Came up ${relTime(v.created_at) === "now" ? "just now" : relTime(v.created_at) + " ago"}${v.source ? " in " + (isOrch(v.source) ? "the orchestrator" : v.source) : ""}.`);
    c.append(h("div", "nd-more", parts.join("\n\n")));
  }
  return c;
}

// ---- step player
function ndStart(v) {
  const n = ndSteps(v).length;
  ND.player = { id: v.id, i: Math.min(v.step || 0, Math.max(0, n - 1)), intro: !(v.step > 0), stuck: false, finish: false };
  ndRefresh();
}

function renderNeedsPlayer(pane) {
  const P = ND.player;
  const v = ndFind(P.id);
  if (!v || !v.plan) { ND.player = null; return renderNeedsPane(true); }
  pane.textContent = "";
  const page = h("div", "page");
  const box = h("div", "nd-player");
  page.append(box);
  pane.append(page);
  const steps = ndSteps(v), n = steps.length, plan = v.plan;
  const exit = () => { ND.player = null; ndRefresh(); };
  box.append(h("div", "nd-pstep", v.tldr || v.title));
  const bar = h("div", "nd-bar");
  const fill = h("i");
  fill.style.width = (P.intro ? 0 : Math.min(100, ((P.finish ? n : P.i) / n) * 100)) + "%";
  bar.append(fill);
  box.append(bar);
  if (P.intro) {
    box.append(h("div", "nd-pdo", plan.goal || v.title));
    if (plan.before_you_start) box.append(h("div", "nd-pexp", "Before you start: " + plan.before_you_start));
    box.append(h("div", "nd-pstep", `${n} step${n === 1 ? "" : "s"}${v.minutes ? " · about " + v.minutes + " min" : ""}. You can stop any time and pick up again.`));
    const nav = h("div", "nd-pnav");
    const go = h("button", "btn primary", "Start");
    go.onclick = () => { P.intro = false; ndRefresh(); };
    const x = h("button", "btn quiet", "Not now");
    x.onclick = exit;
    nav.append(go, x);
    box.append(nav);
  } else if (P.stuck) {
    const step = Math.min(P.i, n - 1);
    box.append(h("div", "nd-pdo", `Stuck at step ${step + 1}`));
    box.append(h("div", "nd-stuck", plan.if_stuck ? `Try this: ${plan.if_stuck}` : "No problem. Tell the agent what happened and it will help."));
    const note = h("textarea");
    note.placeholder = "What happened? (this goes to the agent)";
    note.value = ND.draft["s" + v.id] || "";
    note.oninput = () => { ND.draft["s" + v.id] = note.value; };
    box.append(note);
    const nav = h("div", "nd-pnav");
    const send = h("button", "btn primary", "Tell the agent");
    send.onclick = () => {
      if (needIpc(v, { action: "stuck", id: v.id, step, note: note.value.trim() })) { toast("Sent. The agent will help."); delete ND.draft["s" + v.id]; ND.player = null; ndRefresh(); }
    };
    const back = h("button", "btn", "Back to the step");
    back.onclick = () => { P.stuck = false; ndRefresh(); };
    nav.append(send, back);
    box.append(nav);
  } else if (P.finish) {
    box.append(h("div", "nd-pdo", "That's all the steps."));
    if (plan.done_when) box.append(h("div", "nd-pexp", "You're done when: " + plan.done_when));
    box.append(h("div", "nd-pdo", "Did you see that?"));
    const nav = h("div", "nd-pnav");
    const yes = h("button", "btn primary", "Yes, it worked");
    yes.onclick = () => { if (needIpc(v, { action: "done", id: v.id })) { ND.player = null; toast("Done. The agent knows."); ndRefresh(); } };
    const no = h("button", "btn", "No");
    no.onclick = () => { P.stuck = true; P.finish = false; P.i = n - 1; ndRefresh(); };
    const back = h("button", "btn quiet", "← Back");
    back.onclick = () => { P.finish = false; P.i = n - 1; ndRefresh(); };
    nav.append(yes, no, back);
    box.append(nav);
  } else {
    const s = steps[P.i];
    const per = Math.max(1, Math.round((v.minutes || n) / n));
    box.append(h("div", "nd-pstep", `Step ${P.i + 1} of ${n} · ~${per} min`));
    box.append(h("div", "nd-pdo", s.do));
    const btns = h("div", "nd-pbtns");
    if (s.copy) { const cb = h("button", "btn", "📋 Copy"); cb.onclick = async () => { if (await copyText(s.copy)) toast("Copied"); }; btns.append(cb); }
    if (s.url) { const ob = h("button", "btn", "↗ Open"); ob.onclick = () => (/^https?:/i.test(s.url) ? openUrl(s.url) : openPath(s.url, "default")); btns.append(ob); }
    if (btns.children.length) box.append(btns);
    if (s.user_types_this) box.append(h("div", "nd-pstep", "You type this part yourself. It is never saved or sent."));
    if (s.expect) { const e = h("div", "nd-pexp"); e.append(h("b", null, "You should see: "), document.createTextNode(s.expect)); box.append(e); }
    const nav = h("div", "nd-pnav");
    const next = h("button", "btn primary", P.i === n - 1 ? "Finish" : "Next →");
    next.onclick = () => {
      if (P.i === n - 1) { P.finish = true; needIpc(v, { action: "step", id: v.id, step: n - 1 }); }
      else { P.i++; needIpc(v, { action: "step", id: v.id, step: P.i }); }
      ndRefresh();
    };
    nav.append(next);
    if (P.i > 0) { const bk = h("button", "btn", "← Back"); bk.onclick = () => { P.i--; ndRefresh(); }; nav.append(bk); }
    nav.append(h("span", "sp"));
    const st = h("button", "btn", "I'm stuck");
    st.onclick = () => { P.stuck = true; ndRefresh(); };
    const stop = h("button", "btn quiet", "Stop here");
    stop.title = "Save where you are and come back later";
    stop.onclick = () => { needIpc(v, { action: "step", id: v.id, step: P.i }); toast(`Saved. You're at step ${P.i + 1} of ${n}.`); ND.player = null; ndRefresh(); };
    nav.append(st, stop);
    box.append(nav);
  }
}

// ---- sweep (focus mode): one card at a time, full width, big buttons.
// TL;DR first, then what I'd do, then the details under More. Same IPC paths as the cards.
const SW = { active: false, pane: "needs", ids: [], skipped: new Set(), gone: new Set(), more: false, reject: false, rev: 0 };
const SW_ASK = { approve: "approve it", reject: "reject it", edit: "edit it first", do: "do it", skip: "skip it" };
const swView = (id) => { const v = ndFind(id); return v && v.status === "open" ? v : null; };
const swLeft = () => SW.ids.filter((id) => !SW.gone.has(id) && !SW.skipped.has(id) && swView(id));
function swRefresh() { SW.rev++; lastSig.needs = null; lastSig.queue = null; if (SW.pane === "queue") renderQueuePane(); else renderNeedsPane(true); }
function swCandidates(pane) {
  const open = (S.state.needs?.open || []).filter((v) => ndSameWs(v) && ndCan(v));
  return pane === "queue" ? open.filter((v) => v.kind === "decision" || v.kind === "manual") : open;
}
function swStart(pane) {
  const all = swCandidates(pane);
  if (!all.length) return;
  const pick = [];
  let mins = 0;
  for (const v of all) { if (pick.length && mins + (v.minutes || 2) > 10) break; pick.push(v.id); mins += v.minutes || 2; }
  Object.assign(SW, { active: true, pane, ids: pick, skipped: new Set(), gone: new Set(), more: false, reject: false });
  swRefresh();
}
function swStop() { SW.active = false; SW.reject = false; swRefresh(); }
function swDid(v, ok) { if (ok) { SW.gone.add(v.id); SW.more = false; SW.reject = false; swRefresh(); } return ok; }
function swSkip(v) { SW.skipped.add(v.id); SW.more = false; SW.reject = false; swRefresh(); }
// What Enter does: the same checks as the click, and never on a careful item.
function swEnter() {
  const left = swLeft();
  const v = left.length && swView(left[0]);
  if (!v) return;
  if (v.careful) return toast("This one needs a click. Read it first.", true);
  const rec = v.recommend;
  if (v.kind === "decision" && rec === "approve") return swApprove(v);
  if (v.kind === "heads_up") return swDid(v, needIpc(v, { action: "done", id: v.id }));
  if (rec === "skip") return swSkip(v);
  toast("Nothing for Enter to do here. Click a button.", true);
}
function swApprove(v) {
  const q = v.queue || {};
  const overCap = q.capacity && !q.capacity.ok;
  if (overCap && !confirm(`This might not fit in your usage:\n\n${q.capacity.note}\n\nApprove anyway?`)) return;
  if (v.careful && !confirm("You are approving exactly what is shown. Go ahead?")) return;
  swDid(v, sendIpc({ op: "queue_decide", id: v.queue_item, action: "approve", force: !!overCap, hash: q.payload_hash }));
}

function renderSweep(pane) {
  const keep = scrollState(pane);
  pane.textContent = "";
  const page = h("div", "page nd-page");
  const box = h("div", "nd-player nd-sweep");
  page.append(box);
  pane.append(page);
  const left = swLeft();
  const total = SW.ids.length;
  const exit = h("button", "btn quiet", "Stop (Esc)");
  exit.onclick = swStop;
  if (!left.length) {
    box.append(h("div", "nd-pdo", total ? "That's everything for now." : "Nothing to do."));
    box.append(h("div", "nd-pexp", SW.skipped.size ? `The ${SW.skipped.size} you skipped ${SW.skipped.size === 1 ? "is" : "are"} still in the list.` : "Take a break."));
    const nav = h("div", "nd-pnav");
    const b = h("button", "btn primary", "Back to the list");
    b.onclick = swStop;
    nav.append(b);
    box.append(nav);
    return;
  }
  const v = swView(left[0]);
  const mins = left.reduce((a, id) => a + (swView(id)?.minutes || 2), 0);
  const top = h("div", "nd-sweephead");
  top.append(h("span", "nd-pstep", `${total - left.length + 1} of ${total} · about ${mins} min left`), h("span", "sp"), exit);
  box.append(top);
  const bar = h("div", "nd-bar");
  const fill = h("i");
  fill.style.width = ((total - left.length) / total) * 100 + "%";
  bar.append(fill);
  box.append(bar);
  box.dataset.anchor = v.id;
  box.append(ndChips(v, false));
  box.append(h("div", "nd-pdo", v.tldr || v.title));
  const q = v.queue || {};
  if (v.careful) box.append(h("div", "nd-stuck", "Check this one carefully. Read it below, then click."));
  if (v.recommend && v.kind !== "question") {
    const r = h("div", "nd-pexp");
    r.append(document.createTextNode("I'd suggest: "), h("b", null, SW_ASK[v.recommend] || v.recommend));
    box.append(r);
  }
  if (v.why) box.append(h("div", "nd-pstep", v.why));
  if (q.capacity) box.append(h("div", "nd-pstep", (q.capacity.ok ? "✓ " : "✗ ") + q.capacity.note));
  if ((v.careful || SW.more) && q.payload) box.append(h("pre", "qpayload", q.payload));
  if (SW.more) {
    const parts = [v.details, v.steps].filter(Boolean);
    if (parts.length) box.append(h("div", "nd-more", parts.join("\n\n")));
  }
  const nav = h("div", "nd-pnav");
  const doneBtn = (label, msg, cls = "btn primary") => { const b = h("button", cls, label); b.onclick = () => { b.disabled = true; swDid(v, needIpc(v, msg)); }; return b; };
  if (v.kind === "question") {
    const qs = Array.isArray(v.questions) ? v.questions : [];
    const one = qs.length === 1 && !qs[0].multi_select ? qs[0] : null;
    if (one) {
      box.append(h("div", "nd-pstep", one.question || ""));
      for (const o of one.options || []) {
        const b = h("button", "btn", o.label);
        if (o.description) b.title = o.description;
        b.onclick = () => { b.disabled = true; swDid(v, needIpc(v, { action: "answer", id: v.id, answers: [{ selected: [o.label], other: "" }] })); };
        nav.append(b);
      }
    } else {
      const wrap = h("div", "ndcard");
      wrap.append(ndQuestion(v));
      box.append(wrap);
    }
  } else if (v.kind === "decision") {
    const overCap = q.capacity && !q.capacity.ok;
    const ap = h("button", "btn" + (v.recommend === "approve" && !v.careful ? " primary" : ""), overCap ? "Approve anyway…" : "Approve");
    ap.onclick = () => swApprove(v);
    const rj = h("button", "btn" + (v.recommend === "reject" ? " primary" : " danger"), "Reject…");
    rj.onclick = () => { SW.reject = true; swRefresh(); };
    const ed = h("button", "btn", "Edit…");
    ed.title = "Edit it in the Queue";
    ed.onclick = () => { SW.active = false; openQueue(); };
    nav.append(ap, rj, ed);
  } else if (v.kind === "manual") {
    nav.append(doneBtn("I did it", { action: "done", id: v.id }));
  } else if (v.kind === "action") {
    if (v.plan) {
      const n = ndSteps(v).length;
      const b = h("button", "btn primary", v.step > 0 ? `Continue at step ${Math.min(v.step, n - 1) + 1} of ${n}` : `Start · ${n} step${n === 1 ? "" : "s"}`);
      b.onclick = () => { SW.active = false; openNeeds(); ndStart(v); };
      nav.append(b);
    } else if (v.link) {
      const lb = h("button", "btn", "Open");
      lb.onclick = () => (/^https?:/i.test(v.link) ? openUrl(v.link) : openPath(v.link, "default"));
      nav.append(lb);
    }
    nav.append(doneBtn("I did it", { action: "done", id: v.id }, v.plan ? "btn" : "btn primary"));
  } else if (v.kind === "heads_up") {
    nav.append(doneBtn("Got it", { action: "done", id: v.id }));
  } else if (v.kind === "message" && v.xws) {
    box.append(xwsBlock(v.xws, v.id, () => swDid(v, true)));
  }
  if (nav.children.length) box.append(nav);
  if (SW.reject && v.kind === "decision") {
    const m = h("div", "nd-menu");
    const inp = h("input", "nd-in");
    inp.placeholder = "Why? (one line: how the agents learn your taste)";
    const ok = h("button", "btn danger", "Reject");
    const go = () => {
      if (!inp.value.trim()) { inp.focus(); return toast("Add a short reason", true); }
      swDid(v, sendIpc({ op: "queue_decide", id: v.queue_item, action: "reject", reason: inp.value.trim() }));
    };
    ok.onclick = go;
    inp.onkeydown = (e) => { if (e.key === "Enter") go(); if (e.key === "Escape") { e.stopPropagation(); SW.reject = false; swRefresh(); } };
    m.append(inp, ok);
    box.append(m);
    setTimeout(() => inp.focus());
  }
  const sec = h("div", "nd-pnav");
  const sk = h("button", "btn", "Skip for now");
  sk.onclick = () => swSkip(v);
  const mb = h("button", "btn quiet", SW.more ? "Less" : "More");
  mb.onclick = () => { SW.more = !SW.more; swRefresh(); };
  sec.append(sk, mb);
  box.append(sec);
  if (!v.careful && v.kind === "decision" && v.recommend === "approve") box.append(h("div", "sub", "Enter approves it."));
  restoreScroll(pane, keep);
}
function swButton(pane) {
  if (!swCandidates(pane).length) return null;
  const b = h("button", "btn primary nd-sweepgo", "▶ Start a 10-min sweep");
  b.title = "One card at a time. Esc stops.";
  b.onclick = () => swStart(pane);
  return b;
}

// ---- the tab
function ndGroups() {
  if (ND.all && S.state.needs_all) return S.state.needs_all.workspaces.filter((w) => w.count || w.current);
  const n = S.state.needs;
  return n ? [{ path: n.ws, name: baseName(n.ws), open: n.open, today: n.today, count: n.count, minutes: n.minutes }] : [];
}

function renderNeedsPane(force) {
  const pane = $("#pane-needs");
  if (cur()?.type !== "needs") return;
  const n = S.state.needs;
  const act = document.activeElement;
  // Don't wipe an answer or note the user is typing.
  if (!force && act && pane.contains(act) && /^(INPUT|TEXTAREA)$/.test(act.tagName) && act.value) return;
  if (!changed("needs", [n, S.state.needs_all, ND.all, ND.later, ND.done, [...ND.more], ND.menu, ND.dismiss, ND.reject, ND.player, S.state.workspace, SW.active && SW.pane === "needs", SW.rev]) && $(".page", pane)) return;
  if (SW.active && SW.pane === "needs") { renderSweep(pane); return; }
  const keep = scrollState(pane);
  if (ND.player) { renderNeedsPlayer(pane); restoreScroll(pane, keep); return; }
  pane.textContent = "";
  const page = h("div", "page nd-page");
  page.append(paneTitle("📌 Needs you", "needs"));
  if (!n) { page.append(h("div", "sub", "Loading…")); pane.append(page); return; }
  const tot = ND.all && S.state.needs_all ? S.state.needs_all : n;
  page.append(h("div", "nd-total", tot.count ? `${tot.count} waiting · about ${tot.minutes || 0} min in all` : "Nothing is waiting for you. Nice."));
  const sweep = swButton("needs");
  if (sweep) { const sr = h("div", "nd-tools"); sr.append(sweep); page.append(sr); }
  const multi = (S.state.needs_all?.workspaces || []).length > 1;
  if (multi) {
    const tools = h("div", "nd-tools");
    const tb = h("button", "btn", ND.all ? "Show only this workspace" : `Show all workspaces (${S.state.needs_all.count})`);
    tb.onclick = () => { ND.all = !ND.all; ndRefresh(); };
    tools.append(tb);
    page.append(tools);
  }
  const groups = ndGroups();
  const grouped = ND.all && groups.length > 1;
  const later = [];
  if (!grouped) page.append(h("div", "nd-h", "Today"));
  for (const g of groups) {
    if (grouped) page.append(h("div", "nd-h", `${g.name} · ${g.count}`));
    const ids = new Set(g.today || []);
    const today = (g.today || []).map((id) => g.open.find((x) => x.id === id)).filter(Boolean);
    if (!today.length && !grouped && n.count) page.append(h("div", "nd-empty", "Nothing picked for today."));
    for (const v of today) page.append(needCard(v, { today: true }));
    for (const v of g.open) if (!ids.has(v.id)) later.push(v);
  }
  if (later.length) {
    const lh = h("div", "nd-h fold", `${ND.later ? "▾" : "▸"} Later (${later.length})`);
    lh.onclick = () => { ND.later = !ND.later; ndRefresh(); };
    page.append(lh);
    if (ND.later) for (const v of later) page.append(needCard(v, { showWs: grouped }));
  }
  const sn = n.snoozed || [];
  if (sn.length) {
    page.append(h("div", "nd-h", `Snoozed (${sn.length})`));
    for (const v of sn) {
      const r = h("div", "nd-done");
      r.dataset.anchor = v.id;
      r.append(document.createTextNode(`💤 ${v.tldr || v.title} · back ${v.snoozed_until ? new Date(v.snoozed_until * 1000).toLocaleString([], { weekday: "short", hour: "numeric", minute: "2-digit" }) : "later"}  `));
      const b = h("button", "btn sm", "Bring back");
      b.onclick = () => needIpc(v, { action: "unsnooze", id: v.id });
      r.append(b);
      page.append(r);
    }
  }
  const dn = n.done_today || [];
  if (dn.length) {
    const dh = h("div", "nd-h fold", `${ND.done ? "▾" : "▸"} Done today (${dn.length})`);
    dh.onclick = () => { ND.done = !ND.done; ndRefresh(); };
    page.append(dh);
    if (ND.done) for (const v of dn) {
      const r = h("div", "nd-done", `${v.status === "dismissed" ? "✕" : "✓"} ${v.tldr || v.title}${v.note ? " · " + v.note : ""}`);
      r.dataset.anchor = v.id;
      page.append(r);
    }
  }
  pane.append(page);
  restoreScroll(pane, keep);
}

const openNeeds = () => { lastSig.needs = null; openTab({ type: "needs" }); };

// Top 3 for today, above the orchestrator's chat.
function renderNeedsStrip() {
  const el = $("#needs-strip");
  const n = S.state.needs;
  const show = !!(isOrch(curKey()) && n && n.count > 0);
  const rows = show ? (n.today || []).map((id) => n.open.find((x) => x.id === id)).filter(Boolean) : [];
  if (!changed("needsstrip", [show, rows.map((v) => [v.id, v.tldr, v.title, v.step, v.kind, v.priority]), n?.count])) return;
  el.classList.toggle("hidden", !show);
  el.textContent = "";
  if (!show) return;
  const head = h("div", "nd-sh");
  head.append(h("span", null, `📌 Needs you (${n.count})`), h("span", "sp"), h("span", "sub", "open"));
  head.onclick = openNeeds;
  el.append(head);
  for (const v of rows) {
    const r = h("div", "nd-sr");
    r.append(h("span", "qchip" + (v.priority === "urgent" ? " urgent" : ""), ND_KIND[v.kind] || v.kind), h("span", "t", v.tldr || v.title));
    const p = ndPaused(v);
    if (p) r.append(h("span", "sub", p));
    r.onclick = openNeeds;
    el.append(r);
  }
}

// ---------------------------------------------------------------- money (business lines, P&L)

const usd = (v) => (v == null ? "—" : `${v < 0 ? "−" : ""}$${Math.abs(v).toFixed(2)}`);

// Bars of weekly net (oldest first), scaled to `max` across all lines.
function weeklyBars(series, max) {
  const w = 8 * 9, hgt = 28, mid = hgt / 2;
  const bars = (series || []).map((v, i) => {
    const bh = max ? Math.max(1, (Math.abs(v) / max) * (mid - 1)) : 0;
    const y = v >= 0 ? mid - bh : mid;
    return v ? `<rect x="${i * 9}" y="${y.toFixed(1)}" width="7" height="${bh.toFixed(1)}" fill="${v >= 0 ? "#16a34a" : "#dc2626"}"><title>${v >= 0 ? "" : "−"}$${Math.abs(v).toFixed(2)}</title></rect>` : "";
  }).join("");
  return `<svg class="spark" width="${w}" height="${hgt}" viewBox="0 0 ${w} ${hgt}"><line x1="0" x2="${w}" y1="${mid}" y2="${mid}" stroke="currentColor" stroke-opacity=".25"/>${bars}</svg>`;
}

async function renderMoney() {
  const pane = $("#pane-money");
  let m;
  try { m = await api("/api/money"); } catch (e) { return fail(e); }
  pane.textContent = "";
  const page = h("div", "page");
  page.append(h("h1", null, "Money"), h("div", "sub", `${S.state.workspace} · ledger/ledger.csv and the line registry`));
  const L = m.ledger;
  if (L) {
    const t = h("table", "grid");
    t.innerHTML = "<tr><th></th><th class='num'>Revenue</th><th class='num'>Fees</th><th class='num'>Costs</th><th class='num'>Agent usage</th><th class='num'>Net</th><th class='num'>Your hours</th></tr>";
    for (const [label, x] of [["This week", L.this_week], ["Last week", L.last_week], ["All time", L.all_time]]) {
      const tr = h("tr");
      tr.innerHTML = `<td>${label}</td><td class="num">${usd(x.revenue)}</td><td class="num">${usd(x.fees)}</td><td class="num">${usd(x.costs)}</td><td class="num">${usd(x.tokens)}</td><td class="num"><b>${usd(x.net)}</b></td><td class="num">${x.hours ? x.hours.toFixed(1) : "—"}</td>`;
      t.append(tr);
    }
    page.append(t);
  } else page.append(h("div", "sub", "No ledger/ledger.csv in this workspace yet."));
  page.append(h("div", "set-section", "Lines"));
  const max = Math.max(0, ...Object.values(L?.weekly_net || {}).flat().map(Math.abs));
  const t = h("table", "grid money");
  t.innerHTML = "<tr><th>Line</th><th>Status</th><th class='num'>Usage this week</th><th class='num'>Net this week</th><th class='num'>Last week</th><th class='num'>All time</th><th class='num'>Per hour</th><th class='num'>Per $ usage</th><th>Last 8 weeks</th><th>Test</th></tr>";
  for (const r of m.lines) {
    const l = r.line;
    const tr = h("tr");
    const st = h("select");
    for (const s of ["testing", "active", "paused", "killed"]) { const o = h("option", null, s); o.value = s; st.append(o); }
    st.value = l.status;
    st.onchange = () => { if (sendIpc({ op: "line", action: "status", line: { id: l.id, status: st.value } })) setTimeout(renderMoney, 500); };
    const budget = l.weekly_budget_usd ? `${usd(r.spent_week)} / ${usd(l.weekly_budget_usd)}` : usd(r.spent_week);
    const test = l.status === "testing" ? `${r.days_left == null ? "" : r.days_left >= 0 ? `${r.days_left} days left` : `ended ${-r.days_left} days ago`}${l.kill_criteria ? ` · kill if: ${esc(l.kill_criteria)}` : ""}` : "";
    tr.innerHTML = `<td><b>${esc(l.id)}</b> ${esc(l.name)}${l.playbook ? `<div class="sub mono">${esc(l.playbook)}</div>` : ""}</td><td></td>
      <td class="num${l.weekly_budget_usd && r.spent_week >= l.weekly_budget_usd ? " bad" : ""}">${budget}${l.auto_paused ? " <span class='qchip st-void'>auto-paused</span>" : ""}</td>
      <td class="num">${usd(r.this_week.net)}</td><td class="num">${usd(r.last_week.net)}</td><td class="num"><b>${usd(r.all_time.net)}</b></td>
      <td class="num">${r.per_hour == null ? "—" : usd(r.per_hour)}</td><td class="num">${r.per_token_dollar == null ? "—" : r.per_token_dollar.toFixed(1) + "×"}</td>
      <td>${weeklyBars(L?.weekly_net?.[l.id], max)}</td><td class="small">${test}</td>`;
    tr.children[1].append(st);
    t.append(tr);
  }
  if (!m.lines.length) page.append(h("div", "sub", "No lines yet. Add one below, or the orchestrator proposes them (propose_line → a new_venture item for your approval)."));
  else page.append(t);
  // Ledger lines that aren't registered yet.
  const unknown = Object.keys(L?.per_line || {}).filter((id) => !m.lines.some((r) => r.line.id === id) && id !== "-");
  if (unknown.length) page.append(h("div", "sub", `In the ledger but not registered: ${unknown.join(", ")}`));
  page.append(h("div", "set-section", "Add or change a line"));
  const f = h("div", "row line-form");
  const fld = (ph, w, type) => { const e = h("input"); e.placeholder = ph; e.style.width = w; if (type) e.type = type; e.spellcheck = false; return e; };
  const id = fld("id (e.g. A)", "90px"), name = fld("name", "160px"), play = fld("playbook (pipelines/…)", "190px"), bud = fld("$ / week", "90px", "number"), end = fld("", "140px", "date"), kill = fld("kill criteria", "240px");
  const status = h("select");
  for (const s of ["testing", "active"]) { const o = h("option", null, s); o.value = s; status.append(o); }
  const save = h("button", "btn primary", "Save line");
  save.onclick = () => {
    const line = { id: id.value.trim(), name: name.value.trim(), status: status.value, playbook: play.value.trim(), weekly_budget_usd: +bud.value || 0, end: end.value, kill_criteria: kill.value.trim() };
    if (sendIpc({ op: "line", action: "save", line })) setTimeout(renderMoney, 500);
  };
  f.append(id, name, status, play, bud, end, kill, save);
  page.append(f, h("div", "sub", "Saving an existing id changes that line. The weekly budget caps the agent usage of threads tagged with the line: over it, the line is paused automatically and only paid-order threads start for it. Log your own time as ledger rows of kind hours to get profit per hour."));
  pane.append(page);
}

// The Inbox: paste text or drop files here (or press Ctrl+Alt+I anywhere to save the
// clipboard); each lands in <workspace>/inbox/ and the orchestrator is told.
function inboxBox() {
  const box = h("div", "inbox-box");
  const ta = h("textarea", "inbox-text");
  ta.rows = 2;
  ta.placeholder = "Inbox: paste a buyer message, order or stats here, or drop files. Ctrl+Alt+I saves the clipboard from anywhere.";
  const row = h("div", "row");
  const save = h("button", "btn primary", "Save to inbox");
  save.onclick = async () => {
    if (!ta.value.trim()) return;
    try { await api("/api/inbox", { body: { text: ta.value } }); ta.value = ""; } catch (e) { fail(e); }
  };
  const clip = h("button", "btn", "📋 From clipboard");
  clip.title = "Save what's on the clipboard (text or image). Same as Ctrl+Alt+I.";
  clip.onclick = () => sendIpc({ op: "inbox_clipboard" });
  row.append(save, clip);
  box.append(ta, row);
  const drop = async (files) => {
    for (const f of files) {
      const data = await new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(String(r.result).split(",")[1] || ""); r.onerror = rej; r.readAsDataURL(f); });
      try { await api("/api/inbox", { body: { name: f.name, data } }); } catch (e) { fail(e); }
    }
  };
  box.ondragover = (e) => { e.preventDefault(); e.stopPropagation(); box.classList.add("over"); };
  box.ondragleave = () => box.classList.remove("over");
  box.ondrop = (e) => { e.preventDefault(); e.stopPropagation(); box.classList.remove("over"); drop([...e.dataTransfer.files]); };
  return box;
}

function queueCard(i) {
  const c = h("div", "qcard" + (i.urgent ? " urgent" : ""));
  c.dataset.anchor = i.id;
  const head = h("div", "qhead");
  head.append(h("span", "qchip", i.type));
  if (i.line) head.append(h("span", "qchip", "line " + i.line));
  head.append(h("span", "qchip money", `$${(+i.expected_value_usd || 0).toFixed(2)}`));
  head.append(h("span", "qchip risk-" + i.risk, i.risk + " risk"));
  if (i.who_executes === "user") head.append(h("span", "qchip you", "you do this by hand"));
  if (i.urgent) head.append(h("span", "qchip urgent", "urgent"));
  c.append(head, h("div", "qsummary", i.summary), h("div", "qmeta", `${i.id} · from ${i.created_by} · ${relTime(i.created_at) === "now" ? "just now" : relTime(i.created_at) + " ago"}`));
  if (i.capacity) {
    const cap = h("div", "qcap " + (i.capacity.ok ? "ok" : "bad"));
    cap.textContent = (i.capacity.ok ? "✓ Fits your usage: " : "✗ Might not fit your usage: ") + i.capacity.note + (i.order_id ? ` (order ${i.order_id})` : "");
    c.append(cap);
  }
  const pre = h("pre", "qpayload", i.payload);
  c.append(pre);
  if (i.manual) c.append(manualTask(i));
  if (i.command) {
    c.append(h("div", "qmeta", `runs once you approve${i.secrets?.length ? ` · uses your saved secrets: ${i.secrets.join(", ")}` : ""}`), h("pre", "qpayload qcmd", i.command));
  }
  if (i.attachments?.length) {
    const g = h("div", "qatt");
    for (const p of i.attachments) {
      const ext = (p.split(".").pop() || "").toLowerCase();
      if (["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"].includes(ext)) {
        const img = h("img");
        img.src = fileUrl(p);
        img.title = p;
        img.onclick = () => lightbox(fileUrl(p));
        g.append(img);
      } else {
        const a = h("a", "flink qfile", "📄 " + baseName(p));
        a.title = p;
        a.onclick = () => openPath(p, ext === "pdf" ? "default" : "editor");
        g.append(a);
      }
    }
    c.append(g);
  }
  const acts = h("div", "row qacts");
  const approve = h("button", "btn primary", "Approve");
  const edit = h("button", "btn", "Edit, then approve");
  const reject = h("button", "btn danger", "Reject…");
  const busy = (b) => { for (const x of $$("button", acts)) x.disabled = true; b.textContent = "…"; };
  const overCap = i.capacity && !i.capacity.ok;
  if (overCap) { approve.textContent = "Approve anyway…"; approve.className = "btn danger"; }
  approve.onclick = () => {
    if (overCap && !confirm(`${i.id} might not fit in your usage:\n\n${i.capacity.note}\n\nTaking it risks a late delivery. Approve anyway?`)) return;
    if (sendIpc({ op: "queue_decide", id: i.id, action: "approve", force: overCap, hash: i.payload_hash })) busy(approve);
  };
  edit.onclick = () => {
    c.classList.add("editing");
    const ta = h("textarea", "qedit");
    ta.value = i.payload;
    ta.rows = Math.min(20, i.payload.split("\n").length + 2);
    pre.replaceWith(ta);
    acts.textContent = "";
    const ok = h("button", "btn primary", "Approve edited");
    const cancel = h("button", "btn", "Cancel");
    ok.onclick = () => {
      if (overCap && !confirm(`${i.id} might not fit in your usage:\n\n${i.capacity.note}\n\nApprove the edited item anyway?`)) return;
      if (sendIpc({ op: "queue_decide", id: i.id, action: "edit_approve", payload: ta.value, force: overCap })) { c.classList.remove("editing"); busy(ok); }
    };
    cancel.onclick = () => { c.classList.remove("editing"); lastSig.queue = null; renderQueuePane(); };
    acts.append(ok, cancel);
    ta.focus();
  };
  reject.onclick = () => {
    c.classList.add("editing");
    acts.textContent = "";
    const why = h("input", "qwhy");
    why.placeholder = "Why? (one line: it's how the agents learn your taste)";
    const ok = h("button", "btn danger", "Reject");
    const cancel = h("button", "btn", "Cancel");
    const go = () => {
      if (!why.value.trim()) { why.focus(); return toast("a rejection needs a reason", true); }
      if (sendIpc({ op: "queue_decide", id: i.id, action: "reject", reason: why.value.trim() })) { c.classList.remove("editing"); busy(ok); }
    };
    ok.onclick = go;
    why.onkeydown = (e) => { if (e.key === "Enter") go(); if (e.key === "Escape") cancel.onclick(); };
    cancel.onclick = () => { c.classList.remove("editing"); lastSig.queue = null; renderQueuePane(); };
    acts.append(why, ok, cancel);
    why.focus();
  };
  if (i.manual) {
    // A task the user does by hand: Done (with the dropped files) instead of approve/edit.
    const done = h("button", "btn primary", `Done${i.dropped?.length ? ` (${i.dropped.length} file${i.dropped.length > 1 ? "s" : ""})` : ""}`);
    done.title = "Tell the orchestrator you did it, and that the files you dropped are in";
    done.onclick = () => { if (sendIpc({ op: "queue_done", id: i.id })) busy(done); };
    acts.append(done, reject);
  } else acts.append(approve, edit, reject);
  c.append(acts);
  return c;
}

// ---------------------------------------------------------------- edit, retry, queue

function startEdit(el) {
  if (el.classList.contains("editing")) return;
  const original = el.dataset.text;
  const saved = [...el.childNodes];
  el.classList.add("editing");
  el.textContent = "";
  const ta = h("textarea");
  ta.value = original;
  ta.spellcheck = false;
  const row = h("div", "edit-row");
  const ok = h("button", "btn primary sm", "Save & resend");
  const cancel = h("button", "btn sm", "Cancel");
  row.append(ok, cancel, h("span", "hint", "Ctrl+Enter to send · Esc to cancel · everything after this message is discarded (files aren't reverted)"));
  el.append(ta, row);
  ta.focus();
  ta.setSelectionRange(ta.value.length, ta.value.length);
  const close = () => { el.classList.remove("editing"); el.textContent = ""; el.append(...saved); };
  cancel.onclick = close;
  ok.onclick = async () => {
    const text = ta.value.trim();
    if (!text) return;
    close();
    await resend(+el.dataset.ts, text);
  };
  ta.onkeydown = (e) => {
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); }
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); ok.click(); }
  };
}

async function resend(ts, text) {
  try {
    await ipcCall("edit", { thread: curKey(), ts, text });
  } catch (e) { fail(e); }
}

function renderQueue() {
  const panel = $("#queue");
  const k = curKey();
  const q = (k && info(k)?.queue) || [];
  if (!changed("queue", [k, q])) return;
  panel.classList.toggle("hidden", !q.length);
  panel.textContent = "";
  if (!q.length) return;
  const head = h("div", "queue-head");
  head.append(h("span", null, `${q.length} queued · runs after the current turn`), h("span", "sp"));
  const clear = h("button", null, "clear all");
  clear.onclick = () => queueAction("clear");
  head.append(clear);
  panel.append(head);
  q.forEach((m, i) => {
    const r = h("div", "qm");
    r.append(h("span", "n", String(i + 1)));
    if (m.edit) r.append(h("span", "tag", "edit"));
    if (m.sender === "orchestrator") r.append(h("span", "tag", "from orchestrator"));
    if (m.sender === "app") r.append(h("span", "tag", "automatic"));
    const t = h("span", "t", m.text + (m.attachments?.length ? `  📎${m.attachments.length}` : ""));
    t.title = m.text;
    r.append(t);
    const btns = h("span", "btns");
    const now = h("button", null, "▶ send now");
    now.title = "Send it into the current turn now (it reads it at its next step)";
    now.onclick = () => queueAction("now", m.id);
    const ed = h("button", null, "✎ edit");
    ed.title = "Move back into the composer";
    ed.onclick = async () => {
      await queueAction("remove", m.id);
      const inp = $("#input");
      inp.value = m.text + (inp.value ? "\n" + inp.value : "");
      autosize();
      inp.focus();
    };
    const x = h("button", null, "✕");
    x.title = "Remove from queue";
    x.onclick = () => queueAction("remove", m.id);
    btns.append(now, ed, x);
    r.append(btns);
    panel.append(r);
  });
}

function queueAction(action, id) {
  return api("/api/queue", { body: { thread: curKey(), action, id: id || "" } }).catch(fail);
}

// ---------------------------------------------------------------- side panel & pop-outs

function openSide(key) {
  if (SOLO) return window.parent !== window ? window.parent.postMessage({ orch: "open-side", key }, location.origin) : popOut(key);
  if (!isOrch(key) && !threadRow(key)) return toast(`no thread named ${key}`, true);
  const frame = $("#side-frame");
  if (S.side !== key) frame.src = `/?t=${encodeURIComponent(ORCH.token)}&view=${encodeURIComponent(key)}`;
  S.side = key;
  S.unread.delete(key);
  $("#side-pane").classList.remove("hidden");
  $("#splitter").classList.remove("hidden");
  renderTree();
}

function closeSide() {
  S.side = null;
  $("#side-pane").classList.add("hidden");
  $("#splitter").classList.add("hidden");
  $("#side-frame").src = "about:blank";
}

async function popOut(key) {
  try {
    const r = await api("/api/popout", { body: { thread: key } });
    if (!r.native) window.open(`/?t=${encodeURIComponent(ORCH.token)}&view=${encodeURIComponent(key)}`, "_blank", "width=920,height=860");
    if (S.side === key) closeSide();
  } catch (e) { fail(e); }
}

// Have the agent write a handoff, then continue the chat in a fresh session from it.
async function handoff(key = curKey()) {
  if (!key) return toast("open a chat first", true);
  try {
    const r = await api("/api/handoff", { body: { key } });
    toast(r.delivery === "queued" ? "Handoff queued: it runs after the current turn" : "Writing a handoff, then continuing in a new session…");
  } catch (e) { fail(e); }
}

function onThreadLink(e) {
  const a = e.target.closest("[data-thread]");
  if (!a) return false;
  e.preventDefault();
  e.stopPropagation();
  const key = a.dataset.thread;
  if (e.ctrlKey || e.metaKey) openChat(key);
  else if (e.shiftKey) popOut(key);
  else openSide(key);
  return true;
}

function wireSplit() {
  const sp = $("#splitter");
  sp.onmousedown = (e) => {
    e.preventDefault();
    sp.classList.add("drag");
    document.body.classList.add("dragging");
    const panes = $("#panes").getBoundingClientRect();
    const move = (m) => {
      const w = Math.min(Math.max(panes.right - m.clientX, 320), panes.width - 360);
      $("#side-pane").style.width = w + "px";
    };
    const up = () => {
      sp.classList.remove("drag");
      document.body.classList.remove("dragging");
      removeEventListener("mousemove", move);
      removeEventListener("mouseup", up);
    };
    addEventListener("mousemove", move);
    addEventListener("mouseup", up);
  };
  addEventListener("message", (e) => {
    if (e.origin !== location.origin || !e.data?.orch) return;
    const { orch, key } = e.data;
    if (orch === "close-side") closeSide();
    if (orch === "pop-out") popOut(key);
    if (orch === "open-tab") { closeSide(); openChat(key); }
    if (orch === "open-side") openSide(key);
    if (orch === "open-doc") openDoc(e.data.path, e.data.base);
  });
}

async function updateNow(upd) {
  const it = await quickPick([
    { label: `Update to ${upd.version} now`, desc: "downloads, verifies, closes, updates and reopens", value: "now" },
    { label: "See what's new", desc: "opens the release page", value: "notes" },
  ], `Orchestrator ${upd.version} is available. Running threads stop and can be continued after.`);
  if (!it) return;
  if (it.value === "notes") return openUrl(upd.url);
  toast("Downloading the update…");
  try {
    await api("/api/update/install", { body: {} });
    toast("Updating: Orchestrator will reopen in a few seconds");
  } catch (e) { fail(e); }
}

function openUrl(url) {
  return api("/api/open-url", { body: { url } }).catch(fail);
}

function copy(text) {
  navigator.clipboard.writeText(text).then(() => toast("copied"), () => toast("copy failed", true));
}

function lightbox(src) {
  const lb = $("#lightbox");
  $("img", lb).src = src;
  lb.classList.remove("hidden");
}

function openPath(path, how, extra = {}) {
  api("/api/open", { body: { path, how, ...extra } }).catch(fail);
}

// Folder that relative paths in the current view are taken from.
const viewBase = () => (cur()?.type === "doc" ? cur().dir : info(curKey())?.cwd) || S.state.workspace;

// A file path in a message or document. Markdown opens in the viewer; anything else is
// shown in Explorer. Shift+click always shows it in Explorer, Ctrl+click opens the editor.
function onFileLink(e) {
  const a = e.target.closest("[data-path]");
  if (!a || a.closest(".files, .composer")) return false;
  e.preventDefault();
  e.stopPropagation();
  let path = a.dataset.path;
  let line;
  const m = path.match(/^(.+?):(\d+)(?::\d+)?$/);
  if (m) { path = m[1]; line = +m[2]; }
  try { path = decodeURIComponent(path); } catch {}
  const base = viewBase();
  if (e.ctrlKey || e.metaKey) openPath(path, "editor", { base, line });
  else if (!e.shiftKey && isDoc(path)) openDoc(path, base);
  else openPath(path, "reveal", { base });
  return true;
}

async function openDoc(path, base) {
  if (SOLO && window.parent !== window) return window.parent.postMessage({ orch: "open-doc", path, base }, location.origin);
  try {
    const d = await api("/api/doc", { body: { path, base } });
    const i = S.tabs.findIndex((t) => t.type === "doc" && t.key.toLowerCase() === d.path.toLowerCase());
    if (i !== -1) Object.assign(S.tabs[i], { text: d.text, name: d.name, dir: d.dir });
    openTab({ type: "doc", key: d.path, name: d.name, dir: d.dir, text: d.text });
  } catch (e) { fail(e); }
}

function renderDoc() {
  const t = cur();
  const pane = $("#pane-doc");
  pane.textContent = "";
  const page = h("div", "page doc-page");
  const bar = h("div", "doc-bar");
  const where = h("span", "path lnk", t.key);
  where.title = "Show in Explorer";
  where.onclick = () => openPath(t.key, "reveal");
  const btn = (label, title, fn) => { const b = h("button", "btn", label); b.title = title; b.onclick = fn; return b; };
  bar.append(where,
    btn("↻ Reload", "Read the file again", () => openDoc(t.key)),
    btn("Edit", "Open in the editor", () => openPath(t.key, "editor")));
  const body = h("div", "md doc");
  body.innerHTML = /\.txt$/i.test(t.key) ? `<pre class="doc-text">${esc(t.text)}</pre>` : md(t.text);
  // Images with paths relative to the document.
  for (const img of body.querySelectorAll("img")) {
    const src = new URL(img.src, location.href);
    const p = src.pathname === "/file" && src.searchParams.get("path");
    if (p && !/^([A-Za-z]:[\\/]|\\\\)/.test(p)) img.src = fileUrl(t.dir + "\\" + p.replace(/\//g, "\\"));
  }
  page.append(bar, body);
  pane.append(page);
}

function mediaView(ev) {
  const e = h("div", "ev media");
  const box = h("div", "media-box");
  const url = fileUrl(ev.path);
  const ext = (ev.ext || "").toLowerCase();
  if (["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"].includes(ext)) {
    const img = h("img");
    img.src = url;
    img.alt = ev.caption || baseName(ev.path);
    img.onclick = () => lightbox(url);
    box.append(img);
  } else if (["mp4", "webm", "mov"].includes(ext)) {
    const v = h("video");
    v.src = url;
    v.controls = true;
    v.preload = "metadata";
    box.append(v);
  } else if (["mp3", "wav", "ogg", "m4a"].includes(ext)) {
    const a = h("audio");
    a.src = url;
    a.controls = true;
    box.append(a);
  } else if (ext === "pdf") {
    const f = h("iframe");
    f.src = url;
    box.append(f);
  }
  const cap = h("div", "media-cap");
  if (ev.caption) cap.append(document.createTextNode(ev.caption + "  "));
  const l = h("span", "lnk", baseName(ev.path));
  l.title = ev.path + " (show in Explorer)";
  l.onclick = () => openPath(ev.path, "reveal");
  cap.append(l);
  box.append(cap);
  e.append(box);
  return e;
}

// ---------------------------------------------------------------- questions

function questionCard(ev) {
  const card = h("div", "ev question" + (ev.permission ? " perm" : ""));
  card.dataset.id = ev.id;
  card.append(h("div", "q-head", ev.permission ? `permission · ${toolName(ev.permission.tool)}` : "needs your input"));
  if (ev.permission?.input && ev.permission.input !== "{}") card.append(h("div", "perm-input", ev.permission.input));
  const qs = Array.isArray(ev.questions) ? ev.questions : [];
  qs.forEach((q, qi) => {
    const multi = !!q.multi_select;
    const box = h("div", "q" + (multi ? " multi" : ""));
    const title = h("div", "q-title");
    if (q.header) title.append(h("span", "q-tag", q.header));
    title.append(document.createTextNode(q.question || ""));
    if (multi) title.append(h("span", "q-sub", "choose any"));
    box.append(title);
    const opts = h("div", "q-opts");
    (q.options || []).forEach((o, oi) => {
      const b = h("button", "q-opt");
      b.dataset.label = o.label;
      const txt = h("span");
      txt.append(h("div", "ol", o.label));
      if (o.description) txt.append(h("div", "od", o.description));
      b.append(h("span", "box"), txt);
      if (qs.length === 1) b.append(h("span", "num", String(oi + 1)));
      b.onclick = () => {
        if (card.classList.contains("done")) return;
        if (multi) b.classList.toggle("sel");
        else { for (const x of $$(".q-opt", opts)) x.classList.toggle("sel", x === b); $(".q-other", box).value = ""; }
        if (qs.length === 1 && !multi) submitAnswers(card, qs);
      };
      opts.append(b);
    });
    box.append(opts);
    const other = h("input", "q-other");
    other.placeholder = "Other: type your own answer…";
    other.oninput = () => { if (!multi && other.value) for (const x of $$(".q-opt", opts)) x.classList.remove("sel"); };
    other.onkeydown = (e) => { if (e.key === "Enter") submitAnswers(card, qs); };
    box.append(other);
    card.append(box);
  });
  const actions = h("div", "q-actions");
  const submit = h("button", "btn primary", qs.length === 1 && !qs[0]?.multi_select ? "Send answer" : "Submit answers");
  submit.onclick = () => submitAnswers(card, qs);
  actions.append(submit, h("span", "hint", qs.length === 1 ? "click an option, or type your own" : "answer each question, then submit"));
  card.append(actions);
  return card;
}

function collectAnswers(card) {
  return $$(".q", card).map((box) => ({
    selected: $$(".q-opt.sel", box).map((b) => b.dataset.label),
    other: $(".q-other", box).value.trim(),
  }));
}

async function submitAnswers(card, qs) {
  const answers = collectAnswers(card);
  const missing = answers.findIndex((a) => !a.selected.length && !a.other);
  if (missing !== -1) return toast(`Answer “${qs[missing]?.header || qs[missing]?.question || "each question"}” first`, true);
  try { await ipcCall("answer", { id: card.dataset.id, answers }); }
  catch (e) { fail(e); }
}

function markQuestion(card, answers, cancelled) {
  card.classList.add("done");
  if (cancelled) card.classList.add("cancelled");
  $(".q-head", card).textContent = cancelled ? "question closed without an answer" : "✓ answered";
  const boxes = $$(".q", card);
  (answers || []).forEach((a, i) => {
    const box = boxes[i];
    if (!box) return;
    for (const b of $$(".q-opt", box)) b.classList.toggle("sel", (a.selected || []).includes(b.dataset.label));
    const other = $(".q-other", box);
    other.value = a.other || "";
  });
  for (const el of $$("button, input", card)) el.disabled = true;
}

// ---------------------------------------------------------------- plan panel

function renderPlan() {
  const panel = $("#plan");
  const k = curKey();
  const plan = (k && info(k)?.plan) || [];
  if (!plan.length) { panel.classList.add("hidden"); return; }
  if (!changed("plan", [k, plan, S.planOpen])) return;
  panel.classList.remove("hidden");
  panel.classList.toggle("closed", !S.planOpen);
  panel.textContent = "";
  const done = plan.filter((p) => p.status === "completed").length;
  const now = plan.find((p) => p.status === "in_progress");
  const head = h("div", "plan-head");
  const bar = h("span", "bar");
  const fill = h("i");
  fill.style.width = `${(100 * done) / plan.length}%`;
  bar.append(fill);
  head.append(h("span", "chev", "⌄"), bar, h("span", "now", now ? now.content : done === plan.length ? "All done" : "Plan"), h("span", "cnt", `${done}/${plan.length}`));
  head.onclick = () => { S.planOpen = !S.planOpen; renderPlan(); };
  const items = h("div", "plan-items");
  for (const p of plan) {
    const it = h("div", "pi " + (p.status || "pending"));
    it.append(h("span", "mk"), h("span", null, p.content));
    items.append(it);
  }
  panel.append(head, items);
}

// ---------------------------------------------------------------- attachments

function renderTray() {
  const tray = $("#tray");
  tray.textContent = "";
  tray.classList.toggle("hidden", !S.attachments.length);
  S.attachments.forEach((a, i) => {
    const el = h("div", "att");
    if (a.preview) { const img = h("img"); img.src = a.preview; el.append(img); }
    else el.append(h("span", "fi", "📄"));
    const meta = h("span");
    meta.append(h("div", "an", a.name), h("div", "up", a.uploading ? "uploading…" : a.error || ""));
    el.append(meta);
    const x = h("button", null, "×");
    x.title = "Remove";
    x.onclick = () => { S.attachments.splice(i, 1); renderTray(); };
    el.append(x);
    tray.append(el);
  });
}

function addFiles(files) {
  for (const file of files) {
    if (file.size > 50_000_000) { toast(`${file.name} is over 50 MB`, true); continue; }
    const a = { name: file.name || "pasted.png", uploading: true };
    if (file.type.startsWith("image/")) a.preview = URL.createObjectURL(file);
    S.attachments.push(a);
    const reader = new FileReader();
    reader.onload = async () => {
      const data = String(reader.result).split(",")[1] || "";
      try {
        const r = await api("/api/upload", { body: { name: a.name, data } });
        a.path = r.path;
      } catch (e) { a.error = e.message; }
      a.uploading = false;
      renderTray();
    };
    reader.readAsDataURL(file);
  }
  renderTray();
  $("#input").focus();
}

// ---------------------------------------------------------------- time estimates

const mins = (s) => { const m = s / 60; return m < 1 ? "<1m" : m < 90 ? `${Math.round(m)}m` : `${(m / 60).toFixed(1)}h`; };
const estRange = (e) => (e.high_s > e.low_s ? `${mins(e.low_s)}–${mins(e.high_s)}` : mins(e.low_s));

// Live text for an estimate: how far in, and how much is left (or how far over).
function estText(e, short = false) {
  const el = Math.max(0, Date.now() / 1000 - e.set_at);
  if (el > e.high_s * 1.1) return short ? `⏱ +${mins(el - e.high_s)}` : `⏱ ${mins(el)} in · est ${estRange(e)} · running over`;
  const left = e.high_s - el;
  return short ? `⏱ ~${mins(Math.max(left, 0))} left` : `⏱ ${mins(el)} in · est ${estRange(e)} · ~${mins(Math.max(left, 60))} left`;
}

// An element that keeps an estimate's text current.
function estBadge(e, short = false) {
  const b = h("span", "est" + (Date.now() / 1000 - e.set_at > e.high_s * 1.1 ? " over" : ""), estText(e, short));
  b.dataset.est = JSON.stringify(e);
  if (short) b.dataset.short = "1";
  b.title = `${e.kind || "task"}: estimated ${estRange(e)}${e.task ? `\n${e.task}` : ""}`;
  return b;
}
setInterval(() => {
  for (const b of $$("[data-est]")) {
    const e = JSON.parse(b.dataset.est);
    b.textContent = estText(e, !!b.dataset.short);
    b.classList.toggle("over", Date.now() / 1000 - e.set_at > e.high_s * 1.1);
  }
}, 15000);

let spinTimer = null;
function updateWorking() {
  $("#working")?.remove();
  clearInterval(spinTimer);
  const k = curKey();
  const inf = k && info(k);
  if (!inf?.busy) return;
  const w = h("div", "ev working");
  w.id = "working";
  const label = inf.needs_input ? "Waiting for your answer…" : inf.status === "queued" ? "Queued…" : "Working…";
  const tm = h("span", "tm");
  w.append(document.createTextNode(label + " "), tm);
  if (inf.estimate && !inf.isOrch) w.append(h("span", "hint", " · "), estBadge(inf.estimate));
  w.append(h("span", "hint", " (esc to stop)"));
  let f = 0;
  w.dataset.g = SPINNER[0];
  const tick = () => {
    if (inf.started_at) {
      const s = Math.max(0, Math.floor(Date.now() / 1000 - inf.started_at));
      tm.textContent = s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
    }
  };
  tick();
  spinTimer = setInterval(() => { f = (f + 1) % SPINNER.length; w.dataset.g = SPINNER[f]; if (f === 0) tick(); }, 120);
  $("#log").append(w);
}

// ================================================================ websocket

function connect() {
  const ws = new WebSocket(`ws://${location.host}/ws?t=${encodeURIComponent(ORCH.token)}`);
  ws.onmessage = (m) => {
    const ev = JSON.parse(m.data);
    if (ev.kind === "state") return scheduleState();
    if (ev.kind === "toast") return toast(ev.text);
    if (ev.kind === "ipc_reply") return ipcReply(ev);
    if (ev.kind === "delta" || ev.kind === "draft_reset") {
      if (ev.thread !== curKey()) return;
      if (ev.kind === "draft_reset") { $("#draft")?.remove(); return; }
      let d = $("#draft");
      const stick = nearEnd();
      if (!d) {
        d = h("div", "ev text md draft");
        d.id = "draft";
        d.dataset.raw = "";
        const w = $("#working");
        w ? $("#log").insertBefore(d, w) : $("#log").append(d);
      }
      d.dataset.raw += ev.text;
      d.innerHTML = md(d.dataset.raw);
      if (stick) scrollEnd(true);
      return;
    }
    if (ev.kind === "question" && ev.thread !== curKey()) {
      toast(`${isOrch(ev.thread) ? "orchestrator" : ev.thread} is asking you a question`);
    }
    if (ev.kind === "resync") { loadState(); if (curKey()) loadLog(curKey()); return; }
    if (S.loading && ev.thread === S.loading.key) { S.loading.pending.push(ev); return; }
    if (ev.thread === curKey()) {
      const stick = nearEnd();
      $("#working")?.remove();
      renderEvent(ev);
      updateWorking();
      if (stick) scrollEnd(true);
    } else if (ev.kind === "result" && !isOrch(ev.thread)) {
      S.unread.add(ev.thread);
      renderTree();
    }
  };
  ws.onclose = () => setTimeout(() => { connect(); loadState().then(() => curKey() && loadLog(curKey())); }, 1200);
}

// ================================================================ composer chips + status bar

function renderChips() {
  const c = $("#chips");
  c.textContent = "";
  const k = curKey();
  if (!k) return;
  const inf = info(k);
  if (!inf) return;
  const chip = (cls, label, value, fn, title) => {
    const b = h("button", "chip " + cls);
    b.innerHTML = `${esc(label)} <b>${esc(value)}</b>`;
    b.title = title;
    b.onclick = (e) => fn(e);
    c.append(b);
  };
  $("#input").placeholder = inf.busy
    ? "Send while it works (Enter) · queue for after this turn (Alt+Enter)…"
    : inf.isOrch ? "Message the orchestrator…" : `Message ${inf.name}…`;
  chip("target", "to", inf.isOrch ? "orchestrator" : inf.name, () => goToThread(), "Switch conversation (Ctrl+P)");
  chip("", "access", permLabel(inf.permission_effective || S.state.permission), () => pickPermission(k), "Permission level");
  chip("", "mix", mixLabel(S.state.usage.mix), (e) => mixPopover(e.currentTarget), "Plan/API mix for this whole project");
  chip("", "model", modelLabel(inf.model), () => pickModel(), "Model for this conversation");
  chip("", "effort", effortLabel(inf.effort), () => pickEffort(), "Reasoning effort for this conversation");
  c.append(h("span", "sp"));
  const keys = h("span", "keys");
  keys.innerHTML = "<kbd>⏎</kbd> send · <kbd>⇧⏎</kbd> newline · <kbd>/</kbd> commands · <kbd>Ctrl</kbd><kbd>⇧</kbd><kbd>P</kbd>";
  c.append(keys);
}

// ---------------------------------------------------------------- messages between workspaces
// An agent asked to message a session in another workspace. Allow / Always allow / Deny go only
// over IPC. `x` has from_name, from_session, to_name, to_session, message, why; `id` is the
// need id or the request id (the backend takes either).
function xwsBlock(x, id, onSent) {
  const box = h("div", "xws");
  const route = h("div", "xws-route");
  route.append(document.createTextNode("From "), h("b", null, `${x.from_name} / ${x.from_session}`), document.createTextNode("  →  To "), h("b", null, `${x.to_name} / ${x.to_session}`));
  box.append(route, h("pre", "xws-msg", x.message || ""));
  if (x.why) box.append(h("div", "nd-why", "Why: " + x.why));
  const acts = h("div", "nd-acts");
  const go = (msg) => {
    if (!sendIpc({ op: "xws", id, ...msg })) return;
    $$("button", box).forEach((b) => { b.disabled = true; });
    // A focused note with text holds the Needs pane still (so typing isn't wiped): clear it,
    // say what happened, and redraw as soon as the new state is in.
    note.value = ""; note.blur(); denyRow.classList.add("hidden");
    acts.replaceChildren(h("span", "sub", msg.action === "deny" ? "Denied: nothing was sent." : msg.action === "allow_always" ? "Allowed, and always from now on. Sending…" : "Allowed once. Sending…"));
    onSent?.();
    setTimeout(() => loadState().then(() => renderNeedsPane(true)).catch(() => {}), 150);
  };
  const once = h("button", "btn primary", "Allow once");
  once.onclick = () => go({ action: "allow_once" });
  const always = h("button", "btn", `Always allow ${x.from_name} → ${x.to_name}`);
  always.title = "Also lets every session in " + x.from_name + " message " + x.to_name + " without asking again. You can undo it in Settings.";
  always.onclick = () => { if (confirm(`From now on, any session in ${x.from_name} can message any session in ${x.to_name} without asking you. Continue?`)) go({ action: "allow_always" }); };
  const deny = h("button", "btn danger", "Deny");
  const denyRow = h("div", "nd-menu hidden");
  const note = h("input", "nd-in");
  note.placeholder = "Why not? (optional, the agent sees it)";
  const dok = h("button", "btn danger", "Deny it");
  dok.onclick = () => go({ action: "deny", note: note.value.trim() });
  note.onkeydown = (e) => { if (e.key === "Enter") dok.onclick(); if (e.key === "Escape") denyRow.classList.add("hidden"); };
  denyRow.append(note, dok);
  deny.onclick = () => { denyRow.classList.toggle("hidden"); note.focus(); };
  acts.append(once, always, deny);
  box.append(acts, denyRow);
  return box;
}

const XWS_OUT = { allowed: "Sent (once).", always: "Sent. Always allowed from now on.", denied: "Denied.", expired: "The agent stopped waiting, so nothing was sent.", cancelled: "Cancelled.", rule: "Sent (standing rule)." };
function xwsRequestCard(ev) {
  const e = h("div", "ev xws-card");
  e.dataset.id = ev.id;
  e.append(h("div", "xws-head", "✉ Message to another workspace: needs your OK"));
  e.append(xwsBlock(ev, ev.id));
  return e;
}
function xwsResult(ev, log) {
  const card = log.querySelector(`.ev.xws-card[data-id="${ev.id}"]`);
  const text = (XWS_OUT[ev.outcome] || ev.outcome) + (ev.note ? " " + ev.note : "");
  if (!card) {
    log.append(h("div", "ev notice", ev.outcome === "rule" ? `→ ${text.replace(/\.$/, "")}` : text));
    return;
  }
  card.classList.add("done");
  $$("button, input", card).forEach((b) => { b.disabled = true; });
  card.append(h("div", "xws-out", text));
}

// ---------------------------------------------------------------- quieter chat: filter + search
const CHAT_FILTERS = [["all", "All"], ["needs", "Needs you"], ["me", "Messages to me"]];
let chatFilter = localStorage.getItem("chatFilter") || "all";
function applyChatFilter() {
  const log = $("#log");
  log.classList.toggle("f-needs", chatFilter === "needs");
  log.classList.toggle("f-me", chatFilter === "me");
  for (const b of $$("#chatbar .cf")) b.classList.toggle("on", b.dataset.f === chatFilter);
}
async function jumpToChat(key, ts) {
  await openChat(key);
  // The log may still be loading; try for a moment.
  for (let i = 0; i < 20; i++) {
    const nodes = $$("#log [data-ts]");
    const t = nodes.find((x) => +x.dataset.ts >= ts - 1e-6);
    if (t) { chatFilter = "all"; localStorage.setItem("chatFilter", "all"); applyChatFilter(); t.scrollIntoView({ block: "center" }); t.classList.add("flash"); setTimeout(() => t.classList.remove("flash"), 2000); return; }
    await new Promise((r) => setTimeout(r, 150));
  }
}
function initChatBar() {
  const bar = $("#chatbar");
  if (!bar) return;
  for (const [f, l] of CHAT_FILTERS) {
    const b = h("button", "cf", l);
    b.dataset.f = f;
    b.onclick = () => { chatFilter = f; localStorage.setItem("chatFilter", f); applyChatFilter(); scrollEnd(true); };
    bar.append(b);
  }
  bar.append(h("span", "sp"));
  const inp = h("input", "cs");
  inp.placeholder = "Search this workspace's chat…";
  inp.spellcheck = false;
  const res = h("div", "cs-res hidden");
  bar.append(inp, res);
  let timer = null, gen = 0;
  const run = async () => {
    const q = inp.value.trim();
    res.textContent = "";
    if (q.length < 2) return res.classList.add("hidden");
    const my = ++gen;
    let data;
    try { data = await api("/api/search?q=" + encodeURIComponent(q) + "&limit=30"); } catch (e) { return fail(e); }
    if (my !== gen) return;
    res.classList.remove("hidden");
    if (!data.hits.length) res.append(h("div", "sub", "Nothing found."));
    for (const x of data.hits) {
      const r = h("div", "cs-hit");
      r.append(h("span", "qchip", x.thread), h("span", "sn", x.snippet), h("span", "sub", relTime(x.ts) === "now" ? "just now" : relTime(x.ts) + " ago"));
      r.onclick = () => { res.classList.add("hidden"); jumpToChat(x.key, x.ts); };
      res.append(r);
    }
    if (data.truncated) res.append(h("div", "sub", "There are more. Try a longer search."));
  };
  inp.oninput = () => { clearTimeout(timer); timer = setTimeout(run, 300); };
  inp.onkeydown = (e) => { if (e.key === "Escape") { inp.value = ""; res.classList.add("hidden"); inp.blur(); } if (e.key === "Enter") { clearTimeout(timer); run(); } };
  document.addEventListener("mousedown", (e) => { if (!bar.contains(e.target)) res.classList.add("hidden"); });
  applyChatFilter();
}

// ---------------------------------------------------------------- busy mode
function busyPopover(anchor) {
  const pop = $("#mixpop");
  if (!pop.classList.contains("hidden") && pop.dataset.kind === "busy") { pop.classList.add("hidden"); return; }
  pop.dataset.kind = "busy";
  pop.textContent = "";
  pop.append(h("div", "mix-title", "I'm busy"), h("div", "mix-sub", "Holds every notification except an order about to miss its deadline. You get one summary when it ends."));
  const row = h("div", "row");
  for (const [l, m] of [["1 hour", 60], ["2 hours", 120], ["Until I turn it off", 0]]) {
    const b = h("button", "btn", l);
    b.onclick = () => { pop.classList.add("hidden"); sendIpc({ op: "busy_set", on: true, minutes: m }); };
    row.append(b);
  }
  pop.append(row);
  pop.classList.remove("hidden");
  const a = anchor.getBoundingClientRect();
  const w = 340;
  pop.style.left = Math.max(8, Math.min(innerWidth - w - 8, a.left)) + "px";
  pop.style.top = "";
  pop.style.bottom = (innerHeight - a.top + 6) + "px";
}

function renderStatus() {
  const s = $("#status");
  s.textContent = "";
  const item = (cls, html, fn, title) => {
    const b = h(fn ? "button" : "span", "sb " + cls);
    b.innerHTML = html;
    if (fn) b.onclick = fn;
    if (title) b.title = title;
    s.append(b);
    return b;
  };
  item("brand", "◈ Orchestrator", () => palette(), "Command palette");
  item("", `<span>⌂</span><span class="mono">${esc(baseName(S.state.workspace))}</span>`, () => pickWorkspace(), S.state.workspace);
  const running = S.state.running;
  item(running ? "run" : "", running ? `<span class="st running"></span>${running} running` : "✓ idle", () => openManage(), "Manage threads");
  if (S.state.waiting) {
    item("wait", `? ${S.state.waiting} waiting for you`, () => {
      const t = S.state.threads.find((x) => x.needs_input);
      openChat(S.state.orchestrator.needs_input ? orchKey() : t ? t.name : orchKey());
    }, "A thread is asking you a question");
  }
  const u = S.state.usage;
  const upd = S.state.app?.update;
  if (upd) item("wait", `⬆ update ${esc(upd.version)}`, () => updateNow(upd), "A new version of Orchestrator is available: click to update");
  const pu = S.state.plan_usage?.unifiedWindows;
  if (pu?.five_hour) {
    const f = pu.five_hour.utilization, w = pu.seven_day?.utilization ?? 0;
    const worst = Math.max(f, w);
    const puItem = item(worst >= 1 ? "pu-full" : worst >= 0.8 ? "pu-warn" : "",
      `◔ plan <span class="mono">${Math.round(f * 100)}%</span>`, null, "Plan usage: 5-hour window (click for details)");
    puItem.style.cursor = "pointer";
    puItem.onclick = () => usagePopover(puItem);
  } else {
    const puItem = item("", "◔ plan usage", null, "Check your plan's usage limits");
    puItem.style.cursor = "pointer";
    puItem.onclick = () => usagePopover(puItem);
  }
  const pj = S.state.project || {};
  if (pj.kind === "business") {
    const sp = S.state.spend || { day: 0, week: 0 };
    const capD = pj.budget_day || 0, capW = pj.budget_week || 0;
    const ratio = Math.max(capD ? sp.day / capD : 0, capW ? sp.week / capW : 0);
    const amt = (v) => (v < 10 ? v.toFixed(2) : v.toFixed(0));
    const money = capD ? `$${sp.day.toFixed(2)} / $${amt(capD)} today` : capW ? `$${sp.week.toFixed(2)} / $${amt(capW)} this week` : `$${sp.day.toFixed(2)} today`;
    item(ratio >= 1 ? "pu-full" : ratio >= 0.8 ? "pu-warn" : "", `$ <span class="mono">${esc(money)}</span>`, () => openSettings(),
      `Spend at API rates (plan and API turns): $${sp.day.toFixed(2)} today, $${sp.week.toFixed(2)} this week.` + (capD || capW ? ` Caps: ${capD ? "$" + capD + "/day" : "no daily cap"}, ${capW ? "$" + capW + "/week" : "no weekly cap"}. Over a cap, only paid-order threads start.` : " No caps set (Settings → Budget)."));
    if (pj.paused) item("pu-full", "⏸ workspace paused", () => openQueue(), "This workspace is paused: nothing runs unless you send it a message. Resume it in the Queue tab.");
    if (pj.outbound_paused_all) item("pu-full", "⏸ outbound paused (all)", null, "Outbound is paused for every business workspace from the tray menu. Untick it there to resume.");
    else item(pj.outbound_paused ? "pu-full" : "", pj.outbound_paused ? "⏸ outbound paused" : "⏸ pause outbound",
      () => sendIpc({ op: "set_outbound_paused", paused: !pj.outbound_paused }),
      pj.outbound_paused ? "Kill switch is on: agents get no approved items. Click to resume." : "Kill switch: stop agents from executing or sending anything until you resume");
  }
  {
    const bz = S.state.busy_mode;
    if (bz?.on) {
      const until = bz.until ? ` until ${new Date(bz.until * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}` : "";
      item("pu-warn", `🔕 busy${esc(until)}${bz.held ? ` · ${bz.held} held` : ""}`, () => sendIpc({ op: "busy_set", on: false }), "Busy mode is on: notifications are held (except an order about to miss its deadline). Click to turn it off and get one summary.");
    } else {
      const b = item("", "🔕 busy?", null, "Hold notifications while you focus");
      b.onclick = () => busyPopover(b);
    }
    if (pj.kind === "business" && pj.held) {
      const nx = pj.next_digest ? ` · next ${new Date(pj.next_digest * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}` : "";
      item("", `⏳ ${pj.held} held${esc(nx)}`, () => openSettings(), "Notifications waiting for the next summary. Change the times in Settings.");
    }
  }
  item(S.state.permission === "bypassPermissions" ? "perm-open" : "", `🛡 ${esc(permLabel(S.state.permission))}`, () => pickPermission(orchKey()), "Permission level for this project");
  const mixItem = item("", `⚖ <span class="mono">${esc(mixLabel(u.mix))}</span>`, null, "Plan/API mix for this project");
  mixItem.style.cursor = "pointer";
  mixItem.onclick = () => mixPopover(mixItem);
  if (!S.state.defaults.api_key_set) item("", '<span class="warn">⚠</span> no API key', () => openSettings(), "Without an API key everything runs on your plan");
  s.append(h("span", "sb-sp"));
  const k = curKey();
  const inf = k && info(k);
  if (inf) {
    item("", `<span class="mono">${esc(inf.isOrch ? "orchestrator" : inf.name)}</span>`, () => goToThread());
    item("", `<span class="mono">${esc(modelLabel(inf.model))}</span>`, () => pickModel(), "Model");
    item("", `effort <span class="mono">${esc(effortLabel(inf.effort))}</span>`, () => pickEffort(), "Reasoning effort");
    item("", `<span class="mono">${money(inf.cost_usd)}</span>`, null, "Estimated cost at API prices, plan and API turns together (plan turns use your plan, not credits)");
  }
  item("", theme(S.state.defaults.theme) === "dark" ? "☾" : "☀", () => toggleTheme(), "Toggle theme");
}

// ================================================================ quick input (palette / pickers)

let qiResolve = null;
let qiItems = [];
let qiIndex = 0;
let qiAllowText = false;

function quickPick(items, placeholder, { allowText = false, value = "" } = {}) {
  closeSuggest();
  return new Promise((resolve) => {
    if (qiResolve) qiResolve(null);
    qiResolve = resolve;
    qiItems = items;
    qiAllowText = allowText;
    qiIndex = Math.max(0, items.findIndex((i) => i.current));
    const input = $("#qi-input");
    input.value = value;
    input.placeholder = placeholder;
    $("#qi").classList.remove("hidden");
    renderQi();
    input.focus();
    input.select();
  });
}

function fuzzy(q, text) {
  if (!q) return { ok: true, html: esc(text), score: 0 };
  const t = text.toLowerCase(), ql = q.toLowerCase();
  const idx = t.indexOf(ql);
  if (idx !== -1) return { ok: true, score: idx, html: esc(text.slice(0, idx)) + "<em>" + esc(text.slice(idx, idx + q.length)) + "</em>" + esc(text.slice(idx + q.length)) };
  let j = 0, html = "";
  for (let i = 0; i < text.length; i++) {
    if (j < ql.length && t[i] === ql[j]) { html += "<em>" + esc(text[i]) + "</em>"; j++; } else html += esc(text[i]);
  }
  return j === ql.length ? { ok: true, html, score: 100 + text.length } : { ok: false };
}

function renderQi() {
  const q = $("#qi-input").value.trim();
  const list = $("#qi-list");
  list.textContent = "";
  const shown = [];
  for (const it of qiItems) {
    const f = fuzzy(q, (it.cat ? it.cat + ": " : "") + it.label);
    if (f.ok) shown.push({ it, f });
  }
  if (q) shown.sort((a, b) => a.f.score - b.f.score);
  qiIndex = Math.min(qiIndex, Math.max(0, shown.length - 1));
  shown.forEach(({ it, f }, i) => {
    const r = h("div", "qr" + (i === qiIndex ? " on" : "") + (it.sep && !q ? " sep" : ""));
    const l = h("span", "l");
    l.innerHTML = f.html;
    r.append(l);
    if (it.current) r.append(h("span", "k", "current"));
    if (it.desc) r.append(h("span", "d", it.desc));
    if (it.key) r.append(h("span", "k", it.key));
    r.onmousedown = (e) => { e.preventDefault(); finishQi(it); };
    list.append(r);
  });
  if (!shown.length) list.append(h("div", "qi-empty", qiAllowText && q ? `Press Enter to use “${q}”` : "No matches"));
  list.children[qiIndex]?.scrollIntoView({ block: "nearest" });
  S._qiShown = shown;
}

function finishQi(item) {
  $("#qi").classList.add("hidden");
  const r = qiResolve;
  qiResolve = null;
  if (r) r(item);
  // Back to the composer, unless the choice opened another picker (e.g. "New folder…"):
  // taking its focus would close it.
  if (curKey()) setTimeout(() => { if ($("#qi").classList.contains("hidden")) $("#input").focus(); }, 0);
}

function wireQi() {
  const input = $("#qi-input");
  input.addEventListener("input", () => { qiIndex = 0; renderQi(); });
  input.addEventListener("keydown", (e) => {
    const shown = S._qiShown || [];
    if (e.key === "ArrowDown") { e.preventDefault(); qiIndex = Math.min(qiIndex + 1, shown.length - 1); renderQi(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); qiIndex = Math.max(qiIndex - 1, 0); renderQi(); }
    else if (e.key === "Enter") {
      e.preventDefault();
      const pick = shown[qiIndex]?.it;
      const text = input.value.trim();
      if (pick) finishQi(pick);
      else if (qiAllowText && text) finishQi({ text });
    } else if (e.key === "Escape") { e.preventDefault(); finishQi(null); }
  });
  input.addEventListener("blur", () => setTimeout(() => { if (!$("#qi").classList.contains("hidden") && document.activeElement !== input) finishQi(null); }, 150));
}

// pickers ---------------------------------------------------------

async function configure(key, fields) {
  if (isOrch(key)) await api("/api/orchestrator", { body: fields });
  else await api("/api/threads/" + encodeURIComponent(key), { method: "PATCH", body: fields });
  await loadState();
}

async function pickModel(key = curKey()) {
  const inf = info(key);
  const items = [{ label: "default", desc: "Claude Code's default model", value: "", current: !inf.model },
    ...MODELS.map((m) => ({ label: m, value: m, current: inf.model === m }))];
  const it = await quickPick(items, `Model for ${inf.isOrch ? "the orchestrator" : inf.name} (or type any model id)`, { allowText: true });
  if (!it) return;
  const model = it.text ?? it.value;
  await configure(key, { model }).then(() => toast(`model: ${model || "default"}`)).catch(fail);
}

const EFFORT_DESC = { low: "fastest, cheapest", medium: "balanced", high: "thorough", xhigh: "very thorough (good for agentic coding)", max: "maximum reasoning" };
async function pickEffort(key = curKey()) {
  const inf = info(key);
  const items = [{ label: "default", desc: "the model's default", value: "", current: !inf.effort },
    ...EFFORTS.map((e) => ({ label: e, desc: EFFORT_DESC[e], value: e, current: inf.effort === e }))];
  const it = await quickPick(items, `Effort for ${inf.isOrch ? "the orchestrator" : inf.name}`);
  if (it) await configure(key, { effort: it.value }).then(() => toast(`effort: ${it.value || "default"}`)).catch(fail);
}

async function pickPermission(key = curKey()) {
  const thread = key && !isOrch(key) ? threadRow(key) : null;
  const project = S.state.permission;
  const items = [];
  if (thread) items.push({ label: `Project default (${permLabel(project)})`, desc: "follow the project's level", value: "", current: !thread.permission });
  for (const [v, l, d] of PERMS) items.push({ label: l, desc: d, value: v, current: thread ? thread.permission === v : project === v });
  const it = await quickPick(items, thread ? `Permission level for ${thread.name}` : "Permission level for this project (every thread without its own)");
  if (!it) return;
  try {
    await ipcCall("permission", { thread: thread ? thread.name : "", level: it.value });
    await loadState();
    toast(`${thread ? thread.name : "project"}: ${it.value ? permLabel(it.value) : "project default"} (from the next turn)`);
  } catch (e) { fail(e); }
}

async function goToThread() {
  const o = S.state.orchestrator;
  const items = [{ label: "orchestrator", desc: baseName(S.state.workspace), key: "Ctrl+1", value: o.name },
    ...S.state.threads.map((t) => ({ label: t.name, desc: `${stClass(t)} · ${modelLabel(t.model)} · ${effortLabel(t.effort)}`, value: t.name }))];
  const it = await quickPick(items, "Go to conversation");
  if (it) openChat(it.value);
}

async function pickFolder(start, title) {
  try { return (await api("/api/pick-folder", { body: { start: start || "", title } })).path; }
  catch (e) { fail(e); return null; }
}

async function pickWorkspace() {
  const items = [
    { label: "Open folder…", desc: "browse", value: "__browse" },
    { label: "New folder…", desc: "type a path to create", value: "__new" },
    { label: "Import a Claude Code project…", desc: "bring over its history, rules and unfinished work", value: "__import" },
    ...S.state.recent_workspaces.map((p, i) => ({ label: baseName(p), desc: p, value: p, current: p === S.state.workspace, sep: i === 0 })),
  ];
  const it = await quickPick(items, "Open a workspace (each has its own orchestrator and threads, and they all run at once) — or paste a path", { allowText: true });
  if (!it) return;
  let path = it.text ?? it.value, create = false;
  if (path === "__import") return importProject();
  if (path === "__browse") path = await pickFolder(S.state.workspace, "Open workspace folder");
  else if (path === "__new") {
    const t = await quickPick([], "Full path of the new folder", { allowText: true, value: S.state.workspace + "\\" });
    path = t?.text; create = true;
  }
  if (!path) return;
  await switchWorkspace(path, create);
  toast("workspace: " + baseName(S.state.workspace));
}

async function threadMenu(name) {
  const items = [
    { label: "Open", value: "open" }, { label: "Open to the side", desc: "Alt+click", value: "side" },
    { label: "Open in new window", desc: "Shift+click", value: "pop" }, { label: "Stop", value: "stop" },
    { label: "Set model…", value: "model" }, { label: "Set permission level…", value: "perm" }, { label: "Set effort…", value: "effort" },
    threadRow(name)?.resolved
      ? { label: "Reopen", desc: "move back to the open threads", value: "reopen" }
      : { label: "Resolve", desc: "done with it: tuck it away", value: "resolve" },
    { label: "Delete", desc: "forget the conversation; files stay", value: "delete" },
  ];
  const it = await quickPick(items, name);
  if (!it) return;
  if (it.value === "open") openChat(name);
  if (it.value === "side") openSide(name);
  if (it.value === "pop") popOut(name);
  if (it.value === "stop") api("/api/stop", { body: { thread: name } }).then((r) => toast(r.stopped ? "stopped" : "not running")).catch(fail);
  if (it.value === "model") pickModel(name);
  if (it.value === "perm") pickPermission(name);
  if (it.value === "effort") pickEffort(name);
  if (it.value === "resolve") setResolved(name, true);
  if (it.value === "reopen") setResolved(name, false);
  if (it.value === "delete") deleteThread(name);
}

async function toggleTheme() {
  const next = theme(S.state.defaults.theme) === "dark" ? "light" : "dark";
  await api("/api/settings", { body: { theme: next } }).catch(fail);
  await loadState();
}

// ================================================================ commands

const COMMANDS = [
  { cmd: "/new", args: "<name> [task]", desc: "Create a thread, optionally starting it", cat: "Thread", key: "Ctrl+N", run: (a) => newThread(a) },
  { cmd: "/open", args: "<thread>", desc: "Open a conversation", cat: "Go", key: "Ctrl+P", complete: "thread", run: (a) => a ? openChat(a === "orchestrator" ? orchKey() : a) : goToThread() },
  { cmd: "/to", args: "<thread> <message>", desc: "Message a thread without leaving this view", cat: "Thread", complete: "thread", run: (a) => sendTo(a) },
  { cmd: "/resolve", args: "[thread]", desc: "Mark a thread resolved (or reopen it)", cat: "Thread", complete: "thread", run: (a) => {
    const n = a?.trim() || (curKey() && !isOrch(curKey()) ? curKey() : "");
    if (!n) return toast("open a thread or name one", true);
    return setResolved(n, !threadRow(n)?.resolved);
  } },
  { cmd: "/handoff", args: "", desc: "Write a handoff and continue in a new session", cat: "Thread", run: () => handoff() },
  { cmd: "/stop", args: "[thread]", desc: "Stop the running turn (Esc)", cat: "Thread", complete: "thread", run: (a) => stopTurn(a) },
  { cmd: "/effort", args: "<level>", desc: "Reasoning effort for this conversation", cat: "Config", complete: "effort", run: (a) => a ? setField("effort", a === "default" ? "" : a) : pickEffort() },
  { cmd: "/model", args: "<model>", desc: "Model for this conversation", cat: "Config", complete: "model", run: (a) => a ? setField("model", a === "default" ? "" : a) : pickModel() },
  { cmd: "/permissions", args: "[level]", desc: "Permission level (project, or this thread)", cat: "Config", complete: "perm", run: (a) => {
    if (!a) return pickPermission(curKey());
    const p = PERMS.find((x) => x[0].toLowerCase() === a.toLowerCase() || x[1].toLowerCase().startsWith(a.toLowerCase()));
    if (!p) throw new Error("levels: " + PERMS.map((x) => x[0]).join(", "));
    const k = curKey();
    return ipcCall("permission", { thread: k && !isOrch(k) ? k : "", level: p[0] }).then(loadState).then(() => toast(`permission: ${p[1]}`));
  } },
  { cmd: "/usage", args: "", desc: "Show your plan's usage limits", cat: "Config", run: () => usagePopover($("#composer")) },
  { cmd: "/mix", args: "<0-10>", desc: "Plan/API mix for the whole project (0 = all plan, 10 = all API)", cat: "Config", complete: "mix", run: (a) => {
    if (a === "") return mixPopover($("#composer"));
    const v = Number(a);
    if (!Number.isInteger(v) || v < 0 || v > 10) throw new Error("usage: /mix 0-10 (0 = all plan, 10 = all API)");
    return setMix(v);
  } },
  { cmd: "/reset", args: "", desc: "Start a new orchestrator conversation", cat: "Orchestrator", run: () => resetOrch() },
  { cmd: "/workspace", args: "[path]", desc: "Switch workspace", cat: "Workspace", complete: "workspace", run: (a) => a ? switchTo(a) : pickWorkspace() },
  { cmd: "/threads", args: "", desc: "Open the thread manager", cat: "View", run: () => openManage() },
  { cmd: "/import", args: "", desc: "Import a Claude Code project (history, rules, unfinished work)", cat: "Workspace", run: () => importProject() },
  { cmd: "/brief", args: "", desc: "Show the project brief and rules", cat: "View", run: () => openBrief() },
  { cmd: "/side", args: "<thread>", desc: "Open a thread beside this chat", cat: "View", complete: "thread", run: (a) => a ? openSide(a) : closeSide() },
  { cmd: "/popout", args: "[thread]", desc: "Open a conversation in its own window", cat: "View", complete: "thread", run: (a) => popOut(a ? (a === "orchestrator" ? orchKey() : a) : curKey()) },
  { cmd: "/settings", args: "", desc: "Open settings", cat: "View", key: "Ctrl+,", run: () => openSettings() },
  { cmd: "/delete", args: "<thread>", desc: "Forget a thread (files stay on disk)", cat: "Thread", complete: "thread", run: (a) => deleteThread(a) },
  { cmd: "/auth", args: "", desc: "Check which login plan and API use", cat: "Config", run: () => { openSettings(); setTimeout(runAuth, 50); } },
  { cmd: "/theme", args: "dark|light", desc: "Switch color theme", cat: "View", complete: "theme", run: (a) => a ? api("/api/settings", { body: { theme: a } }).then(loadState) : toggleTheme() },
  { cmd: "/retry", args: "", desc: "Run your last message again", cat: "Message", run: () => {
    const users = $$("#log .ev.user[data-ts]");
    const u = users[users.length - 1];
    if (!u) throw new Error("nothing to retry");
    return resend(+u.dataset.ts, u.dataset.text);
  } },
  { cmd: "/edit", args: "", desc: "Edit your last message", cat: "Message", run: () => {
    const users = $$("#log .ev.user[data-ts]");
    const u = users[users.length - 1];
    if (!u) throw new Error("nothing to edit");
    u.scrollIntoView({ block: "center" });
    startEdit(u);
  } },
  { cmd: "/clear", args: "", desc: "Clear the screen (history stays on disk)", cat: "View", run: () => { $("#log").textContent = ""; } },
  { cmd: "/help", args: "", desc: "Show all commands", cat: "Help", run: () => palette() },
];

async function setField(field, value) {
  const k = curKey();
  if (!k) throw new Error("open a conversation first");
  await configure(k, { [field]: value });
  toast(`${field}: ${value || "default"}`);
}

async function sendTo(args) {
  const m = (args || "").match(/^(\S+)\s+([\s\S]+)$/);
  if (!m) throw new Error("usage: /to <thread> <message>");
  const target = m[1] === "orchestrator" ? orchKey() : m[1];
  await ipcCall("send", { thread: target, text: m[2] });
  toast(`sent to ${m[1]}`);
}

async function stopTurn(name) {
  const target = name ? (name === "orchestrator" ? orchKey() : name) : curKey();
  const r = await api("/api/stop", { body: { thread: target } });
  toast(r.stopped ? "stopped" : "nothing running");
}

async function resetOrch() {
  await api("/api/orchestrator", { body: { reset: true } });
  await loadState();
  if (curKey() === orchKey()) loadLog(orchKey());
  else openChat(orchKey());
}

async function switchTo(path) {
  await switchWorkspace(path, false);
}

async function deleteThread(name) {
  if (!name) throw new Error("usage: /delete <thread>");
  if (!confirm(`Delete thread "${name}"?\n\nIts conversation is removed. Files it changed stay on disk.`)) return;
  await api("/api/threads/" + encodeURIComponent(name), { method: "DELETE" }).catch(fail);
  await loadState();
  if (cur().type === "chat") activate(S.active);
  toast(`deleted ${name}`);
}

async function runCommand(line) {
  const m = line.trim().match(/^(\/\S+)\s*([\s\S]*)$/);
  const c = m && COMMANDS.find((x) => x.cmd === m[1].toLowerCase());
  if (!c) throw new Error(`unknown command ${m ? m[1] : line} — try /help`);
  await c.run(m[2].trim());
}

async function palette() {
  const items = [
    ...COMMANDS.map((c) => ({ cat: c.cat, label: c.desc, desc: c.cmd, key: c.key, value: () => c.run("") })),
    { cat: "Thread", label: "Go to thread…", key: "Ctrl+P", value: goToThread },
    { cat: "View", label: "Toggle sidebar", key: "Ctrl+B", value: toggleSide },
    { cat: "Workspace", label: "Open workspace in VS Code", value: () => openPath(S.state.workspace, "editor") },
    { cat: "Workspace", label: "Import a Claude Code project…", value: importProject },
    { cat: "Workspace", label: "Reveal workspace in Explorer", value: () => openPath(S.state.workspace, "reveal") },
    { cat: "Message", label: "Attach files…", value: () => $("#file-input").click() },
  ];
  const it = await quickPick(items, "Type a command");
  if (it) Promise.resolve(it.value()).catch(fail);
}

// ================================================================ composer + slash suggestions

function autosize() {
  const i = $("#input");
  i.style.height = "auto";
  i.style.height = Math.min(i.scrollHeight, 280) + "px";
}

function suggestions(value) {
  if (!value.startsWith("/") || value.includes("\n")) return null;
  const sp = value.indexOf(" ");
  if (sp === -1) {
    const q = value.toLowerCase();
    return COMMANDS.filter((c) => c.cmd.startsWith(q)).map((c) => ({
      c: `<em>${esc(c.cmd.slice(0, q.length))}</em>${esc(c.cmd.slice(q.length))}`, a: c.args, d: c.desc,
      insert: c.cmd + (c.args ? " " : ""), exec: !c.args,
    }));
  }
  const cmd = COMMANDS.find((c) => c.cmd === value.slice(0, sp).toLowerCase());
  const rest = value.slice(sp + 1);
  if (!cmd?.complete || rest.includes(" ")) return null;
  let vals = [];
  if (cmd.complete === "thread") vals = [...(cmd.cmd === "/to" || cmd.cmd === "/open" || cmd.cmd === "/stop" ? [{ v: "orchestrator", d: "" }] : []),
    ...S.state.threads.map((t) => ({ v: t.name, d: `${stClass(t)} · ${effortLabel(t.effort)}` }))];
  if (cmd.complete === "effort") vals = [{ v: "default", d: "the model's default" }, ...EFFORTS.map((e) => ({ v: e, d: EFFORT_DESC[e] }))];
  if (cmd.complete === "model") vals = [{ v: "default", d: "" }, ...MODELS.map((m) => ({ v: m, d: "" }))];
  if (cmd.complete === "perm") vals = PERMS.map(([v, l, d]) => ({ v, d: `${l}: ${d}` }));
  if (cmd.complete === "mix") vals = [...Array(11).keys()].map((m) => ({ v: String(m), d: mixLabel(m) }));
  if (cmd.complete === "workspace") vals = S.state.recent_workspaces.map((p) => ({ v: p, d: baseName(p) }));
  if (cmd.complete === "theme") vals = [{ v: "dark", d: "" }, { v: "light", d: "" }];
  const q = rest.toLowerCase();
  return vals.filter((x) => x.v.toLowerCase().includes(q)).map((x) => ({
    c: esc(x.v), a: "", d: x.d, insert: `${cmd.cmd} ${x.v}${cmd.cmd === "/to" ? " " : ""}`, exec: cmd.cmd !== "/to" && cmd.cmd !== "/delete",
  }));
}

function renderSuggest() {
  const box = $("#suggest");
  const items = suggestions($("#input").value);
  if (!items?.length) return closeSuggest();
  S.suggest.items = items;
  S.suggest.i = Math.min(S.suggest.i, items.length - 1);
  box.textContent = "";
  items.forEach((it, i) => {
    const r = h("div", "sg" + (i === S.suggest.i ? " on" : ""));
    r.innerHTML = `<span class="c">${it.c}</span><span class="a">${esc(it.a)}</span><span class="d">${esc(it.d)}</span>`;
    r.onmousedown = (e) => { e.preventDefault(); S.suggest.i = i; acceptSuggest(); };
    box.append(r);
  });
  box.classList.remove("hidden");
  box.children[S.suggest.i]?.scrollIntoView({ block: "nearest" });
}

function closeSuggest() { $("#suggest").classList.add("hidden"); S.suggest.items = []; }

function acceptSuggest() {
  const it = S.suggest.items[S.suggest.i];
  if (!it) return;
  const i = $("#input");
  i.value = it.insert;
  S.suggest.i = 0;
  autosize();
  if (it.exec) return submit();
  renderSuggest();
  i.focus();
}

async function submit(queue = false) {
  const i = $("#input");
  let text = i.value.trim();
  if (!text && !S.attachments.length) return;
  if (S.attachments.some((a) => a.uploading)) return toast("still uploading…");
  const attachments = S.attachments.filter((a) => a.path).map((a) => a.path);
  if (!text && attachments.length) text = "(see attached)";
  i.value = "";
  autosize();
  closeSuggest();
  try {
    if (text.startsWith("/")) await runCommand(text);
    else {
      const r = await ipcCall("send", { thread: curKey(), text, attachments, queue });
      if (r.queued) toast("queued — runs after the current turn");
      else if (r.delivery === "injected") toast("sent — it'll read this at its next step");
      S.attachments = [];
      renderTray();
    }
  } catch (e) {
    fail(e);
    i.value = text;
    autosize();
  }
}

// ================================================================ new thread dialog

async function newThread(args) {
  const m = (args || "").match(/^(\S+)\s*([\s\S]*)$/);
  if (m) {
    await ipcCall("thread_create", { name: m[1], task: m[2] });
    await loadState();
    return openChat(m[1]);
  }
  $("#dlg-bg").classList.remove("hidden");
  for (const id of ["nt-name", "nt-task", "nt-model", "nt-cwd"]) $("#" + id).value = "";
  $("#nt-effort").value = "__default";
  $("#nt-cwd").placeholder = S.state.workspace;
  $("#nt-err").textContent = "";
  setTimeout(() => $("#nt-name").focus(), 20);
}

async function createFromDialog() {
  const effort = $("#nt-effort").value;
  const body = {
    name: $("#nt-name").value.trim(), task: $("#nt-task").value,
    model: $("#nt-model").value, cwd: $("#nt-cwd").value,
  };
  if (effort !== "__default") body.effort = effort;
  try {
    await ipcCall("thread_create", body);
    closeDialog();
    await loadState();
    openChat(body.name);
  } catch (e) { $("#nt-err").textContent = e.message; }
}
const closeDialog = () => $("#dlg-bg").classList.add("hidden");

// ================================================================ manage page

async function renderManage() {
  const pane = $("#pane-manage");
  let rows = S.state.threads;
  if (S.manageScope === "all") {
    try { rows = (await api("/api/all-threads")).threads; } catch (e) { return fail(e); }
  }
  if (!changed("manage", [rows, S.manageScope, S.manageFilter]) && $(".page", pane)) return;
  const f = S.manageFilter.toLowerCase();
  const shown = rows.filter((r) => !f || r.name.toLowerCase().includes(f) || r.cwd.toLowerCase().includes(f));
  const scrollTop = $(".page", pane)?.scrollTop || 0;
  pane.textContent = "";
  const page = h("div", "page");
  page.append(h("h1", null, "Threads"), h("div", "sub", S.manageScope === "all" ? "All workspaces" : S.state.workspace));

  const tb = h("div", "toolbar");
  const seg = h("div", "seg");
  for (const [v, l] of [["ws", "This workspace"], ["all", "All workspaces"]]) {
    const b = h("button", S.manageScope === v ? "on" : "", l);
    b.onclick = () => { S.manageScope = v; renderManage(); };
    seg.append(b);
  }
  const search = h("input", "search");
  search.placeholder = "Filter by name or folder";
  search.value = S.manageFilter;
  search.oninput = () => { S.manageFilter = search.value; renderManage().then(() => { const s = $(".search"); s.focus(); s.setSelectionRange(s.value.length, s.value.length); }); };
  const nb = h("button", "btn primary", "+ New thread");
  nb.onclick = () => newThread("");
  tb.append(seg, search, h("span", "sb-sp"), nb);
  page.append(tb);

  const stats = h("div", "stats");
  const stat = (v, k) => { const d = h("div"); d.append(h("div", "v", String(v)), h("div", "k", k)); stats.append(d); };
  stat(shown.length, "threads");
  stat(shown.filter((r) => r.busy).length, "running");
  stat(shown.reduce((a, r) => a + r.turns, 0), "turns");
  stat(money(shown.reduce((a, r) => a + r.cost_usd, 0)), "est. at API prices (plan + API)");
  page.append(stats);

  const table = h("table", "grid");
  table.innerHTML = "<tr><th>Thread</th><th>Status</th><th>Access</th><th>Model</th><th>Effort</th><th class='num'>Turns</th><th class='num'>Cost</th><th>Folder</th><th>Updated</th><th></th></tr>";
  for (const r of [...shown].sort((a, b) => b.updated_at - a.updated_at)) {
    const tr = h("tr");
    const here = r.workspace.toLowerCase() === S.state.workspace.toLowerCase();
    const name = h("td");
    const nm = h("span", "name", r.name);
    nm.title = here ? "Open" : "In " + r.workspace;
    nm.onclick = async () => { if (!here) await switchTo(r.workspace); openChat(r.name); };
    name.append(nm);
    const st = h("td");
    const sc = h("span", "st-cell");
    sc.append(h("span", "st " + stClass(r)), document.createTextNode(stClass(r)));
    st.append(sc);
    const sel = (opts, value, field) => {
      const td = h("td");
      const s = h("select");
      for (const [v, l] of opts) { const o = h("option", null, l); o.value = v; s.append(o); }
      s.value = value;
      s.onchange = () => (field === "permission"
        ? ipcCall("permission", { thread: r.name, level: s.value }).then(loadState)
        : configure(r.name, { [field]: s.value })).then(() => toast(`${r.name}: ${field} ${s.value || "default"}`)).catch(fail);
      td.append(s);
      return td;
    };
    const model = h("td");
    const mi = h("input");
    mi.setAttribute("list", "models");
    mi.value = r.model;
    mi.placeholder = "default";
    mi.onchange = () => configure(r.name, { model: mi.value }).then(() => toast(`${r.name}: model ${mi.value || "default"}`)).catch(fail);
    model.append(mi);
    const folder = h("td");
    const p = h("div", "path", r.cwd);
    p.title = r.cwd;
    folder.append(p);
    const acts = h("td", "acts");
    if (r.busy) {
      const b = h("button", "btn sm", "Stop");
      b.onclick = () => stopTurn(r.name).catch(fail);
      acts.append(b);
    }
    const del = h("button", "btn sm danger", "Delete");
    del.onclick = () => deleteThread(r.name);
    acts.append(del);
    tr.append(name, st,
      sel([["", `project (${permLabel(S.state.permission)})`], ...PERMS.map(([v, l]) => [v, l])], r.permission || "", "permission"),
      model,
      sel([["", "default"], ...EFFORTS.map((e) => [e, e])], r.effort, "effort"),
      h("td", "num", String(r.turns)), h("td", "num", money(r.cost_usd)), folder,
      h("td", "mono", relTime(r.updated_at)), acts);
    table.append(tr);
  }
  page.append(table);
  page.append(await efficiencyView());
  page.append(await timingView());
  if (!shown.length) page.append(h("div", "sub", "\nNo threads. Ask the orchestrator to start some, or create one."));
  pane.append(page);
  page.scrollTop = scrollTop;
}

// ---------------------------------------------------------------- efficiency

async function efficiencyView() {
  const box = h("div");
  box.append(h("div", "set-section", "Efficiency · last 7 days"));
  let e;
  try { e = await api("/api/efficiency"); } catch (err) { box.append(h("div", "sub", err.message)); return box; }
  if (!e.turns) {
    box.append(h("div", "sub", "No finished thread turns recorded yet. The orchestrator uses these stats to tune which model and effort it picks."));
    return box;
  }
  box.append(h("div", "sub", `${e.turns} thread turns · ${money(e.total_cost)} estimated at API prices (plan and API turns). Verdicts are the orchestrator's: right fit / needed stronger / could be cheaper.`));
  const t = h("table", "grid");
  t.innerHTML = "<tr><th>Model</th><th>Effort</th><th class='num'>Turns</th><th class='num'>Avg cost</th><th class='num'>Total</th><th class='num'>Avg time</th><th class='num'>Avg steps</th><th class='num'>Errors</th><th>Verdicts</th></tr>";
  for (const g of [...e.groups].sort((a, b) => b.total_cost - a.total_cost)) {
    const tr = h("tr");
    const verdicts = [g.right && `✓ ${g.right}`, g.too_weak && `↑ ${g.too_weak} weak`, g.too_strong && `↓ ${g.too_strong} cheaper`].filter(Boolean).join("  ") || "—";
    tr.innerHTML = `<td class="mono">${esc(modelLabel(g.model))}</td><td class="mono">${esc(g.effort)}</td><td class="num">${g.turns}</td>
      <td class="num">${money(g.avg_cost)}</td><td class="num">${money(g.total_cost)}</td><td class="num">${g.avg_seconds.toFixed(0)}s</td>
      <td class="num">${g.avg_steps.toFixed(1)}</td><td class="num">${g.errors}</td><td class="mono">${esc(verdicts)}</td>`;
    t.append(tr);
  }
  box.append(t);
  return box;
}

async function timingView() {
  const box = h("div");
  box.append(h("div", "set-section", "Timelines · last 30 days"));
  let r;
  try { r = await api("/api/timing"); } catch (err) { box.append(h("div", "sub", err.message)); return box; }
  const rows = [
    ...(r.requests.by_kind || []).map((k) => ({ ...k, what: "Whole requests" })),
    ...(r.thread_turns.by_kind || []).map((k) => ({ ...k, what: "Thread tasks" })),
  ];
  if (!rows.length) {
    box.append(h("div", "sub", "Nothing timed yet. The orchestrator estimates how long each request and thread task will take, the app times the real work, and it uses the results to give more realistic timelines."));
    return box;
  }
  box.append(h("div", "sub", "Real wall-clock time by kind of work, and how the orchestrator's estimates compared. It sees these numbers every turn and adjusts its estimates."));
  const t = h("table", "grid");
  t.innerHTML = "<tr><th></th><th>Kind</th><th class='num'>Done</th><th class='num'>Typical</th><th class='num'>80% within</th><th class='num'>Estimated</th><th class='num'>Actual ÷ estimate</th><th>Estimates</th></tr>";
  for (const g of rows) {
    const tr = h("tr");
    const ratio = g.actual_over_estimate_median;
    const acc = g.estimated ? [g.within_range && `✓ ${g.within_range} in range`, g.took_longer && `↑ ${g.took_longer} longer`, g.took_shorter && `↓ ${g.took_shorter} shorter`].filter(Boolean).join("  ") : "—";
    tr.innerHTML = `<td>${esc(g.what)}</td><td class="mono">${esc(g.kind)}</td><td class="num">${g.count}</td>
      <td class="num">${g.median_minutes} min</td><td class="num">${g.p80_minutes} min</td><td class="num">${g.estimated}</td>
      <td class="num">${ratio == null ? "—" : ratio.toFixed(2) + "×"}</td><td class="mono">${esc(acc)}</td>`;
    t.append(tr);
  }
  box.append(t);
  return box;
}

// ================================================================ settings page

async function renderSettings() {
  const pane = $("#pane-settings");
  let s;
  try { s = await api("/api/settings"); } catch (e) { return fail(e); }
  pane.textContent = "";
  const page = h("div", "page");
  page.append(h("h1", null, "Settings"), h("div", "sub", s.config_file));

  const section = (t) => page.append(h("div", "set-section", t));
  const setting = (cat, title, desc, control) => {
    const d = h("div", "set");
    const t = h("div", "t");
    t.innerHTML = `<span class="cat">${esc(cat)}: </span>${esc(title)}`;
    const ds = h("div", "d");
    ds.innerHTML = desc;
    d.append(t, ds, control);
    page.append(d);
  };
  const save = (patch, msg) => saveSettings(patch).then(() => { toast(msg || "saved"); loadState(); }).catch(fail);
  const select = (field, opts, value) => {
    const el = h("select");
    for (const [v, l] of opts) { const o = h("option", null, l); o.value = v; el.append(o); }
    el.value = value;
    el.onchange = () => save({ [field]: el.value });
    return el;
  };
  const text = (field, value, placeholder, list) => {
    const el = h("input");
    el.value = value;
    el.placeholder = placeholder || "";
    el.spellcheck = false;
    if (list) el.setAttribute("list", list);
    el.onchange = () => save({ [field]: el.value });
    return el;
  };
  const effortOpts = [["", "default"], ...EFFORTS.map((e) => [e, `${e} — ${EFFORT_DESC[e]}`])];

  section("Workspace");
  {
    const el = h("select");
    for (const [v, l] of [["development", "Development"], ["business", "Business"]]) { const o = h("option", null, l); o.value = v; el.append(o); }
    el.value = S.state.project?.kind || "development";
    el.onchange = () => { if (sendIpc({ op: "set_workspace_kind", kind: el.value })) { toast(`this workspace is now a ${el.value} workspace`); setTimeout(loadState, 400); } else el.value = S.state.project?.kind || "development"; };
    setting("Workspace", "Workspace type", "<b>Business</b> turns on the business features for this workspace: the approval queue (agents propose anything outbound with <code>queue_item</code>, and only you approve it, in the ✓ Queue tab). <b>Development</b> (the default) works as before. Can only be changed in this window.", el);
  }
  {
    const el = h("select");
    for (const [v, l] of [["", "By workspace type (default)"], ["about_me", "Only lessons about me"], ["none", "None"]]) { const o = h("option", null, l); o.value = v; el.append(o); }
    el.value = S.state.project?.lesson_scope || "";
    el.onchange = () => { if (sendIpc({ op: "set_lesson_scope", scope: el.value })) { toast("saved"); setTimeout(loadState, 400); } else el.value = S.state.project?.lesson_scope || ""; };
    setting("Workspace", "Shared lessons", "Which shared lessons this workspace's orchestrator gets. <b>By workspace type</b>: lessons about you, plus the development or business lessons. <b>Only lessons about me</b> and <b>None</b> are for odd workspaces. This project's own lessons always apply. See them all on the 📋 brief page.", el);
  }

  if (S.state.project?.kind === "business") {
    section("Secrets");
    const box = h("div", "secrets");
    for (const sc of S.state.secrets || []) {
      const r = h("div", "row secret-row");
      r.append(h("span", "mono", sc.name), h("span", "qchip", sc.scope === "all" ? "all sessions (env var)" : "approved actions only"));
      const del = h("button", "btn danger", "Remove");
      del.onclick = () => { if (confirm(`Remove secret ${sc.name}?`) && sendIpc({ op: "vault_remove", name: sc.name })) setTimeout(() => loadState().then(renderSettings), 500); };
      r.append(del);
      box.append(r);
    }
    if (!(S.state.secrets || []).length) box.append(h("div", "sub", "No secrets yet."));
    const add = h("div", "row");
    const name = h("input"); name.placeholder = "NAME (e.g. PINTEREST_TOKEN)"; name.spellcheck = false;
    const val = h("input"); val.type = "password"; val.placeholder = "value"; val.autocomplete = "off";
    const scope = h("select");
    for (const [v, l] of [["approved", "approved actions only"], ["all", "all sessions (env var)"]]) { const o = h("option", null, l); o.value = v; scope.append(o); }
    const b = h("button", "btn primary", "Save secret");
    b.onclick = () => {
      const n = name.value.trim().toUpperCase();
      if (!n || !val.value) return toast("name and value are required", true);
      if (sendIpc({ op: "vault_set", name: n, value: val.value, scope: scope.value })) { val.value = ""; name.value = ""; setTimeout(() => loadState().then(renderSettings), 600); }
    };
    add.append(name, val, scope, b);
    box.append(add);
    {
      const row = h("div", "row");
      const day = h("input"); day.type = "number"; day.min = "0"; day.step = "0.5"; day.value = S.state.project.budget_day || ""; day.placeholder = "per day ($)";
      const week = h("input"); week.type = "number"; week.min = "0"; week.step = "1"; week.value = S.state.project.budget_week || ""; week.placeholder = "per week ($)";
      const b = h("button", "btn primary", "Save caps");
      b.onclick = () => { if (sendIpc({ op: "set_budget", day: +day.value || 0, week: +week.value || 0 })) { toast("budget caps saved"); setTimeout(loadState, 500); } };
      row.append(day, week, b);
      const sp = S.state.spend || { day: 0, week: 0 };
      setting("Budget", "Budget caps", `Daily and weekly caps on this workspace's cost at API rates (plan and API turns both count; blank or 0 = no cap). Spent so far: <b>$${sp.day.toFixed(2)}</b> today, <b>$${sp.week.toFixed(2)}</b> this week. At 80% you're notified; at 100% new threads only start for paid orders (<code>priority: "paid-order"</code>), and the orchestrator is told why. Can only be changed in this window.`, row);
    }
    {
      const box = h("div", "secrets");
      for (const sc of S.state.schedules || []) {
        const r = h("div", "row secret-row");
        r.append(h("span", "mono", `${sc.time} · ${sc.days_text}`), h("span", null, (sc.thread ? `🔁 ${sc.thread}: ` : "") + sc.message));
        if (!sc.confirmed) {
          const cb = h("button", "btn primary", "Confirm");
          cb.title = `Proposed by the ${sc.created_by}${sc.reason ? ": " + sc.reason : ""}`;
          cb.onclick = () => { if (sendIpc({ op: "schedule", action: "confirm", id: sc.id })) setTimeout(() => loadState().then(renderSettings), 500); };
          r.append(h("span", "qchip you", "proposed: needs your OK"), cb);
        } else {
          const on = h("button", "btn", sc.enabled ? "Disable" : "Enable");
          on.onclick = () => { if (sendIpc({ op: "schedule", action: "save", id: sc.id, time: sc.time, days: sc.days_text, message: sc.message, thread: sc.thread || "", enabled: !sc.enabled })) setTimeout(() => loadState().then(renderSettings), 500); };
          r.append(h("span", "qchip" + (sc.enabled ? "" : " st-void"), sc.enabled ? "on" : "off"), on);
        }
        if (sc.last_result) r.title = `last: ${sc.last_result} (${new Date(sc.last_at * 1000).toLocaleString()})`;
        const del = h("button", "btn danger", "Delete");
        del.onclick = () => { if (sendIpc({ op: "schedule", action: "delete", id: sc.id })) setTimeout(() => loadState().then(renderSettings), 500); };
        r.append(del);
        box.append(r);
      }
      if (!(S.state.schedules || []).length) box.append(h("div", "sub", "No schedules yet."));
      const add = h("div", "row");
      const time = h("input"); time.type = "time"; time.value = "08:00";
      const days = h("select");
      for (const d of ["daily", "weekdays", "weekends", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]) { const o = h("option", null, d); o.value = d; days.append(o); }
      const target = h("select");
      target.title = "Who gets the message: the orchestrator, or a standing thread directly";
      for (const [v, l] of [["", "→ orchestrator"], ...(S.state.threads || []).map((t) => [t.name, "→ 🔁 " + t.name])]) { const o = h("option", null, l); o.value = v; target.append(o); }
      const msg = h("input"); msg.placeholder = "message, e.g. morning run"; msg.style.flex = "1";
      const b = h("button", "btn primary", "Add");
      b.onclick = () => {
        if (!msg.value.trim()) return toast("a message is required", true);
        if (sendIpc({ op: "schedule", action: "save", id: "", time: time.value, days: days.value, message: msg.value.trim(), thread: target.value, enabled: true })) { msg.value = ""; setTimeout(() => loadState().then(renderSettings), 500); }
      };
      add.append(time, days, target, msg, b);
      box.append(add);
      setting("Schedules", "Scheduled routines", "Send the orchestrator a message at set times (your local time), e.g. 08:00 every day: <i>morning run</i>. The orchestrator can propose schedules; they only run once you confirm them here. A run is skipped, and the skip logged, if this workspace is paused, its budget is spent, or the previous run is still going. Runs need the app open or in the tray (see <b>Start at sign-in</b>). Missed times aren't caught up.", box);
    }
    {
      const pj = S.state.project;
      const g = pj.gates || { five_hour: 0.7, week: 0.6, week_stop: 0.8, share: 0.5 };
      const row = h("div", "row");
      const num = (v, title) => { const e = h("input"); e.type = "number"; e.min = "1"; e.max = "100"; e.value = Math.round(v * 100); e.title = title; e.style.width = "70px"; return e; };
      const f5 = num(g.five_hour, "5-hour window % at which only paid-order threads start");
      const fw = num(g.week, "weekly % at which only paid-order threads start");
      const fs = num(g.week_stop, "weekly % at which running non-paid threads checkpoint and stop");
      const sh = num(g.share, "share of the projected weekly usage left that open orders may take");
      const fb = h("input"); fb.type = "checkbox"; fb.checked = !!pj.paid_api_fallback;
      const capv = h("input"); capv.type = "number"; capv.min = "0"; capv.step = "1"; capv.value = pj.paid_api_cap_usd || ""; capv.placeholder = "$ per order"; capv.style.width = "100px";
      const b = h("button", "btn primary", "Save");
      b.onclick = () => { if (sendIpc({ op: "set_capacity", five_hour: +f5.value, week: +fw.value, week_stop: +fs.value, share: +sh.value, fallback: fb.checked, cap: +capv.value || 0 })) setTimeout(loadState, 500); };
      const lab = (t, e) => { const l = h("label"); l.style.cssText = "display:flex;gap:4px;align-items:center"; l.append(document.createTextNode(t), e); return l; };
      row.append(lab("5-hour %", f5), lab("week %", fw), lab("stop at week %", fs), lab("offers may use %", sh));
      const row2 = h("div", "row");
      row2.append(lab("Paid orders may finish on the API when the plan runs out", fb), lab("cap", capv), b);
      const box = h("div"); box.append(row, row2);
      setting("Usage", "Usage gates (this workspace runs on your plan)", `Keeps paid work deliverable. At the first two percentages only <code>priority: "paid-order"</code> threads start; at the third, running non-paid threads are asked to checkpoint and stop. Offers, accepted orders and new gigs get a usage check: the orders you already took plus the new one must fit in the share of your weekly usage that should be left before its deadline. If not, the card says so in red and needs a second click. The API fallback (needs an API key) only ever applies to paid-order threads, up to the cap per order.`, box);
    }
    {
      const pj = S.state.project;
      const EV = [["urgent", "Urgent queue items"], ["schedule", "Finished scheduled runs"], ["budget", "Budget alerts"], ["deadline", "Orders at risk of missing their deadline"], ["usage", "Usage gates"]];
      const on = new Set(pj.notify_events || []);
      const box = h("div");
      const cbs = EV.map(([id, label]) => {
        const l = h("label"); l.style.cssText = "display:flex;gap:6px;align-items:center";
        const c = h("input"); c.type = "checkbox"; c.checked = on.has(id); c.dataset.ev = id;
        l.append(c, document.createTextNode(label)); box.append(l); return c;
      });
      const row = h("div", "row");
      const phone = h("select");
      for (const [v, l] of [["", "no phone push"], ["ntfy", "ntfy.sh (secret NTFY_TOPIC)"], ["telegram", "Telegram bot (secrets TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID)"]]) { const o = h("option", null, l); o.value = v; phone.append(o); }
      phone.value = pj.notify_phone || "";
      const save = h("button", "btn primary", "Save");
      save.onclick = () => { if (sendIpc({ op: "set_notify", events: cbs.filter((c) => c.checked).map((c) => c.dataset.ev), phone: phone.value })) setTimeout(loadState, 500); };
      const test = h("button", "btn", "Send a test");
      test.onclick = () => sendIpc({ op: "notify_test" });
      row.append(phone, save, test);
      box.append(row);
      // Summaries: non-urgent notifications wait and arrive together at these times.
      const times = Array.isArray(pj.digest_times) ? pj.digest_times : ["09:00", "13:00", "19:00"];
      const drow = h("div", "row");
      drow.append(h("span", "sub", "Send a summary at:"));
      const tin = [0, 1, 2].map((i) => { const t = h("input"); t.type = "time"; t.value = times[i] || ""; return t; });
      const dsave = h("button", "btn primary", "Save times");
      dsave.onclick = () => { if (sendIpc({ op: "set_digests", times: tin.map((t) => t.value).filter(Boolean) })) setTimeout(loadState, 500); };
      const doff = h("button", "btn", "No summaries");
      doff.title = "Send everything straight away";
      doff.onclick = () => { if (sendIpc({ op: "set_digests", times: [] })) { tin.forEach((t) => { t.value = ""; }); setTimeout(loadState, 500); } };
      drow.append(...tin, dsave, doff);
      box.append(drow);
      box.append(h("div", "sub", times.length ? "Urgent things (a late order, an urgent approval, a usage stop) still arrive right away." : "Summaries are off: every notification arrives right away."));
      if (pj.held) {
        const hrow = h("div", "row");
        const now = h("button", "btn", `Send the ${pj.held} held now`);
        now.onclick = () => { if (sendIpc({ op: "digest_now" })) { now.disabled = true; setTimeout(loadState, 500); } };
        hrow.append(now);
        box.append(hrow);
        for (const x of pj.held_items || []) box.append(h("div", "sub", `• ${x.title}${x.body ? " — " + x.body : ""}`));
      }
      setting("Notifications", "Notifications", "Windows notifications for the events you tick, and optionally a push to your phone through <a href='https://ntfy.sh' target='_blank' rel='noopener'>ntfy.sh</a> or your own Telegram bot (add its topic or token under Secrets below, approved-actions-only scope is fine). Only a one-line summary is sent, with secrets redacted, never an item's payload.", box);
    }
    {
      const pj = S.state.project;
      const lab = h("label"); lab.style.cssText = "display:flex;gap:6px;align-items:center";
      const cb = h("input"); cb.type = "checkbox"; cb.checked = !!pj.phone_feed;
      cb.onchange = () => { if (sendIpc({ op: "set_phone_feed", on: cb.checked })) setTimeout(loadState, 500); else cb.checked = !cb.checked; };
      lab.append(cb, document.createTextNode(`Share this workspace's business feed with your paired phone (id ${pj.feed_id || ""})`));
      {
        const ph = S.state.phones || { enrolled: [], pending: null };
        const box = h("div", "secrets");
        for (const p of ph.enrolled || []) {
          const r = h("div", "row secret-row");
          r.append(h("span", "mono", p.fingerprint), h("span", null, `${p.name} (${p.device})`));
          if (p.revoked_at) r.append(h("span", "qchip st-void", "revoked"));
          else {
            const rv = h("button", "btn danger", "Revoke");
            rv.onclick = () => { if (confirm(`Revoke ${p.name} (${p.fingerprint})? It can't sign decisions again.`) && sendIpc({ op: "phone_revoke", device: p.device })) setTimeout(() => loadState().then(renderSettings), 500); };
            r.append(rv);
          }
          box.append(r);
        }
        if (ph.pending) {
          const r = h("div", "qbanner");
          r.textContent = `Check that your phone shows this fingerprint: ${ph.pending.fingerprint}  (${ph.pending.name}, device ${ph.pending.device})`;
          const ok = h("button", "btn primary", "It matches: add phone");
          ok.onclick = () => { if (sendIpc({ op: "phone_enroll_confirm", device: ph.pending.device, fingerprint: ph.pending.fingerprint })) setTimeout(() => loadState().then(renderSettings), 500); };
          const no = h("button", "btn", "Cancel");
          no.onclick = () => { sendIpc({ op: "phone_enroll_cancel" }); setTimeout(() => loadState().then(renderSettings), 400); };
          const row = h("div", "row"); row.append(ok, no);
          box.append(r, row);
        } else {
          const row = h("div", "row");
          const ta = h("input"); ta.placeholder = "paste the phone's pairing bundle (atlas-orch/v1|pair|… or JSON)"; ta.style.flex = "1"; ta.spellcheck = false;
          const b = h("button", "btn", "Check");
          b.onclick = () => { if (ta.value.trim() && sendIpc({ op: "phone_enroll_begin", bundle: ta.value.trim() })) setTimeout(() => loadState().then(renderSettings), 500); };
          row.append(ta, b);
          box.append(row);
        }
        const lab = h("label"); lab.style.cssText = "display:flex;gap:6px;align-items:center;margin-top:6px";
        const cb2 = h("input"); cb2.type = "checkbox"; cb2.checked = !!S.state.project.phone_decide;
        cb2.onchange = () => { if (sendIpc({ op: "set_phone_decide", on: cb2.checked })) setTimeout(loadState, 500); else cb2.checked = !cb2.checked; };
        lab.append(cb2, document.createTextNode("Allow approve / reject from paired phones in this workspace (each decision signed with Face ID)"));
        box.append(lab);
        setting("Phone", "Paired phones", "A phone can only approve or reject when it signs the decision with a key you paired here: Orchestrator checks that it is really your phone, that it is the same item you saw, and that the answer is fresh. Agents can never approve things themselves. Add a phone by pasting the code it shows, then check the fingerprint matches.", box);
      }
      setting("Phone", "Phone access (Atlas Business tab)", "Off by default. When on, the Atlas companion on this PC can read a <b>read-only</b> feed of this workspace over the local API: pending items with their exact payloads, today's deadlines and usage, money totals, schedules, attachment thumbnails, and live events. Nothing can be approved or changed through it.", lab);
    }
    setting("Secrets", "Secrets for this workspace", "Encrypted with your Windows account (DPAPI), kept out of the app's state file, and redacted from every transcript and log. <b>Approved actions only</b> (recommended): a secret is only given to the command of an item you approved, when an agent runs it after your OK. <b>All sessions</b>: set as an environment variable in every thread, so agents can use it without asking. Only for harmless keys. Can only be changed in this window.", box);
  }

  section("Messages between workspaces");
  {
    const x = S.state.xws || { rules: [], pending: 0 };
    const box = h("div");
    for (const ru of x.rules || []) {
      const row = h("div", "row");
      row.append(h("span", null, `${ru.from_name} → ${ru.to_name}`));
      const rv = h("button", "btn danger sm", "Revoke");
      rv.onclick = () => { if (sendIpc({ op: "xws", action: "rule_revoke", id: ru.id })) { rv.disabled = true; setTimeout(() => loadState().then(renderSettings), 500); } };
      row.append(rv);
      box.append(row);
    }
    if (!(x.rules || []).length) box.append(h("div", "sub", "No standing rules. Every message to another workspace asks you first."));
    const wss = (S.state.open_workspaces || []).map((w) => [w.path, baseName(w.path)]);
    if (wss.length > 1) {
      const add = h("div", "row");
      const mk = () => { const el = h("select"); for (const [v, l] of wss) { const o = h("option", null, l); o.value = v; el.append(o); } return el; };
      const from = mk(), to = mk();
      to.selectedIndex = 1;
      const b = h("button", "btn primary", "Add");
      b.onclick = () => {
        if (from.value === to.value) return toast("Pick two different workspaces", true);
        if (!confirm(`Let any session in ${baseName(from.value)} message any session in ${baseName(to.value)} without asking you each time?`)) return;
        if (sendIpc({ op: "xws", action: "rule_add", from: from.value, to: to.value })) setTimeout(() => loadState().then(renderSettings), 500);
      };
      add.append(h("span", "sub", "Allow"), from, h("span", null, "→"), to, b);
      box.append(add);
    }
    setting("Messages", "Messages between workspaces", "An agent in one workspace can ask to message a session in another. By default you approve each one (it shows up in Needs you and in the chat). A rule lets one direction through without asking. It works one way only: allowing dev → biz does not allow biz → dev.", box);
  }

  section("Billing");
  {
    const row = h("div", "row");
    const key = h("input");
    key.type = "password";
    key.placeholder = s.api_key_set ? `saved (${s.api_key_masked}) — paste to replace` : "sk-ant-…";
    const b = h("button", "btn", "Save key");
    b.onclick = () => { if (key.value.trim()) save({ api_key: key.value.trim() }, "API key saved").then(renderSettings); };
    row.append(key, b);
    if (s.api_key_set) {
      const c = h("button", "btn danger", "Remove");
      c.onclick = () => save({ clear_api_key: true }, "API key removed").then(renderSettings);
      row.append(c);
    }
    setting("Billing", "Anthropic API key", "Used for the API share of the plan/API mix. Stored in your user profile.", row);
  }
  {
    const wrap = h("div", "mixset");
    wrap.append(mixControl(S.state.usage.mix, (v) => setMix(v)));
    setting("Billing", "Plan/API mix for this project", `How much of this project's work runs on API credits: 0 = all on your Claude plan, 10 = all API. Every thread and the orchestrator follow it; the app routes each turn to keep close to it. In between, a plan turn that hits a usage limit also finishes on the API.`, wrap);
  }
  {
    const wrap = h("div", "mixset");
    wrap.append(mixControl(s.api_mix, (v) => api("/api/settings", { body: { api_mix: v } }).then(() => toast(`new projects: ${mixLabel(v)}`))));
    setting("Billing", "Mix for new projects", "Starting value for projects you haven't set a mix on.", wrap);
  }
  setting("Billing", "Plan usage", "Your Claude plan's 5-hour and weekly limits, as Claude Code reports them. Updated after every plan turn.", planUsageView(() => renderSettings()));
  setting("Billing", "API workspace ID", "Only for keys that aren't scoped to a workspace (the API says it <em>must include the anthropic-workspace-id header</em>). Must be a workspace in the <b>same organization</b> as the key. Leave blank for workspace keys.",
    text("api_workspace_id", s.api_workspace_id, "wrkspc_…"));
  {
    const wrap = h("div");
    const b = h("button", "btn", "Run check");
    b.onclick = runAuth;
    const out = h("div", "auth");
    out.id = "auth-out";
    wrap.append(b, out);
    setting("Billing", "Check logins", "Runs a one-word Haiku turn on each backend and shows which credential Claude Code actually used.", wrap);
  }

  {
    const b = h("button", "btn", "Run setup again");
    b.onclick = openSetup;
    setting("Billing", "Setup", "Sign in to a Claude plan and/or add an API key, step by step.", b);
  }
  section("Permissions");
  {
    const el = h("select");
    for (const [v, l, d] of PERMS) { const o = h("option", null, `${l}: ${d}`); o.value = v; el.append(o); }
    el.value = S.state.permission;
    el.onchange = () => ipcCall("permission", { level: el.value }).then(() => { toast("saved"); loadState(); }).catch(fail);
    setting("Permissions", "This project", "How much threads may do without asking you. Approvals appear as cards in the chat. Each thread can override it (Threads tab). Your Claude Code allow and deny rules still apply.", el);
  }
  setting("Permissions", "Default for new projects", "", select("permission_default", PERMS.map(([v, l]) => [v, l]), s.permission_default || "bypassPermissions"));

  section("Orchestrator");
  setting("Orchestrator", "Model", "Blank uses Claude Code's default.", text("orch_model", s.orch_model, "default", "models"));
  setting("Orchestrator", "Effort", "Reasoning effort for the orchestrator.", select("orch_effort", effortOpts, s.orch_effort));

  section("New threads");
  setting("Threads", "Default model", "Blank uses Claude Code's default.", text("thread_model", s.thread_model, "default", "models"));
  setting("Threads", "Default effort", "Used when the orchestrator doesn't pick one. Blank means medium.", select("thread_effort", [["", "medium (default)"], ...EFFORTS.map((e) => [e, `${e} — ${EFFORT_DESC[e]}`])], s.thread_effort));
  setting("Threads", "Effort cap", "Above this, the orchestrator has to ask you first (you approve or it uses this level). You can always set a higher one on a thread yourself.", select("max_effort", EFFORTS.map((e) => [e, e]), s.max_effort || "high"));

  section("Claude Code");
  setting("Claude Code", "CLI path", `Blank finds <code>claude.exe</code> automatically. Currently: <code>${esc(s.cli_resolved)}</code>`, text("cli_path", s.cli_path, "auto"));

  section("Appearance");
  setting("Appearance", "Theme", "", select("theme", [["dark", "Dark"], ["light", "Light"], ["system", "Follow Windows"]], s.theme));

  section("About");
  {
    const app = S.state.app || {};
    const wrap = h("div");
    const line = h("div", "d");
    line.innerHTML = `Orchestrator <b>${esc(app.version || "")}</b> · MIT License · <a href="https://github.com/${esc(app.repo || "")}">github.com/${esc(app.repo || "")}</a>`;
    const row = h("div", "row");
    const chk = h("button", "btn", "Check for updates");
    chk.onclick = async () => {
      chk.disabled = true;
      try {
        const r = await api("/api/check-update", { body: {} });
        toast(r.update ? `Version ${r.update.version} is available: click ⬆ update in the status bar` : `You're on the latest version (${r.version})`);
        await loadState();
      } catch (e) { fail(e); }
      chk.disabled = false;
    };
    const auto = h("label");
    auto.style.cssText = "display:flex;gap:6px;align-items:center;margin-left:10px";
    const cb = h("input");
    cb.type = "checkbox";
    cb.checked = s.check_updates !== false;
    cb.onchange = () => save({ check_updates: cb.checked }, cb.checked ? "will check for updates" : "update checks off");
    auto.append(cb, document.createTextNode("Check automatically"));
    row.append(chk, auto);
    wrap.append(line, row);
    setting("About", "Version", "Update checks ask GitHub for the latest release; nothing else is sent.", wrap);
  }
  {
    const lab = h("label");
    lab.style.cssText = "display:flex;gap:6px;align-items:center";
    const cb = h("input");
    cb.type = "checkbox";
    cb.checked = !!s.start_at_login;
    cb.onchange = () => save({ start_at_login: cb.checked }, cb.checked ? "starts at sign-in, in the tray" : "won't start at sign-in");
    lab.append(cb, document.createTextNode("Start Orchestrator when I sign in to Windows (minimized to the tray)"));
    setting("About", "Start at sign-in", "Keeps scheduled routines and running work going without opening the window first.", lab);
  }
  const about = h("div", "d");
  about.innerHTML = `Data: <code>${esc(s.data_dir)}</code><br>Threads run Claude Code with permissions bypassed. Pick workspaces you're OK with them changing.`;
  page.append(about);
  pane.append(page);
}

async function runAuth() {
  const out = $("#auth-out");
  if (!out) return;
  out.textContent = "running…";
  try {
    const r = await api("/api/auth-check", { body: {} });
    out.textContent = "";
    for (const x of r.results) {
      const row = h("div");
      const keyish = x.backend === "plan" && /key/i.test(x.source);
      row.innerHTML = `${esc(x.backend.padEnd(5))} <span class="${x.ok ? (keyish ? "warn" : "ok") : "bad"}">${x.ok ? "✓" : "✕"} ${esc(x.ok ? x.source : x.detail)}</span>`;
      if (keyish) row.innerHTML += `<div class="warn">plan is using an API key, not your subscription: run <code>claude auth login</code></div>`;
      out.append(row);
    }
    if (!r.api_key_set) out.append(h("div", "warn", "api   – no API key saved"));
  } catch (e) { out.textContent = e.message; }
}

// ================================================================ wiring

function toggleSide() { $("#side").classList.toggle("collapsed"); renderChrome(); }

function wire() {
  wireQi();
  const input = $("#input");
  input.addEventListener("input", () => { autosize(); S.suggest.i = 0; renderSuggest(); });
  input.addEventListener("keydown", (e) => {
    const open = !$("#suggest").classList.contains("hidden") && S.suggest.items.length;
    if (open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      const n = S.suggest.items.length;
      S.suggest.i = (S.suggest.i + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
      return renderSuggest();
    }
    if (open && e.key === "Tab") { e.preventDefault(); return acceptSuggest(); }
    if (open && e.key === "Enter" && !e.shiftKey) {
      const it = S.suggest.items[S.suggest.i];
      if (it && input.value.trim() !== it.insert.trim()) { e.preventDefault(); return acceptSuggest(); }
    }
    if (open && e.key === "Escape") { e.preventDefault(); return closeSuggest(); }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); return submit(e.altKey); }
    if (e.key === "ArrowUp" && !input.value && !open) {
      const q = info(curKey())?.queue || [];
      if (q.length) { // pull the newest queued message back for editing
        e.preventDefault();
        const m = q[q.length - 1];
        queueAction("remove", m.id).then(() => { input.value = m.text; autosize(); });
        return;
      }
    }
    if (e.key === "Escape" && curKey() && info(curKey())?.busy) { e.preventDefault(); stopTurn().catch(fail); }
  });
  input.addEventListener("blur", () => setTimeout(closeSuggest, 120));
  $("#stop").onclick = () => stopTurn().catch(fail);
  $("#attach").onclick = () => $("#file-input").click();
  $("#file-input").onchange = (e) => { addFiles([...e.target.files]); e.target.value = ""; };
  input.addEventListener("paste", (e) => {
    const files = [...(e.clipboardData?.files || [])];
    if (files.length) { e.preventDefault(); addFiles(files); }
  });
  let dragDepth = 0;
  document.addEventListener("dragenter", (e) => { if (e.dataTransfer?.types?.includes("Files")) { dragDepth++; $("#dropzone").classList.remove("hidden"); } });
  document.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; $("#dropzone").classList.add("hidden"); } });
  document.addEventListener("dragover", (e) => e.preventDefault());
  document.addEventListener("drop", (e) => {
    e.preventDefault();
    dragDepth = 0;
    $("#dropzone").classList.add("hidden");
    const files = [...(e.dataTransfer?.files || [])];
    if (files.length && curKey()) addFiles(files);
  });
  $("#lightbox").onclick = () => $("#lightbox").classList.add("hidden");
  document.addEventListener("click", (e) => {
    if (onThreadLink(e) || onFileLink(e)) return;
    const a = e.target.closest("a[href]");
    if (a && /^https?:/.test(a.getAttribute("href"))) { e.preventDefault(); openUrl(a.href); }
  }, true);
  wireSplit();
  $("#side-new").onclick = () => newThread("");
  initChatBar();
  $("#side-collapse").onclick = toggleSide;
  for (const a of $$(".act")) a.onclick = () => {
    const act = a.dataset.act;
    if (act === "orch") openChat(orchKey());
    if (act === "explorer") toggleSide();
    if (act === "manage") openManage();
    if (act === "queue") openQueue();
    if (act === "needs") openNeeds();
    if (act === "money") openMoney();
    if (act === "settings") openSettings();
    if (act === "palette") palette();
  };
  $("#nt-create").onclick = createFromDialog;
  $("#nt-browse").onclick = async () => { const p = await pickFolder($("#nt-cwd").value || S.state.workspace, "Thread folder"); if (p) $("#nt-cwd").value = p; };
  $("#nt-name").addEventListener("keydown", (e) => { if (e.key === "Enter") createFromDialog(); });
  for (const b of $$("[data-close]")) b.onclick = closeDialog;
  $("#dlg-bg").addEventListener("mousedown", (e) => { if (e.target.id === "dlg-bg") closeDialog(); });

  document.addEventListener("keydown", (e) => {
    const ctrl = e.ctrlKey || e.metaKey;
    const k = e.key.toLowerCase();
    if (e.key === "Escape" && !$("#dlg-bg").classList.contains("hidden")) return closeDialog();
    if (e.key === "Escape" && !$("#lightbox").classList.contains("hidden")) return $("#lightbox").classList.add("hidden");
    if (SW.active && cur()?.type === SW.pane) {
      if (e.key === "Escape") { e.preventDefault(); return swStop(); }
      if (e.key === "Enter" && !ctrl && !/^(INPUT|TEXTAREA|BUTTON|SELECT)$/.test(document.activeElement?.tagName || "")) { e.preventDefault(); return swEnter(); }
    }
    // 1-9 picks an option on a single open question when the composer is empty
    if (/^[1-9]$/.test(e.key) && !ctrl && document.activeElement?.id === "input" && !$("#input").value) {
      const open = $$(".ev.question:not(.done)");
      if (open.length === 1 && $$(".q", open[0]).length === 1) {
        const opt = $$(".q-opt", open[0])[+e.key - 1];
        if (opt) { e.preventDefault(); opt.click(); return; }
      }
    }
    if ((ctrl && e.shiftKey && k === "p") || e.key === "F1") { e.preventDefault(); return palette(); }
    if (ctrl && !e.shiftKey && k === "p") { e.preventDefault(); return goToThread(); }
    if (SOLO && ctrl && ["p", "n", "w", "b", ",", "1"].includes(k)) return;
    if (ctrl && k === "n") { e.preventDefault(); return newThread(""); }
    if (ctrl && k === "w") { e.preventDefault(); return closeTab(S.active); }
    if (ctrl && k === "b") { e.preventDefault(); return toggleSide(); }
    if (ctrl && k === ",") { e.preventDefault(); return openSettings(); }
    if (ctrl && k === "1") { e.preventDefault(); return openChat(orchKey()); }
    if (ctrl && k === "l") { e.preventDefault(); return $("#input").focus(); }
    if (ctrl && e.key === "Tab") { e.preventDefault(); return activate((S.active + (e.shiftKey ? S.tabs.length - 1 : 1)) % S.tabs.length); }
  });
  // Hidden or minimized windows get throttled; catch up when shown again.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && S.state) { loadState(); if (curKey()) loadLog(curKey()); }
  });
  matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => S.state && renderChrome());
}

(async function main() {
  wire();
  await loadState();
  if (S.state && !SOLO && !S.state.defaults.setup_done && !S.state.threads.length && !S.state.project) openSetup();
  if (S.state) await activate(0);
  connect();
})();
