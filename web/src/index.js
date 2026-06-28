#!/usr/bin/env node
// Synapse — remote mobile control for the Claude Code CLI.
import { SessionManager } from "./server/session-manager.js";
import { SynapseServer, localIPs } from "./server/server.js";
import { resolveClaudeBin } from "./bridge/claude-bridge.js";
import { syncManaged } from "./server/attach.js";

function parseArgs(argv) {
  const out = { port: 4173, host: "0.0.0.0", cwd: process.cwd(), dev: false, token: null, bin: null };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--port" || a === "-p") out.port = Number(argv[++i]);
    else if (a === "--host") out.host = argv[++i];
    else if (a === "--cwd") out.cwd = argv[++i];
    else if (a === "--token") out.token = argv[++i];
    else if (a === "--bin") out.bin = argv[++i];
    else if (a === "--dev") out.dev = true;
    else if (a === "--help" || a === "-h") out.help = true;
  }
  return out;
}

function help() {
  console.log(`Synapse — remote mobile control for the Claude Code CLI.

Usage:
  synapse [options]

Options:
  --port, -p <n>   HTTP/WS port (default 4173)
  --host <addr>    bind host (default 0.0.0.0)
  --cwd <path>     default working dir for new sessions (default: cwd)
  --token <code>   fixed pairing token (default: random 6-char code)
  --bin <path>     path to the claude binary (default: auto-detect)
  --dev            verbose logging
  -h, --help       show this help
`);
}

async function main() {
  const args = parseArgs(process.argv);
  if (args.help) { help(); return; }

  const bin = resolveClaudeBin(args.bin);
  const manager = new SessionManager({ claudeBin: bin, defaultCwd: args.cwd });
  const server = new SynapseServer({ manager, port: args.port, host: args.host, token: args.token });

  await server.start();

  // Attach to Claude Code sessions already running on this machine.
  try {
    const attached = await syncManaged(manager, { claudeBin: bin });
    if (attached.length) console.log(`  Attached ${attached.length} existing Claude Code session(s).`);
  } catch (e) { if (args.dev) console.error("attach sync failed:", e.message); }

  const info = server.pairingInfo();
  console.log("\n  Synapse is running.\n");
  console.log("  Claude binary:  " + bin);
  console.log("  Working dir:    " + args.cwd);
  console.log("  Pairing token:  " + info.token);
  console.log("\n  Open on your phone:");
  for (const u of info.urls) {
    console.log("    " + u + "?token=" + info.token);
  }
  console.log("\n  Or scan / use the pairing link:");
  console.log("    " + info.qr);
  console.log("");

  if (args.dev) {
    manager.subscribe((evt) => {
      const t = evt.type + (evt.subtype ? ":" + evt.subtype : "");
      console.log("[evt]", t, evt.sessionId?.slice(0, 8));
    });
  }

  const shutdown = async (sig) => {
    console.log(`\n${sig} received, shutting down…`);
    for (const s of manager.sessions.values()) { await s.stop(); }
    process.exit(0);
  };
  process.on("SIGINT", () => shutdown("SIGINT"));
  process.on("SIGTERM", () => shutdown("SIGTERM"));
}

main().catch((e) => { console.error(e); process.exit(1); });
