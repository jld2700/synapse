# Synapse

Remote mobile control surface for the **Claude Code CLI**. Pair your phone to a
machine running `claude` and drive coding sessions from anywhere — UI/UX aligned
to the Codex mobile app (dark theme, session drawer, streaming chat, tool cards).

![chat](https://img.shields.io/badge/UI-Codex%20mobile%20aligned-10a37f) ![node](https://img.shields.io/badge/node-%3E%3D18-10a37f) ![deps](https://img.shields.io/badge/dependencies-zero-10a37f)

## How it works

Synapse wraps the **supported, public** Claude Code streaming transport
(`claude -p --input-format stream-json --output-format stream-json`) and exposes
it over a tiny HTTP + WebSocket server your phone connects to.

```
 Phone (mobile web app)
      │  WebSocket (token-paired)
      ▼
 Synapse server  ──►  SessionManager  ──►  ClaudeSession (bridge)
                                                │ spawns `claude -p` per turn
                                                ▼
                                         Claude Code CLI  ──►  your model gateway
```

- **One logical remote session = one Claude Code session id.** Multi-turn
  continuity uses Claude Code's own session store via `--resume`.
- **Per-turn child process.** Each user message spawns a `claude -p` run that
  streams assistant / system / tool events back live, then exits. This mirrors
  how the official SDK drives the CLI (one-shot streams flush reliably).
- **Automatic streaming fallback.** Some API gateways don't return output under
  `stream-json`. If a turn yields no events, the bridge transparently retries in
  buffered `--output-format json` mode so the UI still receives the full turn.

## Run

```bash
# from the repo root
node src/index.js
# or with options
node src/index.js --port 4173 --cwd /Users/you/code/project --token MYCODE
```

Output prints a pairing code and URLs. On your phone, open
`http://<your-lan-ip>:<port>?token=<CODE>` (the token is filled in automatically
from the URL) and tap **Connect**. Add the page to your home screen for a
full-screen, standalone app experience (PWA manifest included).

### Options

| Flag | Default | Description |
| --- | --- | --- |
| `--port`, `-p` | `4173` | HTTP/WS port |
| `--host` | `0.0.0.0` | bind host |
| `--cwd` | current dir | default working directory for new sessions |
| `--token` | random 6-char | fixed pairing token |
| `--bin` | auto-detect | path to the `claude` binary |
| `--dev` | off | verbose event logging |

## Features

- **Session drawer** — list / switch / create sessions, each with live status.
- **Streaming chat** — assistant messages render as Markdown with code blocks.
- **Tool cards** — `tool_use` / `tool_result` events collapse into tappable
  cards showing the tool name, a one-line arg preview, and full input/output.
- **Interrupt** — the send button becomes a stop button while a turn is busy.
- **Pairing + auth** — short human-readable code gates both HTTP API and WS.
- **New-session sheet** — set name, working dir, model, permission mode, agent.
- **Mobile-first UI** — safe-area aware, dark theme, no zoom, PWA installable.
- **Zero runtime dependencies** — Node stdlib only; no install step, no build.

## Project layout

- `src/bridge/claude-bridge.js` — drives `claude -p`; turn-based, streaming +
  json fallback, `--resume` continuity, interrupt.
- `src/server/session-manager.js` — owns `ClaudeSession`s, broadcasts events.
- `src/server/server.js` — HTTP (static + REST) + minimal RFC6455 WebSocket.
- `src/index.js` — CLI entrypoint.
- `public/` — `index.html`, `app.css`, `app.js` (UI), `md.js` (markdown), PWA.
- `AGENTS.md` — conventions for agents working in this repo.

## Preview the UI without a live session

Open `http://localhost:<port>/?demo=1` to render the app with a seeded demo
conversation (used for the design screenshots).
