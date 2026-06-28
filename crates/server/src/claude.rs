// Claude Code bridge: drives a `claude -p` session per turn over the supported
// stream-json transport. One logical remote session = one Claude Code session
// id, resumed across turns. Mirrors the Node prototype's behavior including the
// automatic fallback to buffered `--output-format json` when stream-json yields
// nothing (some model gateways drop it).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Clone)]
pub struct ClaudeBin(pub PathBuf);

impl ClaudeBin {
    pub fn resolve(explicit: Option<&PathBuf>) -> Self {
        if let Some(p) = explicit {
            if p.exists() {
                return Self(p.clone());
            }
        }
        for c in [
            std::env::var_os("CLAUDE_BIN").map(PathBuf::from),
            homedir().map(|h| h.join(".hermes/node/bin/claude")),
            homedir().map(|h| h.join(".claude/local/claude")),
            Some(PathBuf::from("/usr/local/bin/claude")),
            Some(PathBuf::from("/opt/homebrew/bin/claude")),
        ]
        .into_iter()
        .flatten()
        {
            if c.exists() {
                return Self(c);
            }
        }
        Self(PathBuf::from("claude"))
    }
}

fn homedir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// One logical remote session.
pub struct ClaudeSession {
    pub id: String,
    pub cwd: PathBuf,
    pub name: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub agent: Option<String>,
    pub bin: ClaudeBin,
    /// Claude Code's persistent session id, captured from the first turn.
    pub cc_session_id: tokio::sync::Mutex<Option<String>>,
    pub state: tokio::sync::Mutex<SessionState>,
    /// Handle to the currently-running turn child, if any. Stored behind an
    /// Arc so Stop (from a different task) can kill the live process.
    child: tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Option<Child>>>>>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Idle,
    Busy,
    Error,
}

impl ClaudeSession {
    pub fn new(
        bin: ClaudeBin,
        id: String,
        cwd: PathBuf,
        name: Option<String>,
        model: Option<String>,
        permission_mode: Option<String>,
        agent: Option<String>,
    ) -> Self {
        Self {
            id,
            cwd,
            name,
            model,
            permission_mode,
            agent,
            bin,
            cc_session_id: tokio::sync::Mutex::new(None),
            state: tokio::sync::Mutex::new(SessionState::Idle),
            child: tokio::sync::Mutex::new(None),
        }
    }

    fn base_args(&self, streaming: bool, cc_sid: &Option<String>) -> Vec<String> {
        let mut args = vec!["-p".into(), "--verbose".into()];
        if streaming {
            args.push("--input-format".into());
            args.push("stream-json".into());
            args.push("--output-format".into());
            args.push("stream-json".into());
            args.push("--include-partial-messages".into());
        } else {
            args.push("--output-format".into());
            args.push("json".into());
        }
        if let Some(m) = &self.permission_mode {
            args.push("--permission-mode".into());
            args.push(m.clone());
        }
        if let Some(m) = &self.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        if let Some(a) = &self.agent {
            args.push("--agent".into());
            args.push(a.clone());
        }
        if let Some(sid) = cc_sid {
            args.push("--resume".into());
            args.push(sid.clone());
        }
        args
    }

    /// Run one turn, streaming events to `tx`. Returns the number of
    /// substantive events produced. Tries stream-json first; falls back to
    /// buffered json if the gateway emits nothing.
    pub async fn run_turn(&self, content: &str, tx: &mpsc::Sender<Value>) -> usize {
        *self.state.lock().await = SessionState::Busy;
        let _ = tx
            .send(serde_json::json!({
                "type": "system", "subtype": "turn_started", "sessionId": self.id
            }))
            .await;

        let cc = self.cc_session_id.lock().await.clone();
        let produced = self.exec_turn(content, true, &cc, tx).await;
        let produced = if produced == 0 {
            let _ = tx
                .send(serde_json::json!({
                    "type": "system", "subtype": "fallback_to_json", "sessionId": self.id
                }))
                .await;
            self.exec_turn(content, false, &cc, tx).await
        } else {
            produced
        };

        *self.state.lock().await = if produced > 0 {
            SessionState::Idle
        } else {
            SessionState::Error
        };
        produced
    }

    async fn exec_turn(
        &self,
        content: &str,
        streaming: bool,
        cc_sid: &Option<String>,
        tx: &mpsc::Sender<Value>,
    ) -> usize {
        let args = self.base_args(streaming, cc_sid);
        let mut cmd = Command::new(&self.bin.0);
        cmd.args(&args)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx
                    .send(serde_json::json!({
                        "type": "system", "subtype": "bridge_error",
                        "sessionId": self.id, "error": e.to_string()
                    }))
                    .await;
                return 0;
            }
        };

        // Write stdin synchronously in this task, then drop it so claude knows
        // the turn's input is complete. Doing this before the stdout reader
        // runs removes the prior take()/put-back race where the reader could
        // steal the Child out from under the input writer (leaving stdin never
        // written and claude hanging forever).
        if let Some(mut stdin) = child.stdin.take() {
            if streaming {
                let msg = serde_json::json!({
                    "type": "user",
                    "message": { "role": "user", "content": [ { "type": "text", "text": content } ] }
                });
                let _ = stdin.write_all(format!("{}\n", msg).as_bytes()).await;
            } else {
                let _ = stdin.write_all(content.as_bytes()).await;
            }
            let _ = stdin.flush().await;
            drop(stdin);
        }

        // Publish the child (stdin already taken) so Stop can kill it. The
        // reader owns the stdout/stderr pipes and the wait; Stop only needs a
        // live reference to call start_kill on.
        let cell: Arc<tokio::sync::Mutex<Option<Child>>> =
            Arc::new(tokio::sync::Mutex::new(Some(child)));
        {
            let mut slot = self.child.lock().await;
            *slot = Some(cell.clone());
        }

        let produced = if streaming {
            self.read_stream(cell.clone(), tx).await
        } else {
            self.read_json(cell.clone(), content, tx).await
        };
        // clear the handle slot now that the turn is done
        {
            let mut slot = self.child.lock().await;
            *slot = None;
        }
        produced
    }

    /// Best-effort interrupt of the current turn: kills the live child if any.
    pub async fn stop(&self) {
        // Kill the live child in place; the owning reader still reaps it.
        let cell = { self.child.lock().await.clone() };
        if let Some(cell) = cell {
            if let Some(c) = cell.lock().await.as_mut() {
                let _ = c.start_kill();
            }
        }
    }

    // stream-json: line-delimited events on stdout
    async fn read_stream(
        &self,
        cell: Arc<tokio::sync::Mutex<Option<Child>>>,
        tx: &mpsc::Sender<Value>,
    ) -> usize {
        // Take the stdout/stderr pipes out of the child in place; the Child
        // itself stays in the cell so Stop can still start_kill it.
        let (stdout, stderr) = {
            let mut guard = cell.lock().await;
            let child = match guard.as_mut() {
                Some(c) => c,
                None => return 0,
            };
            (child.stdout.take().unwrap(), child.stderr.take().unwrap())
        };
        let sid = self.id.clone();
        let tx2 = tx.clone();
        tokio::spawn(forward_stderr(stderr, tx2, sid));

        let mut produced = 0usize;
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.is_empty() {
                continue;
            }
            if let Ok(evt) = serde_json::from_str::<Value>(&line) {
                if self.ingest(&evt, tx).await {
                    produced += 1;
                }
            } else {
                let _ = tx
                    .send(serde_json::json!({
                        "type": "stderr", "sessionId": self.id, "text": line
                    }))
                    .await;
            }
        }
        // Reap the child once stdout hits EOF.
        if let Some(mut c) = cell.lock().await.take() {
            let _ = c.wait().await;
        }
        produced
    }

    // buffered json: collect stdout, parse as array at the end
    async fn read_json(
        &self,
        cell: Arc<tokio::sync::Mutex<Option<Child>>>,
        content: &str,
        tx: &mpsc::Sender<Value>,
    ) -> usize {
        let (mut stdout, stderr) = {
            let mut guard = cell.lock().await;
            let child = match guard.as_mut() {
                Some(c) => c,
                None => return 0,
            };
            (child.stdout.take().unwrap(), child.stderr.take().unwrap())
        };
        let tx2 = tx.clone();
        let sid = self.id.clone();
        tokio::spawn(forward_stderr(stderr, tx2, sid));

        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf).await;
        if let Some(mut c) = cell.lock().await.take() {
            let _ = c.wait().await;
        }

        // echo the user turn (json mode doesn't return it)
        let _ = tx
            .send(serde_json::json!({
                "type": "user", "sessionId": self.id,
                "message": { "role": "user", "content": [ { "type": "text", "text": content } ] }
            }))
            .await;

        let mut produced = 1usize;
        if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&buf) {
            for evt in arr {
                if self.ingest(&evt, tx).await {
                    produced += 1;
                }
            }
        } else if !buf.trim().is_empty() {
            let _ = tx
                .send(serde_json::json!({
                    "type": "stderr", "sessionId": self.id, "text": buf.chars().rev().take(500).collect::<String>()
                }))
                .await;
        }
        produced
    }

    async fn ingest(&self, evt: &Value, tx: &mpsc::Sender<Value>) -> bool {
        // capture the persistent Claude Code session id from authoritative sources
        if let Some(sid) = evt.get("session_id").and_then(|v| v.as_str()) {
            let is_init = evt.get("type").and_then(|v| v.as_str()) == Some("system")
                && evt.get("subtype").and_then(|v| v.as_str()) == Some("init");
            let is_ok_result = evt.get("type").and_then(|v| v.as_str()) == Some("result")
                && evt.get("is_error").and_then(|v| v.as_bool()) != Some(true);
            if is_init || is_ok_result {
                *self.cc_session_id.lock().await = Some(sid.to_string());
            }
        }
        let _ = tx.send(evt.clone()).await;
        // count substantive events for fallback detection
        matches!(
            evt.get("type").and_then(|v| v.as_str()),
            Some("assistant") | Some("result") | Some("user") | Some("progress")
        )
    }
}

async fn forward_stderr<R: AsyncReadExt + Unpin>(
    mut stderr: R,
    tx: mpsc::Sender<Value>,
    sid: String,
) {
    let mut lines = BufReader::new(&mut stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx
            .send(serde_json::json!({
                "type": "stderr", "sessionId": sid, "text": line
            }))
            .await;
    }
}

pub fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

// ---- `claude agents --json` discovery ----
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedEntry {
    #[serde(default)]
    pub id: Option<String>,
    /// Full Claude Code session id (the transcript file is named after this).
    /// `claude agents --json` emits this as `sessionId`.
    #[serde(default, alias = "sessionId")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default, alias = "startedAt")]
    pub started_at: Option<u64>,
}

pub async fn list_managed(bin: &ClaudeBin) -> Result<Vec<ManagedEntry>> {
    // `claude agents` can hang indefinitely on some installs (waiting on a TTY
    // or a gateway). Bound it so server startup never blocks on discovery.
    let fut = async {
        let out = Command::new(&bin.0)
            .args(["agents", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        let parsed: Vec<ManagedEntry> = serde_json::from_slice(&out.stdout).unwrap_or_default();
        Ok::<_, std::io::Error>(parsed)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(anyhow::anyhow!(e)),
        Err(_) => {
            tracing::warn!("`claude agents --json` timed out after 5s; skipping session discovery");
            Ok(Vec::new())
        }
    }
}
