mod claude;
mod history;
mod http;
mod manager;
mod relay;
mod tls;
mod tunnel;

use anyhow::{Context, Result};
use clap::Parser;
use manager::SessionManager;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Synapse server — remote mobile control for the Claude Code CLI.
#[derive(Parser, Debug)]
#[command(name = "synapse-server", version)]
struct Args {
    /// HTTP/WS port.
    #[arg(short, long, default_value = "4173")]
    port: u16,
    /// Bind host.
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Default working directory for new sessions.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Fixed pairing token (default: random 6-char code).
    #[arg(long)]
    token: Option<String>,
    /// Path to the claude binary.
    #[arg(long)]
    bin: Option<PathBuf>,
    /// Enable TLS (wss:// / https://). Use one of --tls-cert/--tls-key or
    /// --tls-self-signed.
    #[arg(long)]
    tls: bool,
    /// Path to a PEM certificate chain (enables TLS with --tls-key).
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    /// Path to the PEM private key matching --tls-cert.
    #[arg(long)]
    tls_key: Option<PathBuf>,
    /// Generate an in-memory self-signed certificate for TLS. Optional comma-
    /// separated --tls-san list adds hosts/IPs to the certificate.
    #[arg(long)]
    tls_self_signed: bool,
    /// Comma-separated Subject Alternative Names (hosts/IPs) for the self-
    /// signed certificate, e.g. "mybox,192.168.1.10".
    #[arg(long)]
    tls_san: Option<String>,
    /// Where to persist a generated self-signed cert (PEM), if --tls-self-signed.
    #[arg(long)]
    tls_cert_out: Option<PathBuf>,
    /// Where to persist a generated self-signed key (PEM), if --tls-self-signed.
    #[arg(long)]
    tls_key_out: Option<PathBuf>,
    /// Host shown in the pairing QR code / URL. Defaults to an auto-detected
    /// LAN IP (so phones on the same network can scan & connect). Use this to
    /// override with a public hostname/IP for remote setups.
    #[arg(long)]
    pair_host: Option<String>,
    /// Expose the server over the public internet via a Cloudflare Tunnel
    /// (quick tunnel). Gives a public wss:// URL with a real certificate, so
    /// any phone can reach this machine from anywhere with zero network setup.
    #[arg(long)]
    tunnel: bool,
    /// Connect to a self-hosted Synapse relay for public-internet access. The
    /// server dials this relay URL (wss://host/uplink?deviceId=...&token=...) as
    /// an outbound uplink; mobile apps then reach this machine through the relay
    /// from anywhere. Takes precedence over --tunnel for pairing. Example:
    ///   --relay wss://relay.example.com/uplink
    #[arg(long)]
    relay: Option<String>,
    /// Device id registered at the relay (default: a random id).
    #[arg(long)]
    relay_device_id: Option<String>,
    /// Per-device token the app must present to reach this device via the relay.
    /// Defaults to the pairing token.
    #[arg(long)]
    relay_token: Option<String>,
    /// Verbose logging.
    #[arg(long)]
    dev: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Select rustls' crypto provider up front so TLS works regardless of which
    // transitive features are enabled by dependencies.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "synapse_server=info".into()),
        )
        .init();

    let args = Args::parse();
    let cwd = args
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let bin = claude::ClaudeBin::resolve(args.bin.as_ref());

    let manager = SessionManager::new(bin.clone(), cwd.clone());
    let (router, token) = http::router(manager.clone(), args.token);

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;

    let scheme = if args.tls { "wss" } else { "ws" };

    println!("\n  Synapse server is running.\n");
    println!("  Claude binary:  {}", bin.0.display());
    println!("  Working dir:    {}", cwd.display());
    println!("  Pairing token:  {token}");
    if args.tls {
        println!(
            "  TLS:            enabled ({})",
            if args.tls_self_signed {
                "self-signed"
            } else {
                "provided cert"
            }
        );
    }
    println!("\n  Connect your App to: {scheme}://{addr}/?token={token}\n");

    // Build a scannable pairing URL for the QR code.
    // --tunnel: expose over the public internet via Cloudflare (real TLS,
    //   wss://, reachable from any phone anywhere). This is the productized
    //   remote path; the LAN IP path below remains for local use.
    let (pair_host, pair_port, pair_tls) = if args.tunnel {
        println!("  Starting Cloudflare Tunnel (public wss access)…");
        let local_url = format!("http://localhost:{}", args.port);
        match tunnel::start_quick_tunnel(&local_url).await {
            Ok(public_host) => {
                println!("  Public tunnel:  https://{public_host}");
                (public_host, 443u16, 1u8)
            }
            Err(e) => {
                tracing::error!("cloudflare tunnel failed: {e}; falling back to LAN pairing");
                (
                    args.pair_host.clone().unwrap_or_else(detect_lan_ip),
                    args.port,
                    if args.tls { 1 } else { 0 },
                )
            }
        }
    } else {
        (
            args.pair_host.clone().unwrap_or_else(detect_lan_ip),
            args.port,
            if args.tls { 1 } else { 0 },
        )
    };
    let pair_url = format!("synapse://{pair_host}:{pair_port}?token={token}&tls={pair_tls}");
    println!("  Pairing URL:    {pair_url}");
    println!("  Scan this QR with the app to bind this device:\n");
    match qr2term::print_qr(&pair_url) {
        Ok(_) => println!(),
        Err(e) => tracing::warn!("could not render pairing QR: {e}"),
    }

    // If a relay is configured, start the outbound uplink bridge in the
    // background. The server dials the relay, then pipes frames between the
    // relay socket and its own local WS endpoint so the relay is transparent.
    // This runs concurrently with the local listener below.
    if let Some(relay_url) = args.relay.clone() {
        let device_id = args
            .relay_device_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string());
        let relay_token = args.relay_token.clone().unwrap_or_else(|| token.clone());
        let local_ws = format!(
            "{}://localhost:{}/?token={token}",
            if args.tls { "wss" } else { "ws" },
            args.port
        );
        let scheme = if args.tls { "wss" } else { "ws" };
        // Build the uplink URL the app will ultimately use to reach us.
        let connect_host = relay_url
            .replace("wss://", "")
            .replace("ws://", "")
            .replace("/uplink", "")
            .replace("/connect", "");
        let app_connect = format!(
            "synapse://{connect_host}/connect?deviceId={device_id}&token={relay_token}&tls=1"
        );
        println!("  Relay uplink:   {relay_url} (deviceId={device_id})");
        println!("  Relay pair URL: {app_connect}");
        println!("  Scan this QR with the app to bind this device over the relay:\n");
        match qr2term::print_qr(&app_connect) {
            Ok(_) => println!(),
            Err(e) => tracing::warn!("could not render relay pairing QR: {e}"),
        }
        let _ = scheme;
        tokio::spawn(async move {
            relay::run_bridge(&relay_url, &device_id, &relay_token, &local_ws).await;
        });
    }

    // Attach to existing Claude Code sessions in the background so a slow or
    // hanging `claude agents` never blocks the listener. New sessions can
    // always be created from the app regardless.
    tokio::spawn(async move {
        match manager.sync_managed().await {
            n if n > 0 => println!("  Attached {n} existing Claude Code session(s)."),
            _ => {}
        }
    });

    if args.tls {
        let rustls_config = if args.tls_self_signed {
            let sans: Vec<String> = args
                .tls_san
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            tls::self_signed_config(
                &sans,
                args.tls_cert_out.as_deref(),
                args.tls_key_out.as_deref(),
            )
            .await?
        } else {
            let cert = args
                .tls_cert
                .as_ref()
                .context("--tls requires --tls-cert (or use --tls-self-signed)")?;
            let key = args
                .tls_key
                .as_ref()
                .context("--tls requires --tls-key (or use --tls-self-signed)")?;
            tls::config_from_files(cert, key).await?
        };
        axum_server::bind_rustls(addr, rustls_config)
            .serve(router.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router.into_make_service()).await?;
    }
    Ok(())
}

/// Best-effort detection of this machine's LAN IPv4 address for the pairing QR.
/// Resolves by opening a UDP "connection" to a public address (no packets sent)
/// and reading the local socket address. Falls back to 127.0.0.1.
fn detect_lan_ip() -> String {
    use std::net::UdpSocket;
    let candidates = ["8.8.8.8:80", "114.114.114.114:80", "1.1.1.1:80"];
    for addr in candidates {
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(addr).is_ok() {
                if let Ok(local) = sock.local_addr() {
                    let ip = local.ip();
                    if !ip.is_loopback() {
                        return ip.to_string();
                    }
                }
            }
        }
    }
    "127.0.0.1".to_string()
}
