# Synapse

Remote mobile control for the **Claude Code CLI** (`claude -p`). Drive Claude
Code sessions from your phone — create sessions, send prompts, watch streamed
tool calls, stop a running turn, and attach to sessions already running on your
machine.

Built in **Rust** for true multi-platform output: a small HTTP/WebSocket
server wraps the `claude` CLI, and a native **Slint** UI compiles to desktop,
iOS, and Android from one codebase. The UI/UX mirrors the **ChatGPT mobile**
app (light theme, right-aligned user bubbles, full-width assistant rows with a
circular avatar, pill-shaped floating composer, minimal top bar).

## Architecture

```
┌──────────────┐   WebSocket (JSON ops/events)   ┌─────────────────┐
│  synapse-app │ ◄──────────────────────────────► │ synapse-server  │
│  (Slint UI)  │                                  │  (axum + WS)    │
└──────────────┘                                  └────────┬────────┘
                                                           │ spawns / attaches
                                                           ▼
                                                   ┌─────────────────┐
                                                   │   claude -p     │
                                                   │ (stream-json)   │
                                                   └─────────────────┘
```

| Crate | Role |
|-------|------|
| `crates/server` | axum + tokio-tungstenite service. Spawns `claude -p --output-format stream-json`, parses events, broadcasts to clients, and attaches to existing Claude Code sessions. |
| `crates/app` | Slint 1.7 pure-Rust UI (`backend-winit` + `renderer-femtovg`) that connects to the server over WS. |
| `web/` | Original Node.js prototype, kept as a design reference. Not required to run the Rust app. |

## Requirements

- Rust toolchain (stable)
- The `claude` CLI on `PATH` (or pass `--bin`). Synapse resolves it via the
  same lookup the CLI ships, e.g. `/Users/<you>/.hermes/node/bin/claude`.

## Build

```sh
# from the workspace root — builds both the server and the app
cargo build --release
```

Binaries land in `target/release/`:

- `synapse-server` — the bridge service
- `synapse-app` — the native UI

## Run the server

```sh
./target/release/synapse-server \
  --port 4173 \
  --token CODE \
  --cwd /tmp/synapse-demo
```

On startup it prints the resolved `claude` binary, the working directory, the
pairing token, and the WebSocket URL to connect to. With `--token` omitted a
random 6-char token is generated. On launch it also **attaches to existing
Claude Code sessions** found on disk (` Attached N existing Claude Code
session(s).`).

### Secure remote access (over the internet)

To drive Claude Code from your phone over the internet, run the server with
**TLS** so the connection is encrypted (`wss://`). The fastest path is a
self-signed certificate:

```sh
./target/release/synapse-server   --port 4173   --token CODE   --cwd ~/code/myproject   --tls --tls-self-signed --tls-san mybox,192.168.1.10
```

Then connect the app to `wss://<host>:4173/?token=CODE` and **enable the
"Secure connection (wss/TLS)" toggle** on the pairing screen. With the toggle
on, the app accepts the self-signed certificate (traffic is still encrypted).

For a domain with a real certificate (e.g. from Let's Encrypt), point the
server at your cert/key instead:

```sh
./target/release/synapse-server --port 443 --token CODE   --tls --tls-cert /etc/letsencrypt/live/my.domain/fullchain.pem   --tls-key  /etc/letsencrypt/live/my.domain/privkey.pem
```

### Remote access from anywhere (Cloudflare Tunnel, no setup)

For a **productized** experience — any phone reaching your machine from any
network (4G/5G/Wi-Fi) with zero router/NAT/cert setup and a **real TLS
certificate** — use `--tunnel`:

```sh
./target/release/synapse-server --port 4173 --token CODE   --cwd ~/code/myproject --tunnel
```

The server automatically starts a Cloudflare quick tunnel and prints a public
`https://*.trycloudflare.com` URL with a real certificate. The pairing QR /
link becomes `synapse://<host>.trycloudflare.com:443?token=CODE&tls=1`, which
the app connects to over standard `wss://` — fully encrypted, iOS-compliant,
no domain or port-forwarding needed.

> Requires `cloudflared` on PATH (`brew install cloudflared` on macOS). Quick
> tunnels are free and need no Cloudflare account; for a stable fixed hostname
> in production, create a named tunnel with your own domain. If the tunnel
> can't start, the server falls back to LAN pairing automatically.

### Pair a device by QR code

On startup the server prints a **QR code** in the terminal encoding a pairing
link:

```
synapse://<host>:<port>?token=<TOKEN>&tls=<0|1>
```

The host defaults to this machine's auto-detected LAN IP (override with
`--pair-host`). To bind your phone:

1. On the pairing screen, tap **"Pair with QR / link"**.
2. Scan the terminal QR with any QR app (the camera app, a scanner, etc.) to
   copy the `synapse://…` link.
3. Paste it into the field and tap **Pair** — the app fills in host, port,
   token, and the TLS toggle, then connects automatically.

The manual host/port/token fields are still available for typing connection
details by hand.

> Reachability: if the server is behind NAT with no public IP / domain, put a
> tunnel (e.g. a TLS-capable reverse proxy or a port forward) in front of it,
> or expose it via a tunnel service. The server itself just needs to be
> reachable from the phone on its host:port.

### Server CLI flags

| Flag | Default | Description |
|------|---------|-------------|
| `-p, --port` | `4173` | HTTP/WS port |
| `--host` | `0.0.0.0` | Bind host |
| `--cwd` | current dir | Default working directory for new sessions |
| `--token` | random | Fixed pairing token |
| `--bin` | auto-resolved | Path to the `claude` binary |
| `--tls` | off | Enable TLS (`wss://` / `https://) — use with `--tls-cert`/`--tls-key` or `--tls-self-signed` |
| `--tls-cert` | — | PEM certificate chain (enables TLS with `--tls-key`) |
| `--tls-key` | — | PEM private key matching `--tls-cert` |
| `--tls-self-signed` | off | Generate an in-memory self-signed certificate (TLS quick start) |
| `--tls-san` | localhost | Comma-separated hosts/IPs added to the self-signed cert, e.g. `mybox,192.168.1.10` |
| `--tls-cert-out` | — | Persist the generated self-signed cert (PEM) to this path |
| `--tls-key-out` | — | Persist the generated self-signed key (PEM) to this path |
| `--pair-host` | auto (LAN IP) | Host encoded in the pairing QR / URL (override for a public hostname/IP) |
| `--tunnel` | off | Expose via Cloudflare Tunnel: public `wss://` with a real cert, reachable from anywhere (no NAT/router setup) |
| `--dev` | off | Verbose logging |

## Run the app

```sh
./target/release/synapse-app
```

Enter the server **host**, **port**, and **pairing token**, then **Connect**.
For remote/internet servers enable the **"Secure connection (wss/TLS)"**
toggle so the app uses `wss://`. The drawer lists sessions; tap one to load its
history, then chat. The send button becomes a red ■ **stop** button while a
turn is running.

## WebSocket protocol

Connect to `ws://<host>:<port>/?token=<TOKEN>`. All messages are JSON.

### Client → server (commands)

| `op` | Fields | Behavior |
|------|--------|----------|
| `create` | `opts: { cwd?, name?, model?, permission_mode?, agent? }` | Start a new Claude Code session |
| `send` | `sessionId`, `content` | Send a user message and run a turn |
| `stop` | `sessionId` | Interrupt the running turn |
| `history` | `sessionId`, `limit?` (default 400) | Load transcript events |
| `list` | — | List all sessions |
| `refresh` | — | Re-sync with on-disk Claude Code sessions, then list |

### Server → client (events)

| `type` | Fields | Meaning |
|--------|--------|---------|
| `hello` | `sessions[]` | Sent on connect with the current session list |
| `sessions` | `sessions[]` | Updated session list (after `list`/`refresh`) |
| `created` | `session` | A new session was created |
| `history` | `sessionId`, `events[]`, `found` | Transcript reply |
| `event` | `event` | A streamed Claude event (`assistant`/`user`/`result`/…) |
| `system` | `subtype`, `sessionId` | `turn_started`, `turn_stopped`, `session_created`, `fallback_to_json`, `bridge_error` |
| `error` | `error`, `op?` | Operation failed |

## CI

GitHub Actions (`.github/workflows/ci.yml`) runs on every push / pull request:

- **lint-test** (Ubuntu) — `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test`, `cargo build --release`. Uploads the Linux server binary.
- **ios-lib** (macOS) — compiles the `aarch64-apple-ios` static library
  (`libsynapse_app.a`) that the Xcode project links, and uploads it. Verifies
  the `iphoneos` SDK is present (the `ring` TLS crate needs it for iOS).

All CI steps are verified green locally (`fmt` clean, clippy `-D warnings`
clean, 11 tests pass, desktop build + release build succeed). The iOS static
library compiles under a full Xcode (it needs the `iphoneos` SDK for `ring`;
CommandLineTools alone cannot cross-compile it).

## Mobile packaging (iOS / Android)



The app crate is split into a shared library (`src/lib.rs`: app logic + the
`run_app` entry) and a thin desktop binary (`src/main.rs`). On iOS/Android the
library is compiled into a native artifact that a thin platform shell links.

| Platform | Renderer | Shell | Entry |
|----------|----------|-------|-------|
| iOS (`aarch64-apple-ios`) | femtovg + wgpu | Xcode app (Obj-C delegate) | `synapse_ios_main()` |
| Android (`aarch64-linux-android`) | Skia via android-activity | `cargo-apk` NativeActivity | crate `android` feature |
| Desktop | femtovg + wgpu | the `synapse-app` binary | `main()` -> `run_app()` |

The renderer was switched to **`renderer-femtovg-wgpu`** specifically because
the glutin/OpenGL `renderer-femtovg` feature fails to compile for iOS. TLS was
switched to **`rustls-tls-webpki-roots`** so no system OpenSSL is needed on
mobile.

### iOS

Add the target, then use the helper script (it builds the static library and,
if Xcode is installed, assembles the `.app`):

```sh
rustup target add aarch64-apple-ios
./mobile/build-ios.sh
```

This produces `target/aarch64-apple-ios/release/libsynapse_app.a`, which the
Xcode project at `mobile/ios/Synapse.xcodeproj` links (it has a "Build Rust
staticlib" run-script phase that keeps the `.a` in sync). To run on a device or
simulator, open the project in Xcode:

```sh
open mobile/ios/Synapse.xcodeproj
```

> The Rust -> static library step is verified to compile and link-archive for
> iOS. Assembling and signing the final `.ipa` requires a full Xcode
> installation (`xcode-select` pointing at `Xcode.app`, not just
> `CommandLineTools`).

### Android

Install the NDK (Android Studio -> SDK Manager -> SDK Tools -> NDK) and set
`ANDROID_NDK`, then:

```sh
rustup target add aarch64-linux-android
export ANDROID_NDK="\$HOME/Library/Android/sdk/ndk/<version>"
cargo install cargo-apk
./mobile/build-android.sh
```

The crate's `android` cargo feature enables `slint/backend-android-activity-06`
(Skia renderer + `android-activity` NativeActivity). `crates/app/AndroidManifest.xml`
declares the `NativeActivity` and `INTERNET` permission.

> The Rust + android-activity code path compiles for Android. Final APK
> packaging needs the NDK (to build Skia) and `cargo-apk`.

## Project layout

```
crates/
  server/   axum + WS bridge to `claude -p`
    src/
      main.rs      CLI entry + arg parsing
      http.rs      HTTP routes + WS command/event loop
      manager.rs   session lifecycle, broadcast, stop, attach
      claude.rs    spawns `claude`, parses stream-json, child kill for stop
      history.rs   reads on-disk transcripts into normalized events
  app/       Slint mobile UI
    src/lib.rs     shared app logic (run_app) + iOS entry
    src/main.rs    desktop entry -> run_app()
    ui/app.slint   ChatGPT-style UI (top bar, message list, composer)
    build.rs       compiles app.slint into the binary
mobile/
  ios/         Xcode wrapper (AppDelegate.mm) + project + build-ios.sh
  android/     build-android.sh (cargo-apk / NDK)
web/        Node.js design-reference prototype
```

## Status

Core features are implemented and verified end-to-end: session create/list,
send with streamed tool-call rendering, **stop/interrupt**, history replay, and
attach-to-existing-sessions. Mobile packaging (iOS/Android app bundles) is the
remaining platform-integration step.

### UI fidelity to ChatGPT mobile

- **Code blocks** — fenced (``` ``` ```) blocks in assistant replies are
  extracted and rendered as dark cards with a monospace font and a language
  label bar, exactly like ChatGPT. Unterminated fences during streaming stay as
  text until they close.
- **Auto-scroll** — the message list pins to the newest content as tokens
  stream in and tool cards update.
- **Empty state** — a new session shows a greeting ("How can I help with your
  code?") until the first message.
- **Auto-reconnect** — a dropped WebSocket retries with exponential backoff
  (1s→15s cap) and shows an orange "Reconnecting…" banner, then restores the
  active session's transcript instead of returning to pairing.

### Secure remote access (TLS)

- The server supports `wss://` / `https://` via `--tls` with either a provided
  PEM cert/key pair or a one-shot `--tls-self-signed` certificate (verified
  end-to-end: self-signed handshake → create → send → streamed reply).
- The app has a **"Secure connection (wss/TLS)"** pairing toggle that switches
  to `wss://` and accepts self-signed personal certificates, so you can drive
  Claude Code from your phone over the internet with an encrypted link.

### Pair by QR code

- The server prints a scannable QR (and a `synapse://host:port?token&tls` link)
  on startup, auto-detecting the LAN IP for the QR host.
- The app has a **"Pair with QR / link"** flow that parses the link, fills the
  pairing fields, and connects in one step (parser covered by unit tests).

### Attach-to-existing sessions

Session discovery parses `claude agents --json` (camelCase fields) so the full
session id and working directory are captured correctly. This fixes transcript
backfill: attached sessions now load their full on-disk history (verified with
real Claude Code sessions).
