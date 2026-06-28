// Relay client: the local synapse-server dials the public relay's uplink and
// bridges it onto the local server's own WS endpoint, so the relay is a fully
// transparent transport. The local router sees the relay as just another
// client, and the whole command/event protocol is handled once.
//
//   relay.wss  <-->  [run_bridge]  <-->  ws://localhost:PORT/?token=T

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Run the relay bridge forever, reconnecting with backoff on failure. Each
/// iteration:
///   1. dial the relay uplink (wss://.../uplink?deviceId=..&token=..)
///   2. dial the local WS endpoint
///   3. pump frames both ways until either side closes
pub async fn run_bridge(relay_url: &str, device_id: &str, token: &str, local_ws: &str) {
    let mut backoff = 1u64;
    loop {
        match connect_once(relay_url, device_id, token, local_ws).await {
            Ok(()) => {
                tracing::info!("relay bridge ended cleanly; reconnecting");
                backoff = 1;
            }
            Err(e) => {
                tracing::warn!("relay bridge error: {e}; reconnecting in {backoff}s");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn connect_once(relay_url: &str, device_id: &str, token: &str, local_ws: &str) -> Result<()> {
    // Append query params for device id + token to the uplink URL.
    let base = relay_url.trim_end_matches('/');
    let uplink = if base.contains('?') {
        format!("{base}&deviceId={device_id}&token={token}")
    } else {
        format!("{base}?deviceId={device_id}&token={token}")
    };
    let (relay_socket, resp) = connect_async(&uplink).await?;
    tracing::info!("relay uplink connected (HTTP {})", resp.status());

    let (local_socket, _) = connect_async(local_ws).await?;

    let (mut relay_tx, mut relay_rx) = relay_socket.split();
    let (mut local_tx, mut local_rx) = local_socket.split();

    // relay -> local
    let to_local = tokio::spawn(async move {
        while let Some(Ok(msg)) = relay_rx.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
            if local_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    // local -> relay
    while let Some(Ok(msg)) = local_rx.next().await {
        if matches!(msg, Message::Close(_)) {
            break;
        }
        if relay_tx.send(msg).await.is_err() {
            break;
        }
    }
    to_local.abort();
    Ok(())
}
