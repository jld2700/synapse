// Cloudflare Tunnel (quick tunnel) integration: spawn `cloudflared` to expose
// the local server on a public `https://*.trycloudflare.com` URL with a real
// (Let's Encrypt) certificate. This gives any user secure remote (wss://)
// access with zero network setup and no domain of their own — the productized
// path for mobile access from anywhere.

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Start a cloudflared quick tunnel pointing at `local_url` (e.g.
/// `http://localhost:4173`) and return the public hostname
/// (`xxx-yyy.trycloudflare.com`) once cloudflared reports it. The spawned
/// cloudflared is kept alive for the lifetime of the server.
pub async fn start_quick_tunnel(local_url: &str) -> Result<String> {
    which::which("cloudflared").context(
        "`cloudflared` was not found on PATH. Install it to use --tunnel:\n  \
         macOS:  brew install cloudflared\n  \
         Linux:  see https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/",
    )?;

    let mut child = Command::new("cloudflared")
        .args(["tunnel", "--url", local_url])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Don't kill cloudflared when the parent exits unexpectedly; let it be
        // reparented so the tunnel stays up.
        .spawn()
        .context("failed to spawn cloudflared")?;

    let stderr = child
        .stderr
        .take()
        .context("cloudflared stderr unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("cloudflared stdout unavailable")?;

    // The public URL may appear on either stream; scan both until found.
    let (host_tx, mut host_rx) = tokio::sync::mpsc::channel::<String>(4);
    spawn_line_scanner(stderr, host_tx.clone());
    spawn_line_scanner(stdout, host_tx);

    // Wait (bounded) for one of the scanners to find the public hostname.
    let host = tokio::time::timeout(std::time::Duration::from_secs(45), host_rx.recv())
        .await
        .context("timed out waiting for cloudflared to publish the public URL")?
        .context("cloudflared ended before publishing a public URL")?;

    // Keep cloudflared alive by awaiting it; if it ever dies, log so the user
    // knows the tunnel is down. (We must NOT drop the Child — that would kill
    // cloudflared.) The Child is held by this task for the server's lifetime.
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => {
                tracing::warn!("cloudflared exited (status={status}); public tunnel is down");
            }
            Err(e) => {
                tracing::warn!("cloudflared child wait error: {e}");
            }
        }
    });

    Ok(host)
}

/// Spawn a task that reads lines from `reader` and sends the first parsed
/// trycloudflare host through `tx`. The task keeps draining the stream for the
/// process's lifetime (important: dropping a reader the child still writes to
/// can raise SIGPIPE and kill cloudflared).
fn spawn_line_scanner<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    tx: tokio::sync::mpsc::Sender<String>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        let mut sent = false;
        while let Ok(Some(line)) = lines.next_line().await {
            if !sent {
                if let Some(host) = extract_trycloudflare_host(&line) {
                    let _ = tx.send(host).await;
                    sent = true;
                }
            }
        }
        // Drain complete; if we never found a host, signal closure.
    });
}

/// Pull the `xxx.trycloudflare.com` host out of a cloudflared log line.
fn extract_trycloudflare_host(line: &str) -> Option<String> {
    let l = line.trim();
    let start = l.find("https://")?;
    let rest = &l[start + "https://".len()..];
    let end = rest.find(|c: char| c.is_whitespace() || c == '|')?;
    let host = &rest[..end];
    if host.ends_with(".trycloudflare.com") {
        Some(host.to_string())
    } else {
        None
    }
}
