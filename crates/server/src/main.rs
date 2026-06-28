mod claude;
mod history;
mod http;
mod manager;
mod tls;

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

    // Build a scannable pairing URL for the QR code. Prefer --pair-host, else
    // auto-detect a LAN IP so a phone on the same Wi-Fi can scan & connect.
    let pair_host = args.pair_host.clone().unwrap_or_else(detect_lan_ip);
    let tls_flag = if args.tls { 1 } else { 0 };
    let pair_url = format!(
        "synapse://{pair_host}:{}?token={token}&tls={tls_flag}",
        args.port
    );
    println!("  Pairing URL:    {pair_url}");
    println!("  Scan this QR with the app to bind this device:\n");
    match qr2term::print_qr(&pair_url) {
        Ok(_) => println!(),
        Err(e) => tracing::warn!("could not render pairing QR: {e}"),
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
