// HTTP + WebSocket server for Synapse.
// - Serves the mobile web UI from public/
// - Pairs clients via a short token (printed on startup, also as QR-ready text)
// - Authenticates WebSocket upgrades with a bearer token

import { createServer } from "node:http";
import { randomBytes } from "node:crypto";
import { readFile, stat } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { networkInterfaces } from "node:os";

const __dirname = join(fileURLToPath(import.meta.url), "..");
const PUBLIC_DIR = join(__dirname, "..", "..", "public");

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
};

function genToken(n = 6) {
  // human-friendly digits, avoid ambiguous chars
  const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
  const buf = randomBytes(n);
  let out = "";
  for (let i = 0; i < n; i++) out += alphabet[buf[i] % alphabet.length];
  return out;
}

export function localIPs() {
  const ips = [];
  for (const ifaces of Object.values(networkInterfaces())) {
    for (const iface of ifaces || []) {
      if (iface.family === "IPv4" && !iface.internal) ips.push(iface.address);
    }
  }
  return ips;
}

// Minimal RFC6455 WebSocket server (text frames) — no external deps.
export class SynapseServer {
  constructor({ manager, port = 0, host = "0.0.0.0", token } = {}) {
    this.manager = manager;
    this.port = port;
    this.host = host;
    this.token = token || genToken(6);
    this.http = createServer((req, res) => this._handleHttp(req, res));
    this.wsGUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    this.connections = new Set();
  }

  async start() {
    await new Promise((resolve, reject) => {
      this.http.on("error", reject);
      this.http.on("upgrade", (req, socket, head) => this._handleUpgrade(req, socket, head));
      this.http.listen(this.port, this.host, () => {
        this.port = this.http.address().port;
        resolve();
      });
    });
  }

  url() {
    const ips = localIPs();
    const addrs = [`http://${this.host}:${this.port}`];
    for (const ip of ips) addrs.push(`http://${ip}:${this.port}`);
    return addrs;
  }

  pairingInfo() {
    return {
      token: this.token,
      urls: this.url(),
      qr: this._qrText(),
    };
  }

  _qrText() {
    // Deep link the app can parse: synapse://pair?host=...&port=...&token=...
    const ip = localIPs()[0] || this.host;
    return `synapse://pair?host=${ip}&port=${this.port}&token=${this.token}`;
  }

  async _handleHttp(req, res) {
    const url = req.url.split("?")[0];

    // API routes
    if (url === "/api/pair" && req.method === "GET") {
      return this._json(res, 200, { ok: true });
    }
    if (url === "/api/health" && req.method === "GET") {
      return this._json(res, 200, { ok: true, sessions: this.manager.list().length });
    }
    if (url === "/api/sessions" && req.method === "GET") {
      if (!this._checkBearer(req)) return this._json(res, 401, { error: "unauthorized" });
      return this._json(res, 200, { sessions: this.manager.list() });
    }
    if (url === "/api/sessions" && req.method === "POST") {
      if (!this._checkBearer(req)) return this._json(res, 401, { error: "unauthorized" });
      let body = await this._readBody(req);
      try { body = JSON.parse(body || "{}"); } catch { return this._json(res, 400, { error: "bad json" }); }
      try {
        const s = await this.manager.create(body);
        return this._json(res, 200, { session: this.manager._summary(s) });
      } catch (e) {
        return this._json(res, 500, { error: e.message });
      }
    }

    // Static UI
    let path = url === "/" ? "/index.html" : url;
    const filePath = normalize(join(PUBLIC_DIR, path));
    if (!filePath.startsWith(PUBLIC_DIR)) return this._json(res, 403, { error: "forbidden" });
    try {
      const st = await stat(filePath);
      if (st.isDirectory()) return this._serve(join(filePath, "index.html"), res);
      return this._serve(filePath, res);
    } catch {
      // SPA fallback
      return this._serve(join(PUBLIC_DIR, "index.html"), res);
    }
  }

  async _serve(filePath, res) {
    try {
      const data = await readFile(filePath);
      res.writeHead(200, { "Content-Type": MIME[extname(filePath)] || "application/octet-stream" });
      res.end(data);
    } catch {
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "not found" }));
    }
  }

  _checkBearer(req) {
    const auth = req.headers["authorization"] || "";
    const q = new URL("http://x" + req.url).searchParams.get("token");
    const token = auth.replace(/^Bearer\s+/i, "") || q;
    return token === this.token;
  }

  _readBody(req) {
    return new Promise((resolve) => {
      let data = "";
      req.on("data", (c) => (data += c));
      req.on("end", () => resolve(data));
    });
  }

  _json(res, code, obj) {
    res.writeHead(code, { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store" });
    res.end(JSON.stringify(obj));
  }

  // ---- WebSocket ----
  _handleUpgrade(req, socket, head) {
    const url = new URL(req.url, "http://localhost");
    const token = url.searchParams.get("token");
    if (token !== this.token) {
      socket.write("HTTP/1.1 401 Unauthorized\r\n\r\n");
      socket.destroy();
      return;
    }
    const key = req.headers["sec-websocket-key"];
    if (!key) { socket.destroy(); return; }
    // accept
    const { createHash } = require_node_crypto();
    const accept = createHash("sha1").update(key + this.wsGUID).digest("base64");
    const headers = [
      "HTTP/1.1 101 Switching Protocols",
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Accept: ${accept}`,
      "\r\n",
    ].join("\r\n");
    socket.write(headers);

    const conn = { socket, alive: true, subscriptions: new Set() };
    this.connections.add(conn);
    socket.on("data", (buf) => this._handleWsData(conn, buf));
    socket.on("end", () => this.connections.delete(conn));
    socket.on("close", () => this.connections.delete(conn));
    socket.on("error", () => this.connections.delete(conn));

    // subscribe to all manager events and push to this client
    const unsub = this.manager.subscribe((evt) => this._wsSend(conn, { type: "event", event: evt }));
    socket.on("close", unsub);

    // initial snapshot
    this._wsSend(conn, { type: "hello", sessions: this.manager.list(), pairing: this.pairingInfo() });

    if (head && head.length) { /* ignore leftover */ }
  }

  _handleWsData(conn, buf) {
    // Minimal frame parser for client->server text/JSON commands.
    let idx = 0;
    while (idx < buf.length) {
      const b0 = buf[idx];
      const fin = (b0 & 0x80) !== 0;
      const opcode = b0 & 0x0f;
      const masked = (buf[idx + 1] & 0x80) !== 0;
      let len = buf[idx + 1] & 0x7f;
      let ptr = idx + 2;
      if (len === 126) { len = buf.readUInt16BE(ptr); ptr += 2; }
      else if (len === 127) { len = Number(buf.readBigUInt64BE(ptr)); ptr += 8; }
      let mask = null;
      if (masked) { mask = buf.subarray(ptr, ptr + 4); ptr += 4; }
      const payload = buf.subarray(ptr, ptr + len);
      const data = mask ? Buffer.from(payload.map((b, i) => b ^ mask[i % 4])) : payload;
      idx = ptr + len;
      if (opcode === 0x8) { try { conn.socket.end(); } catch {} return; } // close
      if (opcode === 0x9) { this._wsSendRaw(conn.socket, 0x0a, data); continue; } // pong
      if (opcode === 0x1 && fin) {
        let msg; try { msg = JSON.parse(data.toString("utf8")); } catch { continue; }
        this._handleWsCommand(conn, msg).catch(() => {});
      }
    }
  }

  async _handleWsCommand(conn, msg) {
    const { op, sessionId, content, opts } = msg;
    switch (op) {
      case "create": {
        const s = await this.manager.create(opts || {});
        this._wsSend(conn, { type: "created", session: this.manager._summary(s) });
        break;
      }
      case "send": {
        try { await this.manager.send(sessionId, content); }
        catch (e) { this._wsSend(conn, { type: "error", error: e.message, op }); }
        break;
      }
      case "interrupt": {
        this.manager.interrupt(sessionId);
        break;
      }
      case "destroy": {
        await this.manager.destroy(sessionId);
        break;
      }
      case "list": {
        this._wsSend(conn, { type: "sessions", sessions: this.manager.list() });
        break;
      }
      case "history": {
        const sinceSeq = msg.sinceSeq || 0;
        const h = this.manager.history(sessionId, { sinceSeq }) || [];
        this._wsSend(conn, { type: "history", sessionId, events: h });
        break;
      }
      default:
        this._wsSend(conn, { type: "error", error: "unknown op", op });
    }
  }

  _wsSend(conn, obj) {
    this._wsSendRaw(conn.socket, 0x01, Buffer.from(JSON.stringify(obj), "utf8"));
  }

  _wsSendRaw(socket, opcode, payload) {
    const masked = false;
    const len = payload.length;
    let header;
    if (len < 126) {
      header = Buffer.alloc(2);
      header[1] = len;
    } else if (len < 65536) {
      header = Buffer.alloc(4);
      header[1] = 126;
      header.writeUInt16BE(len, 2);
    } else {
      header = Buffer.alloc(10);
      header[1] = 127;
      header.writeBigUInt64BE(BigInt(len), 2);
    }
    header[0] = 0x80 | opcode; // FIN
    if (masked) header[1] |= 0x80;
    try { socket.write(Buffer.concat([header, payload])); } catch {}
  }
}

// avoid top-level import cycle quirk: require crypto lazily
import * as nodeCrypto from "node:crypto";
function require_node_crypto() { return nodeCrypto; }
