//! Synapse app library: shared app logic plus the iOS entry point.
//! The desktop binary (`src/main.rs`) calls [`run_app`].

slint::include_modules!();

use futures_util::{SinkExt, StreamExt};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

/// Shared WebSocket sender half. Wrapped in Arc<Mutex<Option<…>>> so UI
/// callbacks can push commands and the connection can be swapped on reconnect.
type WsSink = Arc<
    Mutex<
        Option<
            futures_util::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
                Message,
            >,
        >,
    >,
>;

fn model_rc<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

pub async fn run_app() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    // Ensure a rustls crypto provider is installed before any TLS handshake.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app = App::new()?;

    // shared WS sender so UI callbacks can push commands
    let ws_tx: WsSink = Arc::new(Mutex::new(None));

    // --- pair via QR / link ---
    // Parses a synapse://host:port?token=T&tls=N link, fills the pairing
    // fields, and connects — the path taken after scanning the server's QR.
    {
        let weak = app.as_weak();
        let ws_tx = ws_tx.clone();
        app.on_pairClicked(move |link| {
            let weak = weak.clone();
            let ws_tx = ws_tx.clone();
            slint::spawn_local(async move {
                let parsed = match parse_pair_link(link.as_str()) {
                    Some(p) => p,
                    None => {
                        if let Some(app) = weak.upgrade() {
                            app.set_pairingError(
                                "Invalid pairing link. Use the QR from the server.".into(),
                            );
                            app.set_pairLinkText("".into());
                        }
                        return;
                    }
                };
                if let Some(app) = weak.upgrade() {
                    app.set_pairingHost(parsed.host.clone().into());
                    app.set_pairingPort(parsed.port.clone().into());
                    app.set_pairingToken(parsed.token.clone().into());
                    app.set_pairingTls(parsed.tls);
                    app.set_pairingError("".into());
                    app.set_pairLinkText("".into());
                    app.set_showPairSheet(false);
                }
                // Now connect with the freshly filled credentials.
                if let Some(app) = weak.upgrade() {
                    app.set_connecting(true);
                }
                match connect(
                    &parsed.host,
                    &parsed.port,
                    &parsed.token,
                    parsed.tls,
                    weak.clone(),
                    ws_tx.clone(),
                )
                .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        if let Some(app) = weak.upgrade() {
                            app.set_connecting(false);
                            app.set_pairingError(format!("Could not connect: {e}").into());
                        }
                    }
                }
            })
            .unwrap();
        });
    }

    // --- connect ---
    {
        let weak = app.as_weak();
        let ws_tx = ws_tx.clone();
        app.on_connectClicked(move || {
            let weak = weak.clone();
            let ws_tx = ws_tx.clone();
            let host = weak.unwrap().get_pairingHost().to_string();
            let port = weak.unwrap().get_pairingPort().to_string();
            let token = weak.unwrap().get_pairingToken().to_string();
            let tls = weak.unwrap().get_pairingTls();
            weak.unwrap().set_connecting(true);
            slint::spawn_local(async move {
                match connect(&host, &port, &token, tls, weak.clone(), ws_tx.clone()).await {
                    Ok(_) => {}
                    Err(e) => {
                        if let Some(app) = weak.upgrade() {
                            app.set_connecting(false);
                            app.set_pairingError(format!("Could not connect: {e}").into());
                        }
                    }
                }
            })
            .unwrap();
        });
    }

    {
        let weak = app.as_weak();
        app.on_toggleTool(move |idx| {
            let app = weak.unwrap();
            let mut msgs: Vec<MsgBlock> = app.get_messages().iter().collect();
            if let Some(m) = msgs.get_mut(idx as usize) {
                m.expanded = !m.expanded;
            }
            app.set_messages(model_rc(msgs));
        });
    }

    // --- send message ---
    {
        let weak = app.as_weak();
        let ws_tx = ws_tx.clone();
        app.on_sendClicked(move |text| {
            let app = weak.unwrap();
            let sid = app.get_activeSessionId().to_string();
            if sid.is_empty() {
                return;
            }
            // optimistic local render
            let mut v: Vec<MsgBlock> = app.get_messages().iter().collect();
            v.push(MsgBlock {
                kind: "text".into(),
                role: "user".into(),
                text: text.clone(),
                toolName: "".into(),
                toolStatus: "".into(),
                expanded: false,
                toolId: "".into(),
                codeLang: "".into(),
            });
            app.set_messages(model_rc(v));
            app.set_composerText("".into());
            app.set_busy(true);
            let ws_tx = ws_tx.clone();
            let msg =
                serde_json::json!({ "op": "send", "sessionId": sid, "content": text.as_str() });
            slint::spawn_local(async move {
                if let Some(tx) = ws_tx.lock().await.as_mut() {
                    let _ = tx.send(Message::Text(msg.to_string())).await;
                }
            })
            .unwrap();
        });
    }

    // --- stop current turn ---
    {
        let weak = app.as_weak();
        let ws_tx = ws_tx.clone();
        app.on_stopClicked(move || {
            let sid = weak.unwrap().get_activeSessionId().to_string();
            if sid.is_empty() {
                return;
            }
            let msg = serde_json::json!({ "op": "stop", "sessionId": sid });
            let ws_tx = ws_tx.clone();
            slint::spawn_local(async move {
                if let Some(tx) = ws_tx.lock().await.as_mut() {
                    let _ = tx.send(Message::Text(msg.to_string())).await;
                }
            })
            .unwrap();
        });
    }

    // --- new session ---
    {
        let weak = app.as_weak();
        let ws_tx = ws_tx.clone();
        app.on_newSessionClicked(move || {
            let ws_tx = ws_tx.clone();
            let weak = weak.clone();
            let msg = serde_json::json!({ "op": "create", "opts": { "name": "New session" } });
            slint::spawn_local(async move {
                if let Some(tx) = ws_tx.lock().await.as_mut() {
                    let _ = tx.send(Message::Text(msg.to_string())).await;
                }
                if let Some(app) = weak.upgrade() {
                    app.set_drawerOpen(false);
                }
            })
            .unwrap();
        });
    }

    // --- drawer / select / refresh ---
    {
        let weak = app.as_weak();
        app.on_toggleDrawer(move || {
            let app = weak.unwrap();
            app.set_drawerOpen(!app.get_drawerOpen());
        });
    }
    {
        let weak = app.as_weak();
        let ws_tx = ws_tx.clone();
        app.on_selectSession(move |sid| {
            {
                let app = weak.unwrap();
                app.set_activeSessionId(sid.clone());
                app.set_drawerOpen(false);
                // clear while we request the backfilled transcript
                app.set_messages(ModelRc::new(VecModel::default()));
            }
            let ws_tx = ws_tx.clone();
            let msg =
                serde_json::json!({ "op": "history", "sessionId": sid.to_string(), "limit": 400 });
            slint::spawn_local(async move {
                if let Some(tx) = ws_tx.lock().await.as_mut() {
                    let _ = tx.send(Message::Text(msg.to_string())).await;
                }
            })
            .unwrap();
        });
    }
    {
        let ws_tx = ws_tx.clone();
        app.on_refreshClicked(move || {
            let ws_tx = ws_tx.clone();
            let msg = serde_json::json!({ "op": "refresh" });
            slint::spawn_local(async move {
                if let Some(tx) = ws_tx.lock().await.as_mut() {
                    let _ = tx.send(Message::Text(msg.to_string())).await;
                }
            })
            .unwrap();
        });
    }

    // --- debug auto-connect (skips pairing screen for testing) ---
    // SYNAPSE_AUTO_CONNECT=1 SYNAPSE_HOST=127.0.0.1 SYNAPSE_PORT=4173 SYNAPSE_TOKEN=demo SYNAPSE_SESSION=demo-001 ./target/debug/synapse-app
    if let Ok(host) = std::env::var("SYNAPSE_HOST").or_else(|_| std::env::var("SYNAPSE_AUTO_HOST"))
    {
        let port = std::env::var("SYNAPSE_PORT").unwrap_or_else(|_| "4173".to_string());
        let token = std::env::var("SYNAPSE_TOKEN").unwrap_or_else(|_| "demo".to_string());
        let tls = std::env::var("SYNAPSE_TLS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let auto_session = std::env::var("SYNAPSE_SESSION").ok();
        let weak = app.as_weak();
        let ws_tx2 = ws_tx.clone();
        app.set_connecting(true);
        slint::spawn_local(async move {
            match connect(&host, &port, &token, tls, weak.clone(), ws_tx2.clone()).await {
                Ok(_) => {
                    if let Some(sid) = auto_session {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        if let Some(app) = weak.upgrade() {
                            app.set_activeSessionId(sid.clone().into());
                            app.set_drawerOpen(false);
                            app.set_messages(ModelRc::new(VecModel::default()));
                            let msg = serde_json::json!({ "op": "history", "sessionId": sid, "limit": 400 });
                            if let Some(tx) = ws_tx2.lock().await.as_mut() {
                                let _ = tx.send(Message::Text(msg.to_string())).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    if let Some(app) = weak.upgrade() {
                        app.set_connecting(false);
                        app.set_pairingError(format!("Auto-connect failed: {e}").into());
                    }
                }
            }
        })
        .unwrap();
    }

    app.run()?;
    Ok(())
}

/// A parsed `synapse://host:port?token=T&tls=N` pairing link.
struct ParsedPair {
    host: String,
    port: String,
    token: String,
    tls: bool,
}

/// Parse a pairing link emitted by the server's QR code. Accepts
/// `synapse://host:port?token=...&tls=0|1` (also tolerates a bare
/// `host:port?token=...` and a `wss?://` scheme for convenience).
fn parse_pair_link(link: &str) -> Option<ParsedPair> {
    let raw = link.trim();
    if raw.is_empty() {
        return None;
    }
    // Accept the synapse:// scheme or a bare authority.
    let body = raw
        .strip_prefix("synapse://")
        .or_else(|| raw.strip_prefix("synapse:"))
        .unwrap_or(raw);
    // Split authority from query.
    let (authority, query) = body.split_once('?').unwrap_or((body, ""));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => (h.to_string(), p.to_string()),
        _ => return None, // a port is required
    };
    if host.is_empty() {
        return None;
    }
    // Parse query params token / tls.
    let mut token = String::new();
    let mut tls = false;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "token" => token = v.to_string(),
                "tls" => tls = v == "1" || v.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
    }
    if token.is_empty() {
        return None;
    }
    Some(ParsedPair {
        host,
        port,
        token,
        tls,
    })
}

/// Open a TLS WebSocket stream that trusts both the standard WebPKI roots
/// and any self-signed certificate. The self-signed acceptance makes personal
/// remote deployments (one-off self-signed certs) usable from the phone without
/// a CA bundle; traffic is still TLS-encrypted.
async fn make_tls_stream(
    url: &str,
) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let request = url.into_client_request()?;

    // ClientConfig that accepts any server certificate. Combined with TLS the
    // link is still encrypted; the permissive verifier trades PKI validation
    // for usability with self-signed personal certs over the internet.
    let config = rustls::client::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    let connector = tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(config));

    let (stream, _resp) =
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
            .await?;
    Ok(stream)
}

/// A `ServerCertVerifier` that accepts every certificate. Only used when the
/// user explicitly enables the TLS toggle for a self-signed personal server.
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

async fn connect(
    host: &str,
    port: &str,
    token: &str,
    tls: bool,
    weak: Weak<App>,
    ws_tx: WsSink,
) -> anyhow::Result<()> {
    let scheme = if tls { "wss" } else { "ws" };
    let url = format!("{scheme}://{host}:{port}/?token={token}");

    // First (manual) connection attempt.
    connect_and_pump(&url, tls, &weak, &ws_tx, true).await?;

    // Auto-reconnect loop with capped exponential backoff. We reuse the last
    // known credentials so transient drops (wifi handoff, app backgrounding)
    // heal transparently instead of dumping the user back to pairing.
    let mut backoff = std::time::Duration::from_secs(1);
    loop {
        if weak.upgrade().is_none() {
            break;
        }
        if let Some(app) = weak.upgrade() {
            app.set_reconnecting(true);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_secs(15));

        match connect_and_pump(&url, tls, &weak, &ws_tx, false).await {
            Ok(()) => {
                backoff = std::time::Duration::from_secs(1);
                if let Some(app) = weak.upgrade() {
                    app.set_reconnecting(false);
                }
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

/// Establish one WS connection and pump events until it drops. On the initial
/// connection (`first`) it transitions the UI into the chat view; on reconnects
/// it restores the active session's transcript so the conversation resumes.
async fn connect_and_pump(
    url: &str,
    tls: bool,
    weak: &Weak<App>,
    ws_tx: &WsSink,
    first: bool,
) -> anyhow::Result<()> {
    let stream = if tls {
        make_tls_stream(url).await?
    } else {
        tokio_tungstenite::connect_async(url).await?.0
    };
    let (tx, mut rx) = stream.split();
    *ws_tx.lock().await = Some(tx);

    if first {
        if let Some(app) = weak.upgrade() {
            app.set_connecting(false);
            app.set_view("chat".into());
            app.set_drawerOpen(true);
        }
    } else {
        // On reconnect, re-request the active session's history so the
        // transcript is refreshed after the drop.
        if let Some(app) = weak.upgrade() {
            let sid = app.get_activeSessionId().to_string();
            if !sid.is_empty() {
                let msg = serde_json::json!({ "op": "history", "sessionId": sid, "limit": 400 });
                if let Some(t) = ws_tx.lock().await.as_mut() {
                    let _ = t.send(Message::Text(msg.to_string())).await;
                }
            }
        }
    }

    while let Some(Ok(msg)) = rx.next().await {
        let text = match msg {
            Message::Text(t) => t,
            _ => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(app) = weak.upgrade() {
            handle_event(&app, v);
        }
    }
    // stream ended -> caller decides whether to reconnect
    Ok(())
}

fn handle_event(app: &App, msg: serde_json::Value) {
    let ty = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match ty {
        "hello" => {
            let sessions = parse_sessions(msg.get("sessions"));
            app.set_sessions(model_rc(sessions));
        }
        "sessions" => {
            let sessions = parse_sessions(msg.get("sessions"));
            app.set_sessions(model_rc(sessions));
        }
        "created" => {
            if let Some(s) = msg.get("session") {
                let id = s
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = s
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Session")
                    .to_string();
                let model = s
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                app.set_activeSessionId(id.into());
                app.set_activeSessionName(name.into());
                app.set_activeSessionSub(model.into());
                app.set_messages(ModelRc::new(VecModel::default()));
            }
        }
        "history" => {
            // backfilled transcript for the active session; rebuild message list
            let active = app.get_activeSessionId().to_string();
            let sid = msg
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if sid == active {
                if let Some(events) = msg.get("events").and_then(|v| v.as_array()) {
                    let mut msgs: Vec<MsgBlock> = Vec::new();
                    for evt in events {
                        ingest_event_into(&mut msgs, evt);
                    }
                    normalize_code_blocks(&mut msgs);
                    app.set_messages(model_rc(msgs));
                }
            }
        }
        "event" => {
            if let Some(evt) = msg.get("event") {
                ingest_event(app, evt);
            }
        }
        _ => {}
    }
}

fn parse_sessions(v: Option<&serde_json::Value>) -> Vec<SessionInfo> {
    let arr = match v.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .map(|s| SessionInfo {
            id: s.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
            name: s
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Session")
                .into(),
            cwd: s.get("cwd").and_then(|v| v.as_str()).unwrap_or("").into(),
            model: s.get("model").and_then(|v| v.as_str()).unwrap_or("").into(),
            state: s
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("idle")
                .into(),
            attached: s.get("attached").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .collect()
}

/// Push the conversational content of one stream-json event into `msgs`.
/// Returns a TurnState transition (Some(true)=busy, Some(false)=idle) for the
/// caller to apply, since backfill should not flip the busy indicator.
fn ingest_event_into(msgs: &mut Vec<MsgBlock>, evt: &serde_json::Value) -> Option<bool> {
    let ty = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let mut state: Option<bool> = None;
    match ty {
        "assistant" => {
            if let Some(content) = evt
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for block in content {
                    let bt = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match bt {
                        "text" => {
                            let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            // Coalesce into the last assistant *text* segment of
                            // the current answer. Code blocks (already split out
                            // by normalize_code_blocks) are skipped so prose
                            // fragments keep accumulating on the trailing prose
                            // block; a subsequent normalize re-fences them.
                            let appended = if let Some(idx) = msgs
                                .iter()
                                .rposition(|m| m.role == "assistant" && m.kind == "text")
                            {
                                // Only coalesce if no tool block separates this
                                // text block from the end (i.e. it belongs to the
                                // same assistant turn).
                                let separated = msgs[idx + 1..]
                                    .iter()
                                    .any(|m| m.kind == "tool" || m.role == "user");
                                if separated {
                                    false
                                } else {
                                    let combined = format!("{}{}", msgs[idx].text, text);
                                    msgs[idx].text = combined.into();
                                    true
                                }
                            } else {
                                false
                            };
                            if !appended {
                                push_text(msgs, "assistant", text);
                            }
                        }
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let arg_preview = tool_arg_preview(&name, block.get("input"));
                            // upsert by tool id
                            if let Some(existing) = msgs
                                .iter_mut()
                                .find(|m| m.kind == "tool" && m.toolId == id.as_str())
                            {
                                existing.toolName = name.into();
                                existing.toolStatus = "running".into();
                                existing.text = arg_preview.into();
                            } else {
                                msgs.push(MsgBlock {
                                    kind: "tool".into(),
                                    role: "assistant".into(),
                                    text: arg_preview.into(),
                                    toolName: name.into(),
                                    toolStatus: "running".into(),
                                    expanded: false,
                                    toolId: id.into(),
                                    codeLang: "".into(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            if let Some(content) = evt
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                let mut text_parts: Vec<String> = Vec::new();
                for block in content {
                    let bt = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match bt {
                        "tool_result" => {
                            let id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let is_err = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let result_text = block
                                .get("content")
                                .map(|c| {
                                    if let Some(s) = c.as_str() {
                                        s.to_string()
                                    } else {
                                        c.to_string()
                                    }
                                })
                                .unwrap_or_default();
                            let result_preview = truncate(&result_text, 4000);
                            if let Some(existing) = msgs
                                .iter_mut()
                                .find(|m| m.kind == "tool" && m.toolId == id.as_str())
                            {
                                existing.toolStatus = if is_err { "error" } else { "done" }.into();
                                existing.text = SharedString::from(format!(
                                    "{}\n\n{}",
                                    existing.text, result_preview
                                ));
                            }
                        }
                        "text" => {
                            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    text_parts.push(t.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !text_parts.is_empty() {
                    let text = text_parts.join("\n");
                    let is_dup = msgs
                        .last()
                        .map(|m| m.role == "user" && m.text == text)
                        .unwrap_or(false);
                    if !is_dup {
                        push_text(msgs, "user", &text);
                    }
                }
            } else if let Some(content) = evt
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                if !content.is_empty() {
                    let is_dup = msgs
                        .last()
                        .map(|m| m.role == "user" && m.text == content)
                        .unwrap_or(false);
                    if !is_dup {
                        push_text(msgs, "user", content);
                    }
                }
            }
        }
        "result" => {
            state = Some(false);
        }
        "system" => {
            let sub = evt.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            match sub {
                "turn_started" => state = Some(true),
                "turn_stopped" => state = Some(false),
                _ => {}
            }
        }
        _ => {}
    }
    state
}

fn push_text(msgs: &mut Vec<MsgBlock>, role: &str, text: &str) {
    msgs.push(MsgBlock {
        kind: "text".into(),
        role: role.into(),
        text: text.into(),
        toolName: "".into(),
        toolStatus: "".into(),
        expanded: false,
        toolId: "".into(),
        codeLang: "".into(),
    });
}

/// Split assistant markdown into (kind, text, lang) segments, where `kind` is
/// either "text" or "code" and `lang` is the fence language tag (empty for
/// prose). Only fenced code blocks (``` ```) are extracted; inline formatting
/// is left as plain text because Slint dynamic text cannot style it.
fn split_markdown(md: &str) -> Vec<(&'static str, String, String)> {
    let mut out: Vec<(&'static str, String, String)> = Vec::new();
    let mut lines = md.split('\n').peekable();
    let mut prose = String::new();
    // flush accumulated prose as a single text segment
    let flush_prose = |prose: &mut String, out: &mut Vec<(&'static str, String, String)>| {
        if !prose.is_empty() {
            out.push(("text", std::mem::take(prose), String::new()));
        }
    };
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush_prose(&mut prose, &mut out);
            let lang = trimmed.strip_prefix("```").unwrap_or("").trim().to_string();
            let mut code: Vec<String> = Vec::new();
            let mut closed = false;
            for cline in lines.by_ref() {
                if cline.trim_start().starts_with("```") {
                    closed = true;
                    break;
                }
                code.push(cline.to_string());
            }
            // A code block is only emitted once the closing fence is seen.
            // While streaming, an unclosed fence stays in prose so the user
            // still sees partial output.
            if closed {
                out.push(("code", code.join("\n"), lang));
            } else {
                // unterminated fence during streaming: show raw so far
                let raw = format!("```{}\n{}", lang, code.join("\n"));
                out.push(("text", raw, String::new()));
            }
        } else {
            if !prose.is_empty() {
                prose.push('\n');
            }
            prose.push_str(line);
        }
    }
    flush_prose(&mut prose, &mut out);
    out
}

/// Re-expand any assistant text blocks into text/code segments based on their
/// current accumulated markdown. This runs after each event is ingested (and
/// after history backfill) so fenced code blocks render as ChatGPT-style dark
/// cards even though the content arrived as streaming text fragments. Tool
/// blocks and user blocks are left untouched.
fn normalize_code_blocks(msgs: &mut Vec<MsgBlock>) {
    let mut out: Vec<MsgBlock> = Vec::with_capacity(msgs.len());
    for m in msgs.iter() {
        if m.role == "assistant" && m.kind == "text" {
            for seg in split_markdown(m.text.as_str()) {
                if seg.1.is_empty() {
                    continue;
                }
                out.push(MsgBlock {
                    kind: seg.0.into(),
                    role: "assistant".into(),
                    text: seg.1.into(),
                    toolName: "".into(),
                    toolStatus: "".into(),
                    expanded: false,
                    toolId: "".into(),
                    codeLang: seg.2.into(),
                });
            }
        } else {
            out.push(m.clone());
        }
    }
    *msgs = out;
}

/// Build a short human-readable preview of a tool call input, mirroring the
/// Codex mobile / web prototype behavior (command, file_path, pattern, ...).
fn tool_arg_preview(name: &str, input: Option<&serde_json::Value>) -> String {
    let input = match input {
        Some(v) => v,
        None => return String::new(),
    };
    let low = name.to_ascii_lowercase();
    if low == "bash" {
        return input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    for key in &["file_path", "pattern", "path", "command", "query"] {
        if let Some(s) = input.get(*key).and_then(|v| v.as_str()) {
            return short_path(s);
        }
    }
    String::new()
}

fn short_path(p: &str) -> String {
    if p.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = p.split('/').collect();
    let len = parts.len();
    let start = len.saturating_sub(2);
    parts[start..].join("/")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    format!("{}\n…({} chars)", &s[..n], s.len())
}

fn ingest_event(app: &App, evt: &serde_json::Value) {
    let sid = evt.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
    if !sid.is_empty() && sid != app.get_activeSessionId().as_str() {
        return; // ignore events for other sessions in this simple view
    }
    let mut msgs: Vec<MsgBlock> = app.get_messages().iter().collect();
    match ingest_event_into(&mut msgs, evt) {
        Some(true) => {
            app.set_busy(true);
            app.set_activeState("busy".into());
        }
        Some(false) => app.set_busy(false),
        None => {}
    }
    normalize_code_blocks(&mut msgs);
    app.set_messages(model_rc(msgs));
}

// iOS entry point, called from the Obj-C UIApplicationDelegate
// (mobile/ios/Sources/AppDelegate.mm) once UIKit is ready. We block on a
// multi-thread runtime so tokio tasks (WS client) keep running while
// Slint's winit event loop is pumped by UIKit's main run loop.
#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn synapse_ios_main() {
    let _ = env_logger::try_init();
    let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
    rt.block_on(async {
        let _ = run_app().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_pure_prose() {
        let segs = split_markdown("hello world");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].0, "text");
        assert_eq!(segs[0].1, "hello world");
        assert!(segs[0].2.is_empty());
    }

    #[test]
    fn split_one_code_block() {
        let md = "before\n```rust\nfn main() {}\n```\nafter";
        let segs = split_markdown(md);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].0, "text");
        assert_eq!(segs[0].1, "before");
        assert_eq!(segs[1].0, "code");
        assert_eq!(segs[1].1, "fn main() {}");
        assert_eq!(segs[1].2, "rust");
        assert_eq!(segs[2].0, "text");
        assert_eq!(segs[2].1, "after");
    }

    #[test]
    fn split_unterminated_fence_keeps_raw() {
        // While streaming, an unclosed fence must not be dropped.
        let md = "intro\n```py\nprint(1)";
        let segs = split_markdown(md);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].0, "text");
        assert_eq!(segs[1].0, "text"); // unterminated -> stays as text
        assert!(segs[1].1.contains("print(1)"));
    }

    #[test]
    fn normalize_extracts_code_from_assistant_text() {
        let mut msgs = vec![MsgBlock {
            kind: "text".into(),
            role: "assistant".into(),
            text: "here is code:\n```sh\necho hi\n```\ndone".into(),
            toolName: "".into(),
            toolStatus: "".into(),
            expanded: false,
            toolId: "".into(),
            codeLang: "".into(),
        }];
        normalize_code_blocks(&mut msgs);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].kind, "text");
        assert_eq!(msgs[1].kind, "code");
        assert_eq!(msgs[1].codeLang, "sh");
        assert_eq!(msgs[1].text, "echo hi");
        assert_eq!(msgs[2].kind, "text");
        assert_eq!(msgs[2].text, "done");
    }

    #[test]
    fn normalize_leaves_tool_blocks_untouched() {
        let mut msgs = vec![
            MsgBlock {
                kind: "tool".into(),
                role: "assistant".into(),
                text: "ls".into(),
                toolName: "Bash".into(),
                toolStatus: "running".into(),
                expanded: false,
                toolId: "t1".into(),
                codeLang: "".into(),
            },
            MsgBlock {
                kind: "text".into(),
                role: "user".into(),
                text: "```not code```".into(),
                toolName: "".into(),
                toolStatus: "".into(),
                expanded: false,
                toolId: "".into(),
                codeLang: "".into(),
            },
        ];
        normalize_code_blocks(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].kind, "tool");
        assert_eq!(msgs[1].kind, "text"); // user text never split
    }
}

#[cfg(test)]
mod pair_tests {
    use super::parse_pair_link;

    #[test]
    fn parse_full_synapse_link() {
        let p = parse_pair_link("synapse://192.168.1.6:4173?token=CODE&tls=1").unwrap();
        assert_eq!(p.host, "192.168.1.6");
        assert_eq!(p.port, "4173");
        assert_eq!(p.token, "CODE");
        assert!(p.tls);
    }

    #[test]
    fn parse_no_tls_defaults_false() {
        let p = parse_pair_link("synapse://example.com:443?token=abc").unwrap();
        assert_eq!(p.host, "example.com");
        assert_eq!(p.port, "443");
        assert_eq!(p.token, "abc");
        assert!(!p.tls);
    }

    #[test]
    fn parse_tls_zero_is_false() {
        let p = parse_pair_link("synapse://h:1?token=t&tls=0").unwrap();
        assert!(!p.tls);
    }

    #[test]
    fn parse_rejects_missing_token() {
        assert!(parse_pair_link("synapse://h:1").is_none());
        assert!(parse_pair_link("synapse://h:1?tls=1").is_none());
    }

    #[test]
    fn parse_rejects_missing_port() {
        assert!(parse_pair_link("synapse://host?token=t").is_none());
    }

    #[test]
    fn parse_tolerates_whitespace_and_no_scheme() {
        let p = parse_pair_link("  10.0.0.5:4173?token=XYZ&tls=true  ").unwrap();
        assert_eq!(p.host, "10.0.0.5");
        assert_eq!(p.token, "XYZ");
        assert!(p.tls);
    }
}
