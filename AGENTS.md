# Synapse — AGENTS.md

Synapse is a remote mobile control surface for the **Claude Code CLI** (`claude`).
It pairs a phone/mobile browser to one or more local `claude` sessions and
streams messages bidirectionally.

## Architecture
- `src/bridge/` — wraps the `claude` binary using its supported streaming
  transport: `claude -p --input-format stream-json --output-format stream-json`.
  Each session is one long-lived child process. Lines on stdin are user turns;
  lines on stdout are assistant/system/tool events.
- `src/server/` — HTTP + WebSocket server. Holds the SessionManager. Mobile
  clients connect over WS. Pairing is a short token + optional bearer.
- `src/web/` + `public/` — single-page mobile web UI, design aligned to the
  Codex mobile app (dark theme, session drawer, streaming chat, tool cards).
- `src/index.js` — single CLI entrypoint (`node src/index.js` or `npm start`).

## Conventions
- Node ESM (`"type":"module"`), no build step, no external deps in v1.
- Keep the dependency footprint minimal. Stdlib first.
- Dark-first mobile UI; all sizes in rem; safe-area aware.
