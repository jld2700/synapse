// Transcript backfill: read a persisted Claude Code session transcript from
// disk and return the conversational events (user / assistant / system-init)
// in order, shaped like the live stream-json events the client already
// ingests. Mirrors web/src/bridge/history.js.
//
// Claude Code stores transcripts at:
//   ~/.claude/projects/<cwd-with-slashes-as-dashes>/<sessionId>.jsonl
// Each line is one JSON event; we keep only message-bearing events so the
// backfilled transcript matches what a live session would have streamed.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".claude"))
                .unwrap_or_else(|| PathBuf::from(".claude"))
        })
}

// Claude Code encodes cwd by replacing non-alnum with '-'.
fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn project_dir(cwd: &str) -> PathBuf {
    claude_dir().join("projects").join(encode_cwd(cwd))
}

pub fn transcript_path(cwd: &str, session_id: &str) -> PathBuf {
    project_dir(cwd).join(format!("{session_id}.jsonl"))
}

/// Read a transcript and return normalized conversational events, capped to the
/// most recent `limit` to bound payload size. Returns `found: false` if the
/// file is absent or unreadable.
pub async fn load_transcript(cwd: &str, session_id: &str, limit: usize) -> (Vec<Value>, bool) {
    let path = transcript_path(cwd, session_id);
    load_from(&path, session_id, limit).await
}

async fn load_from(path: &Path, session_id: &str, limit: usize) -> (Vec<Value>, bool) {
    let data = match tokio::fs::read_to_string(path).await {
        Ok(d) => d,
        Err(_) => return (Vec::new(), false),
    };
    let mut events: Vec<Value> = Vec::new();
    for line in data.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut evt = match serde_json::from_str::<Value>(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
        // Keep only message-bearing events the client renders.
        if ty != "user" && ty != "assistant" && ty != "system" {
            continue;
        }
        if ty == "system" && evt.get("subtype").and_then(|v| v.as_str()) != Some("init") {
            continue;
        }
        if evt.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        // Normalize: tag with sessionId, drop transcript-internal fields.
        if let Some(obj) = evt.as_object_mut() {
            obj.insert("sessionId".into(), Value::String(session_id.into()));
            if let Some(m) = obj.get_mut("message").and_then(|m| m.as_object_mut()) {
                m.remove("id");
                m.remove("model");
            }
            obj.remove("parentUuid");
            obj.remove("promptId");
            let cleaned = obj.clone();
            evt = Value::Object(cleaned);
        }
        events.push(evt);
    }
    let found = !events.is_empty();
    if events.len() > limit {
        events = events.split_off(events.len() - limit);
    }
    (events, found)
}

/// Inline normalize helper used when building a client-facing history payload.
pub fn _shape_session(_obj: &Map<String, Value>) -> Value {
    Value::Null
}
