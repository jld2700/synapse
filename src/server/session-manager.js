// Owns the set of ClaudeSession instances and broadcasts events to subscribers.

import { ClaudeSession, newSessionId, resolveClaudeBin } from "../bridge/claude-bridge.js";

export class SessionManager {
  constructor({ claudeBin, defaultCwd } = {}) {
    this.claudeBin = resolveClaudeBin(claudeBin);
    this.defaultCwd = defaultCwd || process.cwd();
    this.sessions = new Map(); // id -> ClaudeSession
    this.subscribers = new Set(); // fn(evt)
  }

  subscribe(fn) {
    this.subscribers.add(fn);
    return () => this.subscribers.delete(fn);
  }

  _broadcast(evt) {
    for (const fn of this.subscribers) {
      try { fn(evt); } catch {}
    }
  }

  list() {
    return [...this.sessions.values()].map((s) => ({
      id: s.id,
      name: s.name,
      cwd: s.cwd,
      model: s.model,
      agent: s.agent,
      state: s.state,
      startedAt: s.startedAt,
      lastError: s.lastError,
      eventCount: s.events.length,
    }));
  }

  get(id) {
    return this.sessions.get(id);
  }

  history(id, opts = {}) {
    const s = this.sessions.get(id);
    if (!s) return null;
    const since = opts.sinceSeq || 0;
    return s.events.filter((e) => (e._seq || 0) > since);
  }

  async create({ cwd, name, model, permissionMode, agent, env, extraArgs } = {}) {
    const id = newSessionId();
    const session = new ClaudeSession({
      id,
      cwd: cwd || this.defaultCwd,
      name,
      model,
      permissionMode,
      agent,
      claudeBin: this.claudeBin,
      env,
      extraArgs,
      onEvent: (evt) => {
        const s = session;
        if (evt.subtype === "turn_started" || evt.type === "assistant" || evt.type === "progress") s.state = "busy";
        else if (evt.type === "result") s.state = evt.is_error ? "error" : "idle";
        else if (evt.subtype === "bridge_error") s.state = "error";
        this._broadcast({ ...evt, sessionId: id });
      },
      onClose: () => this._broadcast({ type: "system", subtype: "session_closed", sessionId: id }),
    });
    this.sessions.set(id, session);
    this._broadcast({ type: "system", subtype: "session_created", sessionId: id, session: this._summary(session) });
    return session;
  }

  _summary(s) {
    return { id: s.id, name: s.name, cwd: s.cwd, model: s.model, agent: s.agent, state: s.state, startedAt: s.startedAt };
  }

  async send(id, content) {
    const s = this.sessions.get(id);
    if (!s) throw new Error("unknown session");
    return s.sendUserMessage(content);
  }

  interrupt(id) {
    const s = this.sessions.get(id);
    if (!s) return false;
    return s.interrupt();
  }

  async destroy(id) {
    const s = this.sessions.get(id);
    if (!s) return false;
    await s.stop();
    this.sessions.delete(id);
    this._broadcast({ type: "system", subtype: "session_destroyed", sessionId: id });
    return true;
  }
}
