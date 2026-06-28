// SessionManager owns the set of ClaudeSession instances and broadcasts every
// bridge event to all WebSocket subscribers. Each session's turns run on a
// dedicated task so concurrent sessions are independent.

use crate::claude::{
    list_managed, new_session_id, ClaudeBin, ClaudeSession, ManagedEntry, SessionState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub state: SessionState,
    pub started_at: u64,
    pub attached: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateOpts {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

struct Entry {
    session: Arc<ClaudeSession>,
    started_at: u64,
    attached: bool,
    tx: mpsc::Sender<TurnMsg>,
}

enum TurnMsg {
    Send(String),
    Stop,
}

pub struct SessionManager {
    bin: ClaudeBin,
    default_cwd: PathBuf,
    sessions: Mutex<HashMap<String, Entry>>,
    subscribers: Mutex<Vec<mpsc::Sender<Value>>>,
}

impl SessionManager {
    pub fn new(bin: ClaudeBin, default_cwd: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            bin,
            default_cwd,
            sessions: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
        })
    }

    pub async fn subscribe(&self) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel(256);
        self.subscribers.lock().await.push(tx);
        rx
    }

    async fn broadcast(&self, evt: Value) {
        let mut subs = self.subscribers.lock().await;
        subs.retain(|tx| tx.try_send(evt.clone()).is_ok());
    }

    async fn summary(&self, id: &str, e: &Entry) -> SessionSummary {
        let state = *e.session.state.lock().await;
        SessionSummary {
            id: id.to_string(),
            name: e.session.name.clone(),
            cwd: e.session.cwd.to_string_lossy().to_string(),
            model: e.session.model.clone(),
            agent: e.session.agent.clone(),
            state,
            started_at: e.started_at,
            attached: e.attached,
        }
    }

    pub async fn list(&self) -> Vec<SessionSummary> {
        let sessions = self.sessions.lock().await;
        let mut out = Vec::new();
        for (id, e) in sessions.iter() {
            out.push(self.summary(id, e).await);
        }
        out
    }

    /// Backfill a session's transcript from the Claude Code `.jsonl` store.
    pub async fn history(&self, id: &str, limit: usize) -> (Vec<Value>, bool) {
        let (cwd, cc_id) = {
            let sessions = self.sessions.lock().await;
            match sessions.get(id) {
                Some(e) => (
                    e.session.cwd.to_string_lossy().to_string(),
                    e.session.cc_session_id.lock().await.clone(),
                ),
                None => return (Vec::new(), false),
            }
        };
        // Prefer the persistent Claude Code session id when available, since the
        // transcript file is named after it; fall back to our local id.
        let sid = cc_id.unwrap_or_else(|| id.to_string());
        crate::history::load_transcript(&cwd, &sid, limit).await
    }

    fn spawn_runner(
        self: &Arc<Self>,
        id: String,
        session: Arc<ClaudeSession>,
        mut rx: mpsc::Receiver<TurnMsg>,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    TurnMsg::Send(content) => {
                        let (etx, mut erx) = mpsc::channel::<Value>(256);
                        let session = session.clone();
                        let idc = id.clone();
                        let this2 = this.clone();
                        // forward bridge events to subscribers
                        let fwd = tokio::spawn(async move {
                            while let Some(evt) = erx.recv().await {
                                let mut v = evt;
                                if let Some(obj) = v.as_object_mut() {
                                    obj.insert(
                                        "sessionId".into(),
                                        serde_json::Value::String(idc.clone()),
                                    );
                                }
                                this2.broadcast(v).await;
                            }
                        });
                        session.run_turn(&content, &etx).await;
                        drop(etx);
                        let _ = fwd.await;
                    }
                    TurnMsg::Stop => {
                        session.stop().await;
                        this.broadcast(serde_json::json!({
                            "type": "system", "subtype": "turn_stopped", "sessionId": id
                        }))
                        .await;
                    }
                }
            }
        });
    }

    pub async fn create(self: &Arc<Self>, opts: CreateOpts) -> Result<SessionSummary, String> {
        let id = new_session_id();
        let cwd = opts
            .cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_cwd.clone());
        let session = Arc::new(ClaudeSession::new(
            self.bin.clone(),
            id.clone(),
            cwd,
            opts.name,
            opts.model,
            opts.permission_mode,
            opts.agent,
        ));
        let (tx, rx) = mpsc::channel(16);
        let started_at = now_ms();
        let entry = Entry {
            session: session.clone(),
            started_at,
            attached: false,
            tx,
        };
        let summary = self.summary(&id, &entry).await;
        self.sessions.lock().await.insert(id.clone(), entry);
        self.spawn_runner(id.clone(), session, rx);
        self.broadcast(serde_json::json!({
            "type": "system", "subtype": "session_created", "sessionId": id, "session": summary
        }))
        .await;
        Ok(summary)
    }

    pub async fn send(&self, id: &str, content: String) -> Result<(), String> {
        let tx = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(id)
                .ok_or_else(|| "unknown session".to_string())?
                .tx
                .clone()
        };
        tx.send(TurnMsg::Send(content))
            .await
            .map_err(|e| e.to_string())
    }

    /// Request an interrupt of the current turn for `id`, if one is running.
    pub async fn stop(&self, id: &str) -> Result<(), String> {
        let tx = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(id)
                .ok_or_else(|| "unknown session".to_string())?
                .tx
                .clone()
        };
        // Stop is advisory; ignore a closed channel (no turn running).
        let _ = tx.send(TurnMsg::Stop).await;
        Ok(())
    }

    /// Attach to sessions already running on this machine (`claude agents`).
    pub async fn sync_managed(self: &Arc<Self>) -> usize {
        let entries = match list_managed(&self.bin).await {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let mut count = 0;
        for e in entries {
            if self.attach_managed(e).await.is_some() {
                count += 1;
            }
        }
        count
    }

    async fn attach_managed(self: &Arc<Self>, e: ManagedEntry) -> Option<()> {
        let sid = e.session_id.or(e.id)?;
        let mut sessions = self.sessions.lock().await;
        // dedupe by Claude Code session id
        for entry in sessions.values() {
            if entry.session.cc_session_id.lock().await.as_deref() == Some(&sid) {
                return None;
            }
        }
        let cwd = e
            .cwd
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_cwd.clone());
        let name = e.name.clone().or_else(|| {
            if e.kind.as_deref() == Some("background") {
                Some("Background agent".into())
            } else {
                Some("Interactive".into())
            }
        });
        let session = Arc::new(ClaudeSession::new(
            self.bin.clone(),
            sid.clone(),
            cwd,
            name,
            None,
            None,
            None,
        ));
        // seed cc session id so turns resume into the existing conversation
        *session.cc_session_id.lock().await = Some(sid.clone());
        let (tx, rx) = mpsc::channel(16);
        let started_at = e.started_at.unwrap_or_else(now_ms);
        let entry = Entry {
            session: session.clone(),
            started_at,
            attached: true,
            tx,
        };
        let summary = self.summary(&sid, &entry).await;
        sessions.insert(sid.clone(), entry);
        drop(sessions);
        self.spawn_runner(sid.clone(), session, rx);
        self.broadcast(serde_json::json!({
            "type": "system", "subtype": "session_created", "sessionId": sid, "session": summary
        }))
        .await;
        Some(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
