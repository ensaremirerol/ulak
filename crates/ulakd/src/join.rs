//! `ulakd join`: pin the hub, redeem the OTP, store the signed device certificate.
//! Plain HTTP/1.1 over a TLS stream we already verified by fingerprint (no HTTP client dependency).
use crate::{config, errors::JoinError};
use rcgen::{KeyPair, PKCS_ED25519};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{aws_lc_rs, verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{pem::PemObject, CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{client::TlsStream, TlsConnector};
use ulak_proto::{EnrollRequest, EnrollResponse, NodeId, Otp};

/// SHA-256 of a DER certificate, lowercase hex (same form the hub shows).
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Lowercase hex digits only, so `AB:CD ef` equals `abcdef`.
pub fn normalize_fingerprint(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `https://host[:port]` -> (host, port).
pub fn parse_hub(url: &str) -> Result<(String, u16), JoinError> {
    let bad = || JoinError::Config(crate::errors::ConfigError::BadUrl(url.to_owned()));
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(bad)?
        .trim_end_matches('/');
    if rest.is_empty() || rest.contains('/') {
        return Err(bad());
    }
    // ponytail: IPv6 literals are not supported.
    match rest.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => Ok((h.to_owned(), p.parse().map_err(|_| bad())?)),
        _ => Ok((rest.to_owned(), 443)),
    }
}

pub fn http_request(host: &str, req: &EnrollRequest) -> Result<Vec<u8>, JoinError> {
    let body = serde_json::to_string(req)?;
    Ok(format!(
        "POST /enroll HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes())
}

pub fn parse_http_response(raw: &[u8]) -> Result<EnrollResponse, JoinError> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or(JoinError::BadResponse("no header end"))?;
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or(JoinError::BadResponse("no status line"))?;
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return Err(JoinError::BadResponse(
            "chunked responses are not supported",
        ));
    }
    if status != 200 {
        return Err(JoinError::Rejected {
            status,
            body: body.trim().to_owned(),
        });
    }
    Ok(serde_json::from_str(body)?)
}

/// Sends the request and reads the response to EOF.
pub async fn post_enroll<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    host: &str,
    req: &EnrollRequest,
) -> Result<EnrollResponse, JoinError> {
    stream.write_all(&http_request(host, req)?).await?;
    let mut raw = Vec::new();
    // a TLS peer that closes without close_notify still gave us the whole body (Connection: close)
    if let Err(e) = stream.read_to_end(&mut raw).await {
        if raw.is_empty() || e.kind() != std::io::ErrorKind::UnexpectedEof {
            return Err(e.into());
        }
    }
    parse_http_response(&raw)
}

/// Accepts any server certificate; the caller checks the fingerprint before sending secrets.
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Opens TLS to the hub without trusting it yet; returns the stream and the SHA-256 fingerprints
/// of every certificate the hub presented (leaf first).
pub async fn probe_hub(
    host: &str,
    port: u16,
) -> Result<(TlsStream<TcpStream>, Vec<String>), JoinError> {
    let tls = |e: &dyn std::fmt::Display| JoinError::Tls(e.to_string());
    let provider = Arc::new(aws_lc_rs::default_provider());
    let cfg = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| tls(&e))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_owned()).map_err(|e| tls(&e))?;
    let tcp = TcpStream::connect((host, port)).await?;
    let stream = TlsConnector::from(Arc::new(cfg)).connect(name, tcp).await?;
    let prints = stream
        .get_ref()
        .1
        .peer_certificates()
        .unwrap_or_default()
        .iter()
        .map(|c| fingerprint(c))
        .collect();
    Ok((stream, prints))
}

/// Refuses to replace an existing identity unless `force` (or `$<env>` = `1`), so a forgotten
/// enrollment is not silently overwritten. Call before prompting.
pub fn check_overwrite(dir: &Path, force: bool, env: &'static str) -> Result<(), JoinError> {
    if force
        || std::env::var(env).is_ok_and(|v| v == "1")
        || !(dir.join("cert.pem").exists() || dir.join("key.pem").exists())
    {
        return Ok(());
    }
    let q = |s: Option<String>| s.unwrap_or_else(|| "unknown".into());
    Err(JoinError::AlreadyEnrolled {
        dir: dir.display().to_string(),
        hub: q(std::fs::read_to_string(dir.join("hub_url"))
            .ok()
            .map(|s| s.trim().to_owned())),
        device: q(std::fs::read_to_string(dir.join("cert.pem"))
            .ok()
            .and_then(|c| config::node_id_from_cert(&c).ok())
            .map(|n| n.as_str().to_owned())),
        env,
    })
}

/// Interactive enrollment. `ask` shows a prompt and returns the typed line.
pub async fn join(
    hub_url: &str,
    dir: &Path,
    ask: &mut dyn FnMut(&str) -> std::io::Result<String>,
) -> Result<NodeId, JoinError> {
    let (host, port) = parse_hub(hub_url)?;
    let (mut stream, presented) = probe_hub(&host, port).await?;
    let shown = presented
        .iter()
        .map(|p| format!("  {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    let typed = ask(&format!(
        "Hub {host}:{port} presented certificates:\n{shown}\nType the fingerprint shown in the hub UI: "
    ))?;
    let expected = normalize_fingerprint(&typed);
    if expected.is_empty() || !presented.contains(&expected) {
        return Err(JoinError::FingerprintMismatch(typed.trim().to_owned()));
    }
    let otp = Otp::new(ask("One-time code: ")?.trim().to_ascii_uppercase().as_str())?;
    let key = KeyPair::generate_for(&PKCS_ED25519)?;
    let hostname = hostname();
    let req = EnrollRequest::new(otp, key.public_key_pem(), hostname)?;
    let resp = post_enroll(&mut stream, &host, &req).await?;
    let ca = CertificateDer::from_pem_slice(resp.ca_cert.as_bytes())
        .map_err(|e| JoinError::Tls(e.to_string()))?;
    if fingerprint(&ca) != expected {
        return Err(JoinError::CaMismatch);
    }
    config::save(dir, hub_url, &key.serialize_pem(), &resp)?;
    Ok(config::node_id_from_cert(&resp.cert)?)
}

pub fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        // Windows/macOS have no such files: fall back to the environment.
        .or_else(|| {
            std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .ok()
        })
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> EnrollRequest {
        EnrollRequest::new(Otp::new("ABCD2345").unwrap(), "PEM".into(), "box".into()).unwrap()
    }

    #[test]
    fn join_refuses_to_overwrite_without_force() {
        let d = std::env::temp_dir().join(format!(
            "ulak-ow-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        assert!(check_overwrite(&d, false, "ULAK_TEST_NEVER_SET").is_ok());
        std::fs::write(d.join("cert.pem"), "x").unwrap();
        std::fs::write(d.join("hub_url"), "https://hub:1\n").unwrap();
        let e = check_overwrite(&d, false, "ULAK_TEST_NEVER_SET").unwrap_err();
        assert!(matches!(e, JoinError::AlreadyEnrolled { .. }));
        let m = e.to_string();
        assert!(m.contains("https://hub:1") && m.contains("--force"), "{m}");
        assert!(check_overwrite(&d, true, "ULAK_TEST_NEVER_SET").is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn hub_url_parsing() {
        assert_eq!(
            parse_hub("https://hub.lan:8443/").unwrap(),
            ("hub.lan".into(), 8443)
        );
        assert_eq!(
            parse_hub("https://hub.lan").unwrap(),
            ("hub.lan".into(), 443)
        );
        assert_eq!(
            parse_hub("https://10.0.0.5:9").unwrap(),
            ("10.0.0.5".into(), 9)
        );
        assert!(parse_hub("http://hub").is_err());
        assert!(parse_hub("https://hub/x").is_err());
    }

    #[test]
    fn fingerprint_compare_ignores_format() {
        assert_eq!(normalize_fingerprint("AB:cd ef\n"), "abcdef");
        assert_eq!(fingerprint(b"x").len(), 64);
    }

    #[test]
    fn request_has_length_and_body() {
        let raw = String::from_utf8(http_request("h", &req()).unwrap()).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /enroll HTTP/1.1"));
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
        assert!(body.contains("ABCD2345"));
    }

    #[test]
    fn response_parsing() {
        let ok = br#"HTTP/1.1 200 OK
content-length: 1

{"cert":"C","ca_cert":"A"}"#
            .iter()
            .flat_map(|&b| {
                if b == b'\n' {
                    vec![b'\r', b'\n']
                } else {
                    vec![b]
                }
            })
            .collect::<Vec<u8>>();
        assert_eq!(
            parse_http_response(&ok).unwrap(),
            EnrollResponse::new("C".into(), "A".into())
        );
        let bad = b"HTTP/1.1 403 Forbidden\r\n\r\nbad code";
        assert!(matches!(
            parse_http_response(bad),
            Err(JoinError::Rejected { status: 403, .. })
        ));
        assert!(matches!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(JoinError::BadResponse(_))
        ));
        assert!(parse_http_response(b"garbage").is_err());
    }

    #[tokio::test]
    async fn post_enroll_over_a_stream() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = server.read(&mut buf).await.unwrap();
            let got = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = r#"{"cert":"C","ca_cert":"A"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            server.write_all(resp.as_bytes()).await.unwrap();
            server.shutdown().await.unwrap();
            got
        });
        let r = post_enroll(&mut client, "hub", &req()).await.unwrap();
        assert_eq!(r.cert, "C");
        assert!(srv.await.unwrap().contains("ABCD2345"));
    }
}
