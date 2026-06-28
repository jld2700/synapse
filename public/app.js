import { renderMarkdown } from "./md.js";

// ---------- State ----------
const state = {
  paired: false,
  token: null,
  ws: null,
  sessions: [],          // [{id,name,cwd,model,state,...}]
  activeId: null,
  // per-session transcript view model: events grouped into renderable items
  transcripts: {},       // id -> { items: [...], buffer: "" , streaming:false, openTools:Set }
  drawerOpen: false,
  newSheetOpen: false,
};

const el = (sel, root = document) => root.querySelector(sel);
const elAll = (sel, root = document) => [...root.querySelectorAll(sel)];
const h = (tag, props = {}, ...children) => {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") node.className = v;
    else if (k === "html") node.innerHTML = v;
    else if (k.startsWith("on") && typeof v === "function") node.addEventListener(k.slice(2).toLowerCase(), v);
    else if (v !== null && v !== undefined && v !== false) node.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    node.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return node;
};

// ---------- Pairing ----------
function getStored() {
  try { return JSON.parse(localStorage.getItem("synapse") || "{}"); } catch { return {}; }
}
function setStored(v) { localStorage.setItem("synapse", JSON.stringify(v)); }

function getTokenFromUrl() {
  const p = new URL(location.href).searchParams.get("token");
  if (p) { history.replaceState({}, "", location.pathname); }
  return p;
}

function renderPair(err = "") {
  const app = el("#app");
  app.innerHTML = "";
  const saved = getStored();
  const logo = `<svg viewBox="0 0 64 64"><g fill="none" stroke="#10a37f" stroke-width="3" stroke-linecap="round"><path d="M20 18c-6 6-6 22 0 28"/><path d="M44 18c6 6 6 22 0 28"/></g><circle cx="32" cy="32" r="5" fill="#10a37f"/></svg>`;
  app.append(h("div", { class: "pair" },
    h("div", { class: "logo", html: logo }),
    h("h1", {}, "Synapse"),
    h("p", {}, "Enter the pairing code shown in your terminal to connect to Claude Code."),
    h("input", { id: "pairInput", placeholder: "CODE", maxlength: "12", value: saved.token || getTokenFromUrl() || "", autocomplete: "off", autocapitalize: "characters" }),
    h("div", { class: "err", id: "pairErr" }, err),
    h("button", { onclick: tryPair }, "Connect"),
    h("div", { class: "hint" }, "Remote control for the Claude Code CLI"),
  ));
  const inp = el("#pairInput");
  inp.focus();
  inp.addEventListener("keydown", (e) => { if (e.key === "Enter") tryPair(); });
}

async function tryPair() {
  const code = (el("#pairInput").value || "").trim().toUpperCase();
  if (!code) return;
  // validate against server
  try {
    const r = await fetch("/api/pair?token=" + encodeURIComponent(code));
    if (!r.ok) throw new Error("bad code");
    state.token = code;
    setStored({ token: code });
    connect();
  } catch {
    el("#pairErr").textContent = "Could not connect. Check the code and that the server is reachable.";
  }
}

// ---------- WebSocket ----------
function connect() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const url = `${proto}//${location.host}/?token=${encodeURIComponent(state.token)}`;
  const ws = new WebSocket(url);
  state.ws = ws;
  ws.onopen = () => { state.paired = true; render(); };
  ws.onmessage = (e) => {
    let msg; try { msg = JSON.parse(e.data); } catch { return; }
    handleMessage(msg);
  };
  ws.onclose = () => {
    if (!state.paired) return;
    state.paired = false;
    setTimeout(() => { if (state.token) connect(); }, 1500);
    render();
  };
  ws.onerror = () => {};
}

function send(obj) { if (state.ws && state.ws.readyState === 1) state.ws.send(JSON.stringify(obj)); }

// ---------- Event handling ----------
function handleMessage(msg) {
  switch (msg.type) {
    case "hello":
      state.sessions = msg.sessions || [];
      // request history for any active session
      if (state.activeId) send({ op: "history", sessionId: state.activeId });
      break;
    case "sessions":
      state.sessions = msg.sessions || [];
      break;
    case "created": {
      state.sessions.unshift(msg.session);
      state.activeId = msg.session.id;
      break;
    }
    case "history":
      ingestHistory(msg.sessionId, msg.events);
      break;
    case "event":
      ingestEvent(msg.event);
      break;
    case "error":
      // surface transient errors only if relevant to active session
      break;
  }
  // keep session meta in sync from events
  syncSessionMeta();
  render();
}

function ensureT(sessionId) {
  if (!state.transcripts[sessionId]) {
    state.transcripts[sessionId] = { items: [], buffer: "", streaming: false, streamingRole: null, openTools: new Set() };
  }
  return state.transcripts[sessionId];
}

function ingestHistory(sessionId, events) {
  const t = ensureT(sessionId);
  t.items = [];
  for (const evt of events) ingestEvent(evt, true);
}

// Core: turn stream-json events into a flat list of renderable "items".
function ingestEvent(evt, silent = false) {
  const sid = evt.sessionId || state.activeId;
  if (!sid) return;
  const t = ensureT(sid);

  switch (evt.type) {
    case "system": {
      if (evt.subtype === "session_exit" || evt.subtype === "bridge_error" || evt.subtype === "session_closed") {
        t.streaming = false; t.buffer = "";
        if (evt.error) pushItem(t, { kind: "system", text: evt.error, error: true });
      }
      break;
    }
    case "stderr": {
      pushItem(t, { kind: "system", text: evt.text });
      break;
    }
    case "progress": {
      // update or append a transient progress line
      const data = evt.data || {};
      const label = data.label || data.toolName || data.message || "working";
      const last = t.items[t.items.length - 1];
      if (last && last.kind === "progress") last.text = label;
      else pushItem(t, { kind: "progress", text: label, toolUseID: evt.toolUseID || data.toolUseID });
      break;
    }
    case "assistant": {
      const content = evt.message?.content;
      if (Array.isArray(content)) {
        for (const block of content) {
          if (block.type === "text") {
            appendAssistantText(t, block.text, evt.isMeta);
          } else if (block.type === "tool_use") {
            // finalize any streaming text
            flushAssistant(t);
            upsertTool(t, {
              id: block.id,
              name: block.name,
              input: block.input,
              status: "running",
            });
          }
        }
      } else if (typeof content === "string") {
        appendAssistantText(t, content, evt.isMeta);
      }
      // partial streaming text accumulation
      if (evt.message?.content?.length && evt.message.content[0]?.type === "text" && evt.isPartial) {
        t.streaming = true; t.streamingRole = "assistant";
      }
      break;
    }
    case "user": {
      // tool_result comes back as a user message
      const content = evt.message?.content;
      if (Array.isArray(content)) {
        for (const block of content) {
          if (block.type === "tool_result") {
            upsertTool(t, {
              id: block.tool_use_id,
              status: block.is_error ? "error" : "done",
              result: typeof block.content === "string" ? block.content : JSON.stringify(block.content),
            });
          } else if (block.type === "text") {
            pushItem(t, { kind: "user", text: block.text });
          }
        }
      } else if (typeof content === "string") {
        pushItem(t, { kind: "user", text: content });
      }
      break;
    }
    case "result": {
      flushAssistant(t);
      t.streaming = false;
      // result carries the final assembled text sometimes
      if (evt.result && typeof evt.result === "string" && !t.items.some((i) => i.kind === "assistant" && i.text === evt.result)) {
        // avoid duplicate if already streamed
      }
      break;
    }
  }
}

function pushItem(t, item) {
  item.key = Math.random().toString(36).slice(2);
  t.items.push(item);
}

function appendAssistantText(t, text, isMeta) {
  // coalesce consecutive text into the last assistant item
  const last = t.items[t.items.length - 1];
  if (last && last.kind === "assistant" && !last.finalized && !isMeta) {
    last.text += text;
  } else {
    pushItem(t, { kind: "assistant", text, finalized: false, isMeta });
  }
  t.streaming = true; t.streamingRole = "assistant";
}

function flushAssistant(t) {
  const last = t.items[t.items.length - 1];
  if (last && last.kind === "assistant") last.finalized = true;
}

function upsertTool(t, tool) {
  // group tool_use + tool_result + progress together by id
  let existing = t.items.find((i) => i.kind === "tool" && i.id === tool.id);
  if (!existing) {
    existing = { kind: "tool", id: tool.id, name: tool.name, input: tool.input, result: null, status: "running", progress: null };
    pushItem(t, existing);
  }
  if (tool.name) existing.name = tool.name;
  if (tool.input !== undefined) existing.input = tool.input;
  if (tool.result !== undefined) existing.result = tool.result;
  if (tool.status) existing.status = tool.status;
}

function syncSessionMeta() {
  for (const t of Object.values(state.transcripts)) {
    const lastTool = [...t.items].reverse().find((i) => i.kind === "tool");
    if (t.items.some((i) => i.kind === "tool" && i.status === "running") || t.streaming) {
      // busy
    }
  }
}

// ---------- Rendering ----------
const ICONS = {
  menu: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M3 6h18M3 12h18M3 18h18"/></svg>`,
  plus: `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>`,
  send: `<svg width="19" height="19" viewBox="0 0 24 24" fill="currentColor"><path d="M3.4 20.4 21 12 3.4 3.6 3 10l12 2-12 2z"/></svg>`,
  stop: `<svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor"><rect x="5" y="5" width="14" height="14" rx="2"/></svg>`,
  close: `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 6l12 12M18 6L6 18"/></svg>`,
  bolt: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2L3 14h7l-1 8 10-12h-7l1-8z"/></svg>`,
  wrench: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.5-.5-.5-2.5z"/></svg>`,
  chev: `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>`,
};

function render() {
  const app = el("#app");
  if (!state.paired) { renderPair(); return; }
  app.innerHTML = "";

  const active = state.sessions.find((s) => s.id === state.activeId);
  const t = active ? ensureT(active.id) : null;

  const root = h("div", { class: "app" });

  // ---- topbar ----
  const topbar = h("div", { class: "topbar" },
    h("button", { class: "icon-btn", onclick: () => toggleDrawer(true), html: ICONS.menu }),
    h("div", { class: "title" },
      h("div", { class: "name" }, active ? (active.name || "Session") : "Synapse"),
      h("div", { class: "sub" },
        active ? h("span", { class: `status-dot ${active.state}` }) : null,
        active ? h("span", {}, active.model ? active.model : (active.cwd ? shortPath(active.cwd) : "ready")) : "no session",
      ),
    ),
    h("button", { class: "icon-btn", onclick: () => { state.newSheetOpen = true; render(); }, html: ICONS.plus }),
  );

  // ---- chat or empty ----
  let chatArea;
  if (!active) {
    chatArea = h("div", { class: "empty" },
      h("div", { html: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M13 2L3 14h7l-1 8 10-12h-7l1-8z"/></svg>` }),
      h("h2", {}, "No session"),
      h("p", {}, "Start a new Claude Code session to begin a conversation from your phone."),
      h("button", { class: "new-btn", style: "justify-content:center", onclick: () => { state.newSheetOpen = true; render(); } }, "+ New session"),
    );
  } else {
    chatArea = h("div", { class: "chat-scroll", id: "chatScroll" },
      h("div", { class: "msg-list", id: "msgList" }, ...renderItems(t)),
    );
  }

  // ---- composer ----
  const composerWrap = h("div", { class: "composer-wrap" });
  if (active) {
    const ta = h("textarea", { id: "composer", placeholder: "Message Claude…", rows: "1" });
    const busy = active.state === "busy" || (t && t.streaming);
    const sendBtn = h("button", {
      class: `send-btn ${busy ? "stop" : ""}`,
      onclick: () => { if (busy) { send({ op: "interrupt", sessionId: active.id }); } else doSend(active, ta); },
      html: busy ? ICONS.stop : ICONS.send,
    });
    ta.addEventListener("input", () => autoGrow(ta));
    ta.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey && window.matchMedia("(min-width: 600px)").matches) {
        e.preventDefault(); doSend(active, ta);
      }
    });
    composerWrap.append(h("div", { class: "composer" }, ta, sendBtn));
  }

  root.append(topbar, chatArea, composerWrap);

  // ---- drawer ----
  const scrim = h("div", { class: `scrim ${state.drawerOpen ? "open" : ""}`, onclick: () => toggleDrawer(false) });
  const drawer = renderDrawer();
  root.append(scrim, drawer);

  app.append(root);

  // ---- new session sheet ----
  if (state.newSheetOpen) app.append(renderNewSheet());

  // autoscroll
  if (active) {
    requestAnimationFrame(() => {
      const cs = el("#chatScroll");
      if (cs) cs.scrollTop = cs.scrollHeight;
    });
  }
}

function renderItems(t) {
  if (!t || !t.items.length) {
    return [h("div", { class: "empty", style: "height:auto;padding-top:80px" },
      h("p", { style: "color:var(--text-faint)" }, "Send a message to start."))];
  }
  return t.items.map((item) => renderItem(item, t));
}

function renderItem(item, t) {
  switch (item.kind) {
    case "user":
      return h("div", { class: "msg fade-in" },
        h("div", { class: "msg-role user" }, "You"),
        h("div", { class: "msg-bubble user md", html: renderMarkdown(item.text) }),
      );
    case "assistant":
      return h("div", { class: "msg fade-in" },
        h("div", { class: "msg-role assistant" }, "Claude"),
        h("div", { class: "msg-bubble assistant md", html: renderMarkdown(item.text) }),
      );
    case "tool":
      return renderTool(item, t);
    case "progress":
      return h("div", { class: "progress-line" }, h("span", { class: "spinner" }), h("span", {}, item.text));
    case "system":
      return h("div", { class: `sys-line ${item.error ? "error" : ""}` }, item.text);
    default:
      return null;
  }
}

function renderTool(item, t) {
  const open = t.openTools.has(item.id);
  const arg = toolArgPreview(item);
  const status = item.status === "done" ? "done" : item.status === "error" ? "error" : "";
  return h("div", { class: `tool ${open ? "open" : ""}` },
    h("button", {
      class: "tool-head",
      onclick: () => { if (open) t.openTools.delete(item.id); else t.openTools.add(item.id); render(); },
    },
      h("span", { class: "tool-icon", html: ICONS.wrench }),
      h("span", { class: "tool-name" }, item.name, arg ? h("span", { class: "tool-arg" }, " " + arg) : null),
      h("span", { class: `tool-status ${status}` }, item.status === "running" ? "…" : item.status),
      h("span", { class: "tool-chev", html: ICONS.chev }),
    ),
    h("div", { class: "tool-body" },
      item.input ? h("pre", {}, "input:\n" + fmt(item.input)) : null,
      item.result ? h("pre", {}, "output:\n" + truncate(item.result, 4000)) : null,
    ),
  );
}

function toolArgPreview(item) {
  if (!item.input) return "";
  if (item.name === "Bash" || item.name === "bash") return item.input.command || "";
  if (item.input.file_path) return shortPath(item.input.file_path);
  if (item.input.pattern) return item.input.pattern;
  if (item.input.path) return shortPath(item.input.path);
  if (item.input.command) return item.input.command;
  return "";
}

function fmt(v) { try { return typeof v === "string" ? v : JSON.stringify(v, null, 2); } catch { return String(v); } }
function truncate(s, n) { if (!s) return ""; s = String(s); return s.length > n ? s.slice(0, n) + "\n…(" + s.length + " chars)" : s; }
function shortPath(p) { if (!p) return ""; const parts = String(p).split("/"); return parts.slice(-2).join("/"); }

function renderDrawer() {
  const items = state.sessions.map((s) =>
    h("div", { class: `session-item ${s.id === state.activeId ? "active" : ""}`,
      onclick: () => { state.activeId = s.id; toggleDrawer(false); send({ op: "history", sessionId: s.id }); render(); } },
      h("div", { class: "si-top" },
        h("span", { class: `status-dot ${s.state}` }),
        h("span", { class: "si-name" }, s.name || "Session"),
      ),
      h("div", { class: "si-meta" }, s.model || "—"),
      h("div", { class: "si-cwd" }, shortPath(s.cwd || "")),
    ),
  );
  if (!items.length) items.push(h("div", { class: "si-meta", style: "padding:14px;text-align:center" }, "No sessions yet"));

  return h("div", { class: `drawer ${state.drawerOpen ? "open" : ""}` },
    h("div", { class: "drawer-head" },
      h("div", { class: "brand" },
        h("span", { html: `<svg viewBox="0 0 64 64"><g fill="none" stroke="#10a37f" stroke-width="3" stroke-linecap="round"><path d="M20 18c-6 6-6 22 0 28"/><path d="M44 18c6 6 6 22 0 28"/></g><circle cx="32" cy="32" r="5" fill="#10a37f"/></svg>` }),
        "Synapse",
      ),
      h("button", { class: "icon-btn", onclick: () => toggleDrawer(false), html: ICONS.close }),
    ),
    h("button", { class: "new-btn", onclick: () => { state.newSheetOpen = true; render(); } },
      h("span", { html: ICONS.plus }), "New session"),
    h("div", { class: "session-list" }, ...items),
  );
}

function renderNewSheet() {
  let opts = { name: "", model: "", cwd: "", permissionMode: "default", agent: "" };
  const scrim = h("div", { class: "modal-scrim", onclick: (e) => { if (e.target === scrim) { state.newSheetOpen = false; render(); } } });
  const sheet = h("div", { class: "sheet" },
    h("h3", {}, "New session"),
    field("Session name", h("input", { placeholder: "optional", oninput: (e) => opts.name = e.target.value })),
    field("Working directory", h("input", { placeholder: "/Users/you/project", oninput: (e) => opts.cwd = e.target.value })),
    field("Model", h("input", { placeholder: "sonnet, opus, …", oninput: (e) => opts.model = e.target.value })),
    field("Permission mode", sel(["default", "acceptEdits", "plan", "bypassPermissions"], (v) => opts.permissionMode = v, "default")),
    h("div", { class: "sheet-actions" },
      h("button", { class: "btn-ghost", onclick: () => { state.newSheetOpen = false; render(); } }, "Cancel"),
      h("button", { class: "btn-primary", onclick: () => createSession(opts) }, "Create"),
    ),
  );
  scrim.append(sheet);
  return scrim;
}

function field(label, control) {
  const wrap = h("div", { class: "field" });
  wrap.append(h("label", {}, label), control);
  return wrap;
}
function sel(options, onsel, def) {
  const s = h("select", { onchange: (e) => onsel(e.target.value) });
  for (const o of options) s.append(h("option", { value: o, selected: o === def }, o));
  return s;
}

function createSession(opts) {
  const clean = Object.fromEntries(Object.entries(opts).filter(([, v]) => v && v.trim()));
  send({ op: "create", opts: clean });
  state.newSheetOpen = false;
}

function doSend(active, ta) {
  const text = ta.value.trim();
  if (!text) return;
  // optimistic local render
  const t = ensureT(active.id);
  pushItem(t, { kind: "user", text });
  render();
  send({ op: "send", sessionId: active.id, content: text });
  ta.value = "";
  autoGrow(ta);
}

function autoGrow(ta) {
  ta.style.height = "auto";
  ta.style.height = Math.min(ta.scrollHeight, 140) + "px";
}

function toggleDrawer(open) { state.drawerOpen = open; render(); }

// ---------- demo mode (for previewing the UI without a live session) ----------
function loadDemo() {
  state.paired = true;
  state.token = "DEMO";
  const sid = "demo-1";
  state.sessions = [{
    id: sid, name: "Refactor auth module", cwd: "/Users/zx/code/synapse",
    model: "claude-opus-4-8", state: "idle", startedAt: Date.now() - 120000,
  }];
  state.activeId = sid;
  const t = ensureT(sid);
  const code = "function _checkBearer(req) {\n  const t = req.headers.authorization.replace(/^Bearer\\s+/i, \"\");\n  return t === this.token;\n}";
  pushItem(t, { kind: "user", text: "Summarize the auth module and add a token-refresh helper." });
  pushItem(t, { kind: "assistant", finalized: true, text: "Let me read the auth module first." });
  upsertTool(t, { id: "tu1", name: "Read", input: { file_path: "/Users/zx/code/synapse/src/server/server.js" }, status: "done", result: "312 lines. HTTP + WebSocket server with pairing auth and session-manager wiring." });
  pushItem(t, { kind: "assistant", finalized: true, text: "## Auth overview\n\nThe module handles **pairing** and session authorization:\n\n- Clients pair with a short token\n- WebSocket upgrades validate the bearer token\n- HTTP routes check the Authorization header\n\nCore check:\n\n```js\n" + code + "\n```\n\nNext I will add a refreshToken() helper." });
  upsertTool(t, { id: "tu2", name: "Bash", input: { command: "rg -n token src/server" }, status: "running" });
  pushItem(t, { kind: "progress", text: "Searching for token usages" });
  t.openTools.add("tu1");
}

function loadDemoDrawer() {
  state.paired = true;
  state.token = "DEMO";
  const a = "demo-a", b = "demo-b";
  state.sessions = [
    { id: a, name: "Refactor auth module", cwd: "/Users/zx/code/synapse", model: "claude-opus-4-8", state: "idle", startedAt: Date.now() - 120000 },
    { id: b, name: "Fix flaky CI tests", cwd: "/Users/zx/code/dcc", model: "claude-sonnet-4-6", state: "busy", startedAt: Date.now() - 600000 },
    { id: "demo-c", name: "Write API docs", cwd: "/Users/zx/code/llm-proxy", model: "claude-opus-4-8", state: "idle", startedAt: Date.now() - 900000 },
  ];
  state.activeId = a;
  state.drawerOpen = true;
  const t = ensureT(a);
  pushItem(t, { kind: "user", text: "Summarize the auth module." });
  pushItem(t, { kind: "assistant", finalized: true, text: "See the drawer for your other sessions." });
}


// ---------- boot ----------
(function init() {
  const params = new URLSearchParams(location.search);
  const d = params.get("demo");
  if (d === "drawer") { loadDemoDrawer(); render(); return; }
  if (d) { loadDemo(); render(); return; }
    const stored = getStored();
  const urlToken = getTokenFromUrl();
  const token = urlToken || stored.token;
  if (token) { state.token = token; connect(); render(); }
  else { renderPair(); }
})();
