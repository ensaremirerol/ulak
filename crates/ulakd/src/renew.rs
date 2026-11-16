//! Client side of certificate renewal: `POST /renew` over mTLS when the cert is near expiry,
//! then an atomic replace of `cert.pem`/`ca.pem` (the key is kept, so the pair never mismatches).
use crate::{
    config::{self, Identity},
    errors::JoinError,
    hub::client_tls,
    join::{parse_http_response, parse_hub},
};
use rcgen::KeyPair;
use rustls::pki_types::ServerName;
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

/// Renew when this many days or fewer are left (the hub allows the same window by default).
pub const WINDOW_DAYS: i64 = 30;
/// How often a running client re-checks.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

/// `notAfter` of the leaf certificate, unix ms.
pub fn not_after_ms(cert_pem: &str) -> Result<i64, JoinError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|_| JoinError::Tls("bad certificate pem".into()))?;
    let cert = pem
        .parse_x509()
        .map_err(|_| JoinError::Tls("bad certificate".into()))?;
    Ok(cert.validity().not_after.timestamp() * 1000)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub fn is_due(cert_pem: &str, now_ms: i64) -> Result<bool, JoinError> {
    Ok(not_after_ms(cert_pem)? - now_ms <= WINDOW_DAYS * 86_400_000)
}

fn write_atomic(path: &Path, data: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("pem.new");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

/// Renews now (no due check) and stores the result in `dir`; returns the reloaded identity.
pub async fn renew(dir: &Path, id: &Identity) -> Result<Identity, JoinError> {
    let tls = |e: &dyn std::fmt::Display| JoinError::Tls(e.to_string());
    let (host, port) = parse_hub(&id.hub_url)?;
    let cfg = client_tls(&id.ca_pem, &id.cert_pem, &id.key_pem).map_err(|e| tls(&e))?;
    let name = ServerName::try_from(host.clone()).map_err(|e| tls(&e))?;
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    let mut s = TlsConnector::from(cfg).connect(name, tcp).await?;
    let body = serde_json::json!({"public_key": KeyPair::from_pem(&id.key_pem)?.public_key_pem()})
        .to_string();
    let req = format!(
        "POST /renew HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await?;
    let mut raw = Vec::new();
    if let Err(e) = s.read_to_end(&mut raw).await {
        if raw.is_empty() || e.kind() != std::io::ErrorKind::UnexpectedEof {
            return Err(e.into());
        }
    }
    let resp = parse_http_response(&raw)?;
    config::node_id_from_cert(&resp.cert)
        .ok()
        .filter(|n| *n == id.node_id)
        .ok_or(JoinError::Tls("renewed cert is for another device".into()))?;
    write_atomic(&dir.join("ca.pem"), &resp.ca_cert)?;
    write_atomic(&dir.join("cert.pem"), &resp.cert)?;
    Ok(Identity::load(dir)?)
}

/// `Some(new identity)` when the cert was due and got renewed.
pub async fn renew_if_due(dir: &Path, id: &Identity) -> Result<Option<Identity>, JoinError> {
    if is_due(&id.cert_pem, now_ms())? {
        Ok(Some(renew(dir, id).await?))
    } else {
        Ok(None)
    }
}
