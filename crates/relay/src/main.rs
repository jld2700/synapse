// Synapse relay — a public WebSocket bridge for internet-wide remote access to
// a self-hosted Synapse server.
//
//   mobile app  --wss-->  relay  <--wss--  synapse-server (outbound only)
//
// The relay is a transparent forwarder: it never touches the claude CLI and
// never interprets the app/server JSON frames. It only authenticates
// (deviceId + token) and shuttles frames both ways, so the existing app/server
// WS protocol is completely unchanged.
//
// Roles:
//   * Uplink   GET /uplink?deviceId=<ID>&token=<T>   (server -> relay)
//   * Downlink GET /connect?deviceId=<ID>&token=<T>  (app -> relay)

mod registry;

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use registry::Registry;
use serde::Deserialize;
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(
    name = "synapse-relay",
    version,
    about = "Public WebSocket relay for Synapse remote access"
)]
struct Args {
    #[arg(short, long, default_value = "443")]
    port: u16,
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// PEM certificate chain for TLS (wss). Required for public use.
    #[arg(long)]
    tls_cert: Option<std::path::PathBuf>,
    /// PEM private key matching --tls-cert.
    #[arg(long)]
    tls_key: Option<std::path::PathBuf>,
    /// Optional relay-wide API token the uplink must present.
    #[arg(long)]
    api_token: Option<String>,
    #[arg(long)]
    dev: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<Registry>,
    pub api_token: Option<String>,
}

#[derive(Deserialize)]
struct DeviceQ {
    device_id: Option<String>,
    token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "synapse_relay=info".into()),
        )
        .init();

    let args = Args::parse();
    let state = AppState {
        registry: Arc::new(Registry::new()),
        api_token: args.api_token.clone(),
    };

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/uplink", get(uplink))
        .route("/connect", get(connect))
        .with_state(state.clone());

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let scheme = if args.tls_cert.is_some() { "wss" } else { "ws" };

    println!("\n  Synapse relay is running.\n");
    println!("  Listen:        {scheme}://{addr}");
    println!("  Server uplink: {scheme}://{addr}/uplink?deviceId=<ID>&token=<TOKEN>");
    println!("  App connect:   {scheme}://{addr}/connect?deviceId=<ID>&token=<TOKEN>\n");

    if let (Some(cert), Some(key)) = (args.tls_cert.as_ref(), args.tls_key.as_ref()) {
        let cfg = load_tls(cert, key).await?;
        axum_server::bind_rustls(addr, cfg)
            .serve(app.into_make_service())
            .await?;
    } else {
        tracing::warn!(
            "TLS disabled — local testing only. Public deployments need --tls-cert/--tls-key."
        );
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app.into_make_service()).await?;
    }
    Ok(())
}

async fn load_tls(
    cert: &std::path::Path,
    key: &std::path::Path,
) -> Result<axum_server::tls_rustls::RustlsConfig> {
    let cert_pem = std::fs::read(cert).context("read --tls-cert")?;
    let key_pem = std::fs::read(key).context("read --tls-key")?;
    Ok(axum_server::tls_rustls::RustlsConfig::from_pem(cert_pem, key_pem).await?)
}

async fn health(State(s): State<AppState>) -> impl IntoResponse {
    axum::Json(json!({ "ok": true, "devices": s.registry.device_count().await }))
}

fn check_api(state: &AppState, q: &DeviceQ) -> bool {
    match &state.api_token {
        Some(t) => q.token.as_deref() == Some(t.as_str()),
        None => true,
    }
}

async fn uplink(
    State(s): State<AppState>,
    Query(q): Query<DeviceQ>,
    ws: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    let device_id = match q.device_id.clone() {
        Some(d) if !d.is_empty() => d,
        _ => return axum::http::StatusCode::BAD_REQUEST.into_response(),
    };
    if !check_api(&s, &q) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| uplink_loop(s, device_id, q.token, socket))
}

/// Server uplink. Frames the server sends us are forwarded to the currently
/// linked app; frames the app sends arrive via `server_tx`.
async fn uplink_loop(state: AppState, device_id: String, token: Option<String>, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (server_tx, mut server_rx) = mpsc::channel::<Message>(128);

    let app_slot = state
        .registry
        .register(&device_id, token, server_tx.clone())
        .await;
    tracing::info!(%device_id, "uplink registered");

    // App -> server: drain `server_rx`, write to the server socket.
    let to_server = tokio::spawn(async move {
        while let Some(m) = server_rx.recv().await {
            if ws_tx.send(m).await.is_err() {
                break;
            }
        }
    });

    // Server -> app: read server-originated frames, deliver to the linked app.
    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Ping(p) => {
                let _ = server_tx.send(Message::Pong(p)).await;
            }
            Message::Close(_) => break,
            other => {
                if let Some(app_tx) = app_slot.lock().await.as_ref() {
                    let _ = app_tx.send(other).await;
                }
            }
        }
    }

    state.registry.unregister(&device_id).await;
    tracing::info!(%device_id, "uplink gone");
    to_server.abort();
}

async fn connect(
    State(s): State<AppState>,
    Query(q): Query<DeviceQ>,
    ws: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    let device_id = match q.device_id.clone() {
        Some(d) if !d.is_empty() => d,
        _ => return axum::http::StatusCode::BAD_REQUEST.into_response(),
    };
    if !check_api(&s, &q) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| connect_loop(s, device_id, q.token, socket))
}

/// App downlink. App frames go to the server via `server_tx`; server frames
/// arrive via `to_app` (linked into the device's app slot).
async fn connect_loop(
    state: AppState,
    device_id: String,
    token: Option<String>,
    socket: WebSocket,
) {
    let (server_tx, app_slot) = match state.registry.acquire(&device_id, token.as_deref()).await {
        Some(v) => v,
        None => {
            let (mut tx, _rx) = socket.split();
            let _ = tx
                .send(Message::Text(
                    json!({"type":"error","error":"device offline or invalid token"}).to_string(),
                ))
                .await;
            return;
        }
    };

    let (mut app_tx, mut app_rx) = socket.split();
    let (to_app_tx, mut to_app_rx) = mpsc::channel::<Message>(128);

    // Link this app into the device's app slot.
    {
        let mut slot = app_slot.lock().await;
        *slot = Some(to_app_tx.clone());
    }
    tracing::info!(%device_id, "app linked");

    // server -> app
    let s1 = tokio::spawn(async move {
        while let Some(m) = to_app_rx.recv().await {
            if app_tx.send(m).await.is_err() {
                break;
            }
        }
    });

    // app -> server
    let slot = app_slot.clone();
    let s2 = tokio::spawn(async move {
        while let Some(Ok(msg)) = app_rx.next().await {
            if let Message::Close(_) = msg {
                break;
            }
            if server_tx.send(msg).await.is_err() {
                break;
            }
        }
        // unlink
        let mut s = slot.lock().await;
        *s = None;
    });

    let _ = (s1.await, s2.await);
    tracing::info!(%device_id, "app unlinked");
}
