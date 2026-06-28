// Claude Code bridge.
//
// Claude Code's `claude -p --input-format stream-json` flushes its full event
// stream only when stdin reaches EOF (one-shot / SDK mode). It does NOT stream
// incrementally on a long-lived stdin. So a remote "session" is modeled as:
//   - a stable Claude Code session-id (from the first turn's `result` event)
//   - a per-turn child process: `claude -p ... [--resume <sid>]`
// Each turn streams the assistant/system/tool events live to subscribers, then
// exits. Multi-turn continuity is provided by Claude Code's own session store
// via --resume. This mirrors how the official SDK drives the CLI.

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const CANDIDATE_BINS = [
  process.env.CLAUDE_BIN,
  join(homedir(), ".hermes/node/bin/claude"),
  join(homedir(), ".claude/local/claude"),
  "/usr/local/bin/claude",
  "/opt/homebrew/bin/claude",
  "claude",
];

export function resolveClaudeBin(explicit) {
  if (explicit && existsSync(explicit)) return explicit;
  for (const c of CANDIDATE_BINS) {
    if (!c) continue;
    if (c === "claude") return c;
    if (existsSync(c)) return c;
  }
  return "claude";
}

// One logical remote session = config + a Claude Code session id (lazily set).
export class ClaudeSession {
  constructor({ id, cwd, name, model, permissionMode, agent, claudeBin, env, onEvent, onClose, extraArgs = [] }) {
    this.id = id;
    this.cwd = cwd;
    this.name = name;
    this.model = model;
    this.permissionMode = permissionMode;
    this.agent = agent;
    this.claudeBin = claudeBin;
    this.env = env;
    this.onEvent = onEvent || (() => {});
    this.onClose = onClose || (() => {});
    this.extraArgs = extraArgs || [];
    // claudeCodeSessionId is acquired on the first turn from the result event.
    this.claudeCodeSessionId = null;
    this.state = "idle"; // idle | busy | stopped | error
    this.startedAt = Date.now();
    this.lastError = null;
    this.events = [];
    this._cap = 1000;
    this._seq = 0;
    this._current = null; // active turn child process
    this._queue = [];
  }

  // streaming=true : stream-json input + stream-json output (live events)
  // streaming=false: text input (the prompt on stdin) + json output (buffered)
  //   — stream-json input REQUIRES stream-json output, so the json fallback
  //     must switch the input format too.
  _baseArgs(streaming) {
    const args = ["-p", "--verbose"];
    if (streaming) {
      args.push("--input-format", "stream-json", "--output-format", "stream-json",
                "--include-partial-messages");
    } else {
      args.push("--output-format", "json");
    }
    if (this.permissionMode) args.push("--permission-mode", this.permissionMode);
    if (this.model) args.push("--model", this.model);
    if (this.agent) args.push("--agent", this.agent);
    if (this.claudeCodeSessionId) args.push("--resume", this.claudeCodeSessionId);
    args.push(...this.extraArgs);
    return args;
  }

  // Run one turn. streaming=true emits events live (stream-json);
  // streaming=false buffers and parses the json array (more compatible with
  // some API gateways). We try streaming first; if it yields no events we
  // transparently retry the turn in buffered-json mode.
  async _runTurn(content) {
    this.state = "busy";
    this._emit({ type: "system", subtype: "turn_started", sessionId: this.id });
    let produced = await this._execTurn(content, true);
    if (produced === 0) {
      // streaming produced nothing for this gateway/model; fall back to json.
      this._emit({ type: "system", subtype: "fallback_to_json", sessionId: this.id });
      produced = await this._execTurn(content, false);
    }
    this.state = produced > 0 ? "idle" : "error";
    return produced;
  }

  _execTurn(content, streaming) {
    return new Promise((resolve) => {
      const args = this._baseArgs(streaming);
      const spawnOpts = { stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, ...(this.env || {}) } };
      if (this.cwd) spawnOpts.cwd = this.cwd;
      const child = spawn(this.claudeBin, args, spawnOpts);
      this._current = child;
      let produced = 0;
      let buf = "";

      const errRl = createInterface({ input: child.stderr });
      errRl.on("line", (line) => {
        this._emit({ type: "stderr", sessionId: this.id, text: String(line) });
      });

      if (streaming) {
        const rl = createInterface({ input: child.stdout });
        rl.on("line", (line) => { if (this._ingest(line)) produced++; });
      } else {
        // buffer entire stdout, parse as JSON array at the end
        child.stdout.on("data", (d) => (buf += d));
      }

      if (streaming) {
        const blocks = typeof content === "string"
          ? [{ type: "text", text: content }]
          : content;
        const msg = { type: "user_message", message: { role: "user", content: blocks } };
        child.stdin.write(JSON.stringify(msg) + "\n");
      } else {
        // text input: content is the prompt. If structured, flatten to text.
        const text = typeof content === "string"
          ? content
          : (Array.isArray(content) ? content.map((b) => b.text || "").join("\n") : String(content));
        child.stdin.write(text);
      }
      child.stdin.end();

      const finish = (code) => {
        this._current = null;
        if (!streaming) {
          // echo the user turn first (text-input mode doesn't return it)
          const blocks = typeof content === "string"
            ? [{ type: "text", text: content }]
            : Array.isArray(content) ? content : [{ type: "text", text: String(content) }];
          this._emit({ type: "user", sessionId: this.id, message: { role: "user", content: blocks } });
          produced++;
          if (buf.trim()) {
            try {
              const arr = JSON.parse(buf);
              if (Array.isArray(arr)) {
                for (const evt of arr) { if (this._ingest(JSON.stringify(evt))) produced++; }
              }
            } catch {
              this._emit({ type: "stderr", sessionId: this.id, text: buf.slice(-500) });
            }
          }
        }
        resolve(produced);
      };
      child.on("exit", finish);
      child.on("error", (err) => {
        this.lastError = err.message;
        this._emit({ type: "system", subtype: "bridge_error", sessionId: this.id, error: err.message });
        this._current = null;
        resolve(produced);
      });
    });
  }

  async sendUserMessage(content) {
    if (this.state === "stopped" || this.state === "error" && this._current) {
      throw new Error(`session ${this.id} is stopped`);
    }
    this._queue.push(content);
    if (this._current) return true; // queued
    while (this._queue.length) {
      const c = this._queue.shift();
      await this._runTurn(c);
    }
    return true;
  }

  // Best-effort interrupt: kill the active turn process. Claude Code will emit
  // a result with an interrupted stop reason.
  interrupt() {
    const child = this._current;
    if (!child) return false;
    try { child.kill("SIGINT"); } catch { try { child.kill("SIGTERM"); } catch { return false; } }
    return true;
  }

  _ingest(line) {
    if (!line) return false;
    let evt;
    try { evt = JSON.parse(line); } catch {
      this._emit({ type: "stderr", sessionId: this.id, text: line });
      return false;
    }
    // Only adopt a Claude Code session id from authoritative sources:
    //  - system:init (the real persistent session id)
    //  - a non-error result (confirms the conversation was persisted)
    // Never from hook/system chatter, which may carry transient ids that
    // cannot be resumed.
    if (evt.type === "system" && evt.subtype === "init" && evt.session_id) {
      this.claudeCodeSessionId = evt.session_id;
    } else if (evt.type === "result" && evt.session_id && !evt.is_error) {
      this.claudeCodeSessionId = evt.session_id;
    }
    this._emit(evt);
    // count substantive events (not just hook chatter) for fallback detection
    return evt.type === "assistant" || evt.type === "result" || evt.type === "user" || evt.type === "progress";
  }

  _emit(evt) {
    if (evt.type !== "stderr") {
      evt._seq = ++this._seq;
      this.events.push(evt);
      if (this.events.length > this._cap) this.events.splice(0, this.events.length - this._cap);
    }
    try { this.onEvent(evt); } catch {}
  }

  async stop() {
    this._queue.length = 0;
    const child = this._current;
    if (child) {
      try { child.kill("SIGTERM"); } catch {}
      await new Promise((r) => setTimeout(r, 500));
      try { child.kill("SIGKILL"); } catch {}
    }
    this.state = "stopped";
  }
}

export function newSessionId() {
  return randomUUID();
}

// Discover existing sessions managed by `claude agents`.
export async function listManagedSessions({ claudeBin, cwd } = {}) {
  const bin = claudeBin || resolveClaudeBin();
  return new Promise((resolve) => {
    const args = ["agents", "--json"];
    if (cwd) args.push("--cwd", cwd);
    const child = spawn(bin, args, { stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", () => {});
    child.on("error", () => resolve(null));
    child.on("exit", () => { try { resolve(JSON.parse(out)); } catch { resolve(null); } });
  });
}
