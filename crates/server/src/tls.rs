// TLS helpers: load a PEM cert/key pair, or generate a self-signed certificate
// for quick secure remote access. The self-signed path is for development /
// personal use — the mobile app trusts it via `danger_accept_invalid_certs`.

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{CertificateParams, KeyPair, SanType};
use std::convert::TryInto;
use std::path::Path;

/// Build a `RustlsConfig` from on-disk PEM cert + key files.
pub async fn config_from_files(cert: &Path, key: &Path) -> Result<RustlsConfig> {
    let config = RustlsConfig::from_pem_file(cert, key)
        .await
        .map_err(|e| anyhow::anyhow!("loading TLS cert/key: {e}"))?;
    Ok(config)
}

/// Generate a self-signed certificate valid for `subject` (a host name or IP)
/// plus its SANs, returning a ready `RustlsConfig`. Also writes the cert/key to
/// `cert_path`/`key_path` if provided so the same cert can be reused across
/// restarts and the public cert can be inspected/transferred.
pub async fn self_signed_config(
    sans: &[String],
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<RustlsConfig> {
    let mut params = CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        sans.first().map(|s| s.as_str()).unwrap_or("synapse"),
    );

    // Subject Alternative Names — include supplied hosts/IPs plus localhost so
    // the cert is usable for local testing too.
    let mut san_types: Vec<SanType> = Vec::new();
    for s in sans {
        if let Ok(ip) = s.parse::<std::net::IpAddr>() {
            san_types.push(SanType::IpAddress(ip));
        } else {
            san_types.push(SanType::DnsName(
                s.as_str().try_into().context("invalid DNS SAN")?,
            ));
        }
    }
    if !san_types
        .iter()
        .any(|s| matches!(s, SanType::DnsName(n) if n.as_str() == "localhost"))
    {
        san_types.push(SanType::DnsName(
            "localhost".try_into().context("invalid DNS SAN")?,
        ));
    }
    if !san_types
        .iter()
        .any(|s| matches!(s, SanType::IpAddress(ip) if ip.is_loopback()))
    {
        san_types.push(SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));
    }
    params.subject_alt_names = san_types;

    // Valid for ~10 years so personal-use certs don't churn.
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(3650);

    let key = KeyPair::generate().context("generating TLS key")?;
    let cert = params.self_signed(&key).context("self-signing TLS cert")?;

    let cert_pem = cert.pem();
    let key_pem = key.serialize_pem();

    if let (Some(cp), Some(kp)) = (cert_path, key_path) {
        if let (Err(e1), Err(e2)) = (std::fs::write(cp, &cert_pem), std::fs::write(kp, &key_pem)) {
            tracing::warn!("failed to persist self-signed cert/key: {e1}; {e2}");
        }
    }

    let config = RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("building RustlsConfig from self-signed cert: {e}"))?;
    Ok(config)
}
