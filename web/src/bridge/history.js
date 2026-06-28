// Load a persisted Claude Code session transcript from disk and return the
// conversational events (user / assistant / tool_use / tool_result) in order,
// shaped exactly like the live stream-json events the UI already ingests.
//
// Claude Code stores transcripts at:
//   ~/.claude/projects/<cwd-with-slashes-as-dashes>/<sessionId>.jsonl
// Each line is one event (user, assistant, system, attachment, ...). We keep
// only the message-bearing events so the backfilled transcript matches what a
// live session would have streamed.

import { homedir } from "node:os";
import { join } from "node:path";
import { createReadStream } from "node:fs";
import { createInterface } from "node:readline";

const CLAUDE_DIR = process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude");

// Mirror Claude Code's cwd-encoding (Db() in the binary): replace non-alnum
// with '-', then truncate to a max length with a stable suffix.
function encodeCwd(cwd) {
  const clean = String(cwd || "").replace(/[^a-zA-Z0-9]/g, "-");
  return clean; // Claude Code keeps the full encoded path as the dir name
}

function projectDir(cwd) {
  return join(CLAUDE_DIR, "projects", encodeCwd(cwd));
}

export function transcriptPath(cwd, sessionId) {
  return join(projectDir(cwd), `${sessionId}.jsonl`);
}

// Read a transcript and yield normalized conversational events.
// Resolves to { events, found }.
export function loadTranscript(cwd, sessionId, { limit = 400 } = {}) {
  return new Promise((resolve) => {
    const path = transcriptPath(cwd, sessionId);
    const events = [];
    let rl;
    try {
      rl = createInterface({ input: createReadStream(path), crlfDelay: Infinity });
    } catch {
      resolve({ events: [], found: false });
      return;
    }
    let count = 0;
    rl.on("line", (line) => {
      if (!line) return;
      let evt;
      try { evt = JSON.parse(line); } catch { return; }
      const t = evt.type;
      // Only keep message-bearing events the UI renders. Skip bookkeeping
      // lines (mode, permission-mode, last-prompt, queue-operation, etc.).
      if (t !== "user" && t !== "assistant" && t !== "system") return;
      // Skip meta/system noise
      if (t === "system" && evt.subtype !== "init") return;
      // Skip sidechain (sub-agent) messages to keep the main thread readable
      if (evt.isSidechain) return;
      // Normalize into the shape the UI expects from the live stream
      const out = { ...evt, sessionId };
      // drop fields that are transcript-internal
      delete out.parentUuid;
      delete out.promptId;
      events.push(out);
      count++;
    });
    rl.on("close", () => {
      // Cap the number of events sent to the client to bound payload size,
      // keeping the most recent `limit`.
      const trimmed = count > limit ? events.slice(-limit) : events;
      resolve({ events: trimmed, found: count > 0 });
    });
    rl.on("error", () => resolve({ events: [], found: false }));
  });
}
