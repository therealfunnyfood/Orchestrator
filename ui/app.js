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
const SOLO = !!VIEW;
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
const APP_TOOLS = new Set(["mcp__app__update_plan", "mcp__app__ask_user", "mcp__app__show_media"]);
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
       .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(codes[+i])}</code>`);
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
}

// ================================================================ sidebar tree

function stClass(t) { return t.busy ? (t.status === "queued" ? "queued" : "running") : t.status; }

const lastSig = {};
function changed(part, data) {
  const sig = JSON.stringify(data);
  if (lastSig[part] === sig) return false;
  lastSig[part] = sig;
  return true;
}

function renderTree() {
  if (!changed("tree", [S.state.workspace, S.state.orchestrator, S.state.threads.map((t) => [t.name, stClass(t), t.model, t.effort, t.updated_at, t.needs_input, t.plan]), curKey(), [...S.unread]])) return;
  $("#ws-name").textContent = baseName(S.state.workspace);
  $("#ws-row").title = S.state.workspace + " — click to switch";
  const tree = $("#tree");
  tree.textContent = "";
  const o = S.state.orchestrator;
  tree.append(node({ key: o.name, name: "orchestrator", cls: "orch", status: stClass(o), meta: effortLabel(o.effort), row: o }));
  const rows = [...S.state.threads].sort((a, b) => (b.busy - a.busy) || (b.updated_at - a.updated_at));
  tree.append(h("div", "group", `threads · ${rows.length}`));
  if (!rows.length) tree.append(h("div", "empty-tree", "none yet — ask the orchestrator, or + to add one"));
  for (const t of rows) tree.append(node({ key: t.name, name: t.name, status: stClass(t), meta: [modelLabel(t.model) === "default" ? "" : modelLabel(t.model).replace(/-\d.*/, ""), t.effort].filter(Boolean).join("·"), row: t }));
}

function planProgress(plan) {
  if (!plan?.length) return "";
  return `${plan.filter((p) => p.status === "completed").length}/${plan.length}`;
}

function node({ key, name, cls, status, meta, row }) {
  const n = h("div", "node" + (cls ? " " + cls : ""));
  n.classList.toggle("on", curKey() === key);
  n.classList.toggle("unread", S.unread.has(key));
  n.append(h("span", "st " + status), h("span", "nm", name));
  if (row?.needs_input) { const q = h("span", "q-badge", "?"); q.title = "Waiting for your answer"; n.append(q); }
  const prog = row?.busy && planProgress(row.plan);
  if (prog) n.append(h("span", "prog", prog));
  n.append(h("span", "meta", meta));
  n.title = `${name} · ${status}`;
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
  if (isOrch(t.key)) return ["◈", "orchestrator"];
  return ["›", t.key];
}

function renderTabs() {
  if (!changed("tabs", [S.tabs, S.active, S.tabs.map((t) => t.type === "chat" && !!info(t.key)?.busy)])) return;
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
  let i = S.tabs.findIndex((t) => t.type === tab.type && (t.type !== "chat" || t.key === tab.key));
  if (i === -1) { S.tabs.push(tab); i = S.tabs.length - 1; }
  activate(i);
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
  if (t.type === "chat") { S.unread.delete(t.key); await loadLog(t.key); }
  if (t.type === "manage") renderManage();
  if (t.type === "settings") renderSettings();
  if (t.type === "brief") renderBrief();
  renderChrome();
  if (t.type === "chat") $("#input").focus();
}

const openChat = (key) => openTab({ type: "chat", key });
const openManage = () => { lastSig.manage = null; openTab({ type: "manage" }); };
const openSettings = () => openTab({ type: "settings" });
const openBrief = () => openTab({ type: "brief" });

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
  c.append(h("span", "path", `${inf.turns || 0} turns · ${money(inf.cost_usd)}`));
  if (inf.isOrch && S.state.project?.brief) {
    const br = h("button", null, "📋 brief");
    br.title = "Project brief and rules";
    br.onclick = openBrief;
    c.append(br);
  }
  if (inf.isOrch) {
    const b = h("button", null, "new conversation");
    b.onclick = () => runCommand("/reset").catch(fail);
    c.append(b);
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
  if (k === "user") {
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
    if (ev.sender === "threads") { e.classList.add("from-orch"); e.append(h("span", "who", "from threads →")); }
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
        api("/api/send", { body: { thread: ev.thread, text: "Continue where you left off. The app closed in the middle of your last turn, so check what was already done before redoing anything." } }).catch(fail);
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
          await api("/api/settings", { body });
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
        await api("/api/setup/finish", { body: { mode: SETUP.mode, mix: SETUP.mix, permission: SETUP.perm } }).catch(fail);
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
  const section = (title, field, desc) => {
    page.append(h("div", "set-section", title));
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
      else await api("/api/notes", { body: { [field]: ta.value } }).catch(fail);
      await loadState();
      renderBrief();
      toast("saved");
    };
    row.append(edit, saveB);
    page.append(view, ta, row);
  };
  section("Rules", "rules", "Added to every thread's instructions in this workspace.");
  section("Tuning notes", "tuning", "The orchestrator's own lessons about which model and effort fit which work in this project. It updates these from the efficiency stats; you can edit them too.");
  section("Brief", "brief", "Shown to the orchestrator in every conversation in this workspace.");
  pane.append(page);
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
    await api("/api/edit", { body: { thread: curKey(), ts, text } });
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

function openPath(path, how) {
  api("/api/open", { body: { path, how } }).catch(fail);
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
  try { await api("/api/answer", { body: { id: card.dataset.id, answers } }); }
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
  w.append(document.createTextNode(label + " "), tm, h("span", "hint", " (esc to stop)"));
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
    item("", `<span class="mono">${money(inf.cost_usd)}</span>`, null, "API-rate cost so far (plan turns aren't charged)");
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
  if (curKey()) setTimeout(() => $("#input").focus(), 0);
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
    await api("/api/permission", { body: { thread: thread ? thread.name : "", level: it.value } });
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
  const it = await quickPick(items, "Switch workspace (each has its own orchestrator and threads) — or paste a path", { allowText: true });
  if (!it) return;
  let path = it.text ?? it.value, create = false;
  if (path === "__import") return importProject();
  if (path === "__browse") path = await pickFolder(S.state.workspace, "Open workspace folder");
  else if (path === "__new") {
    const t = await quickPick([], "Full path of the new folder", { allowText: true, value: S.state.workspace + "\\" });
    path = t?.text; create = true;
  }
  if (!path) return;
  try {
    await api("/api/workspace", { body: { path, create } });
    S.tabs = [];
    await loadState();
    await activate(0);
    toast("workspace: " + baseName(S.state.workspace));
  } catch (e) { fail(e); }
}

async function threadMenu(name) {
  const items = [
    { label: "Open", value: "open" }, { label: "Open to the side", desc: "Alt+click", value: "side" },
    { label: "Open in new window", desc: "Shift+click", value: "pop" }, { label: "Stop", value: "stop" },
    { label: "Set model…", value: "model" }, { label: "Set permission level…", value: "perm" }, { label: "Set effort…", value: "effort" },
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
  { cmd: "/stop", args: "[thread]", desc: "Stop the running turn (Esc)", cat: "Thread", complete: "thread", run: (a) => stopTurn(a) },
  { cmd: "/effort", args: "<level>", desc: "Reasoning effort for this conversation", cat: "Config", complete: "effort", run: (a) => a ? setField("effort", a === "default" ? "" : a) : pickEffort() },
  { cmd: "/model", args: "<model>", desc: "Model for this conversation", cat: "Config", complete: "model", run: (a) => a ? setField("model", a === "default" ? "" : a) : pickModel() },
  { cmd: "/permissions", args: "[level]", desc: "Permission level (project, or this thread)", cat: "Config", complete: "perm", run: (a) => {
    if (!a) return pickPermission(curKey());
    const p = PERMS.find((x) => x[0].toLowerCase() === a.toLowerCase() || x[1].toLowerCase().startsWith(a.toLowerCase()));
    if (!p) throw new Error("levels: " + PERMS.map((x) => x[0]).join(", "));
    const k = curKey();
    return api("/api/permission", { body: { thread: k && !isOrch(k) ? k : "", level: p[0] } }).then(loadState).then(() => toast(`permission: ${p[1]}`));
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
  await api("/api/send", { body: { thread: target, text: m[2] } });
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
  await api("/api/workspace", { body: { path, create: false } });
  S.tabs = [];
  await loadState();
  await activate(0);
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
      const r = await api("/api/send", { body: { thread: curKey(), text, attachments, queue } });
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
    await api("/api/threads", { body: { name: m[1], task: m[2] } });
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
    await api("/api/threads", { body });
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
  stat(money(shown.reduce((a, r) => a + r.cost_usd, 0)), "API-rate cost");
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
        ? api("/api/permission", { body: { thread: r.name, level: s.value } }).then(loadState)
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
  box.append(h("div", "sub", `${e.turns} thread turns · ${money(e.total_cost)} at API rates. Verdicts are the orchestrator's: right fit / needed stronger / could be cheaper.`));
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
  const save = (patch, msg) => api("/api/settings", { body: patch }).then(() => { toast(msg || "saved"); loadState(); }).catch(fail);
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
    el.onchange = () => api("/api/permission", { body: { level: el.value } }).then(() => { toast("saved"); loadState(); }).catch(fail);
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
    if (onThreadLink(e)) return;
    const a = e.target.closest("a[href]");
    if (a && /^https?:/.test(a.getAttribute("href"))) { e.preventDefault(); openUrl(a.href); }
  }, true);
  wireSplit();
  $("#side-new").onclick = () => newThread("");
  $("#side-collapse").onclick = toggleSide;
  $("#ws-row").onclick = pickWorkspace;
  for (const a of $$(".act")) a.onclick = () => {
    const act = a.dataset.act;
    if (act === "orch") openChat(orchKey());
    if (act === "explorer") toggleSide();
    if (act === "manage") openManage();
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
