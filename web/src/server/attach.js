// Attach to existing Claude Code sessions discovered via `claude agents --json`.
// These sessions are already running interactively or as background agents on
// this machine. We model an attached session as a ClaudeSession that, on its
// first turn, --resume's into the discovered session id so new messages
// continue the existing conversation rather than starting fresh.

import { ClaudeSession } from "../bridge/claude-bridge.js";

export function attachManaged(manager, entry) {
  // entry: { sessionId, cwd, name, kind, state, ... } from `claude agents --json`
  if (!entry || !entry.sessionId) return null;
  // avoid double-attaching
  for (const s of manager.sessions.values()) {
    if (s.claudeCodeSessionId === entry.sessionId) return s;
  }
  const session = new ClaudeSession({
    id: entry.sessionId,
    cwd: entry.cwd || manager.defaultCwd,
    name: entry.name || (entry.kind === "background" ? "Background agent" : "Interactive"),
    claudeBin: manager.claudeBin,
    // seed the Claude Code session id so turns immediately --resume into it
    // (set via a private field the bridge reads)
  });
  session.claudeCodeSessionId = entry.sessionId;
  session.state = entry.state === "blocked" ? "idle" : (entry.state || "idle");
  session.attached = true;
  session.startedAt = entry.startedAt || Date.now();

  session.onEvent = (evt) => {
    const s = session;
    if (evt.subtype === "turn_started" || evt.type === "assistant" || evt.type === "progress") s.state = "busy";
    else if (evt.type === "result") s.state = evt.is_error ? "error" : "idle";
    manager._broadcast({ ...evt, sessionId: session.id });
  };
  session.onClose = () => manager._broadcast({ type: "system", subtype: "session_closed", sessionId: session.id });

  manager.sessions.set(session.id, session);
  manager._broadcast({ type: "system", subtype: "session_created", sessionId: session.id, session: manager._summary(session) });
  return session;
}

export async function syncManaged(manager, { claudeBin } = {}) {
  const { listManagedSessions } = await import("../bridge/claude-bridge.js");
  const list = await listManagedSessions({ claudeBin: claudeBin || manager.claudeBin });
  if (!Array.isArray(list)) return [];
  const attached = [];
  for (const entry of list) {
    const s = attachManaged(manager, entry);
    if (s) attached.push(s);
  }
  return attached;
}
