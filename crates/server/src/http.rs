// HTTP + WebSocket server. Mirrors the Node prototype protocol so the same
// mobile client contract applies:
//   WS  /?token=<CODE>            -> bidirectional event stream + commands
//   GET /api/health               -> { ok, sessions }
//   GET /api/sessions?token=CODE  -> list
//   GET /api/pair                 -> { ok }
// Commands over WS: {op:"create"|"send"|"refresh"|"list", ...}

use crate::manager::{CreateOpts, SessionManager};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use rand::seq::SliceRandom;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub manager: Arc<SessionManager>,
    pub token: String,
}

fn gen_token() -> String {
    let alphabet: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| *alphabet.choose(&mut rng).unwrap() as char)
        .collect()
}

pub fn router(manager: Arc<SessionManager>, token: Option<String>) -> (Router, String) {
    let token = token.unwrap_or_else(gen_token);
    let state = AppState {
        manager,
        token: token.clone(),
    };
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/pair", get(pair))
        .route("/api/sessions", get(sessions))
        .route("/", get(ws_handler))
        .with_state(state);
    (app, token)
}

async fn health(State(s): State<AppState>) -> impl IntoResponse {
    let n = s.manager.list().await.len();
    axum::Json(json!({ "ok": true, "sessions": n }))
}

#[derive(Deserialize)]
struct TokenQ {
    token: Option<String>,
}

async fn pair(State(s): State<AppState>, Query(q): Query<TokenQ>) -> impl IntoResponse {
    if q.token.as_deref() != Some(&s.token) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error":"unauthorized"})),
        )
            .into_response();
    }
    axum::Json(json!({ "ok": true })).into_response()
}

async fn sessions(State(s): State<AppState>, Query(q): Query<TokenQ>) -> impl IntoResponse {
    if q.token.as_deref() != Some(&s.token) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error":"unauthorized"})),
        )
            .into_response();
    }
    let list = s.manager.list().await;
    axum::Json(json!({ "sessions": list })).into_response()
}

async fn ws_handler(
    State(s): State<AppState>,
    Query(q): Query<TokenQ>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if q.token.as_deref() != Some(&s.token) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| client_loop(s, socket))
}

async fn client_loop(state: AppState, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // send hello
    let sessions = state.manager.list().await;
    let hello = json!({ "type": "hello", "sessions": sessions });
    let _ = ws_tx.send(Message::Text(hello.to_string())).await;

    // subscribe to manager events
    let mut rx = state.manager.subscribe().await;

    // a shared sender so both the event pump and command loop can write
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Message>(64);
    let out_pump = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if ws_tx.send(m).await.is_err() {
                break;
            }
        }
    });

    // pump manager events -> client via out_tx
    let event_tx = out_tx.clone();
    let event_pump = tokio::spawn(async move {
        while let Some(evt) = rx.recv().await {
            let msg = Message::Text(json!({"type":"event","event":evt}).to_string());
            if event_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    // read commands from client
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let cmd: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let op = cmd.get("op").and_then(|v| v.as_str()).unwrap_or("");
        match op {
            "create" => {
                let opts: CreateOpts =
                    serde_json::from_value(cmd.get("opts").cloned().unwrap_or(json!({})))
                        .unwrap_or(CreateOpts {
                            cwd: None,
                            name: None,
                            model: None,
                            permission_mode: None,
                            agent: None,
                        });
                match state.manager.create(opts).await {
                    Ok(s) => {
                        let _ = out_tx
                            .send(Message::Text(
                                json!({"type":"created","session":s}).to_string(),
                            ))
                            .await;
                    }
                    Err(e) => {
                        let _ = out_tx
                            .send(Message::Text(json!({"type":"error","error":e}).to_string()))
                            .await;
                    }
                }
            }
            "send" => {
                let sid = cmd
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = cmd
                    .get("content")
                    .map(|v| {
                        if let Some(s) = v.as_str() {
                            s.to_string()
                        } else {
                            v.to_string()
                        }
                    })
                    .unwrap_or_default();
                if let Err(e) = state.manager.send(&sid, content).await {
                    let _ = out_tx
                        .send(Message::Text(
                            json!({"type":"error","error":e,"op":"send"}).to_string(),
                        ))
                        .await;
                }
            }
            "refresh" => {
                state.manager.sync_managed().await;
                let list = state.manager.list().await;
                let _ = out_tx
                    .send(Message::Text(
                        json!({"type":"sessions","sessions":list}).to_string(),
                    ))
                    .await;
            }
            "history" => {
                let sid = cmd
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let limit = cmd.get("limit").and_then(|v| v.as_u64()).unwrap_or(400) as usize;
                let (events, found) = state.manager.history(&sid, limit).await;
                let _ = out_tx
                    .send(Message::Text(
                        json!({"type":"history","sessionId":sid,"events":events,"found":found})
                            .to_string(),
                    ))
                    .await;
            }
            "list" => {
                let list = state.manager.list().await;
                let _ = out_tx
                    .send(Message::Text(
                        json!({"type":"sessions","sessions":list}).to_string(),
                    ))
                    .await;
            }
            "stop" => {
                let sid = cmd
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Err(e) = state.manager.stop(&sid).await {
                    let _ = out_tx
                        .send(Message::Text(
                            json!({"type":"error","error":e,"op":"stop"}).to_string(),
                        ))
                        .await;
                }
            }
            _ => {}
        }
    }
    out_pump.abort();
    event_pump.abort();
}
