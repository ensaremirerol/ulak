//! `join` against a TLS server that redeems the code with the real ulak-remote `Enroller`.
use rcgen::{CertificateParams, KeyPair};
use rustls::{
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
use ulak_proto::EnrollRequest;
use ulak_remote::{Ca, DeviceKind, Enroller, Store};
use ulakd::{config::Identity, errors::JoinError, join::join};

fn der(pem: &str) -> Vec<u8> {
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .unwrap()
        .1
        .contents
}

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ulakd-{tag}-{}", std::process::id()))
}

async fn hub() -> (u16, Arc<Enroller>, String, String) {
    let ca = Ca::load_or_create(&tmp(&format!(
        "ca-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )))
    .unwrap();
    let (ca_pem, fp) = (ca.cert_pem().to_owned(), ca.fingerprint().to_owned());
    let enroller = Arc::new(Enroller::new(
        ca,
        Arc::new(Store::open_in_memory().unwrap()),
    ));
    let key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let chain = vec![
        CertificateDer::from(leaf.der().to_vec()),
        CertificateDer::from(der(&ca_pem)),
    ];
    let cfg = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, PrivateKeyDer::try_from(key.serialize_der()).unwrap())
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let e = Arc::clone(&enroller);
    tokio::spawn(async move {
        loop {
            let (tcp, _) = l.accept().await.unwrap();
            let (acceptor, e) = (acceptor.clone(), Arc::clone(&e));
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let body = loop {
                    let n = tls.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len: usize = head
                            .to_lowercase()
                            .split("content-length: ")
                            .nth(1)
                            .unwrap()
                            .split("\r\n")
                            .next()
                            .unwrap()
                            .parse()
                            .unwrap();
                        if body.len() >= len {
                            break body.to_owned();
                        }
                    }
                };
                let req: EnrollRequest = serde_json::from_str(&body).unwrap();
                let resp = match e.redeem(&req, now()) {
                    Ok(done) => format!(
                        "200 OK\r\nContent-Length: {{}}\r\n\r\n{}",
                        serde_json::to_string(&done.response).unwrap()
                    ),
                    Err(err) => format!("403 Forbidden\r\nContent-Length: {{}}\r\n\r\n{err}"),
                };
                let (head, payload) = resp.split_once("\r\n\r\n").unwrap();
                let out = format!(
                    "HTTP/1.1 {}\r\n\r\n{payload}",
                    head.replace("{}", &payload.len().to_string())
                );
                tls.write_all(out.as_bytes()).await.unwrap();
                tls.shutdown().await.unwrap();
            });
        }
    });
    (port, enroller, ca_pem, fp)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::test]
async fn join_enrolls_and_stores_identity() {
    let (port, enroller, ca_pem, fp) = hub().await;
    let otp = enroller.begin(DeviceKind::Node, now()).otp;
    let dir = tmp("join-ok");
    let _ = std::fs::remove_dir_all(&dir);
    let mut answers = vec![otp.expose().to_lowercase(), fp.to_uppercase()];
    let id = join(&format!("https://localhost:{port}"), &dir, &mut |_q| {
        Ok(answers.pop().unwrap())
    })
    .await
    .unwrap();
    let ident = Identity::load(&dir).unwrap();
    assert_eq!(ident.node_id, id);
    assert_eq!(ident.ca_pem, ca_pem);
    assert!(ident.key_pem.contains("PRIVATE KEY"));
    // the stored material builds a valid mTLS client config
    ulakd::hub::client_tls(&ident.ca_pem, &ident.cert_pem, &ident.key_pem).unwrap();
}

#[tokio::test]
async fn wrong_fingerprint_aborts_before_the_code_is_sent() {
    let (port, enroller, _, _) = hub().await;
    let otp = enroller.begin(DeviceKind::Node, now()).otp;
    let dir = tmp("join-bad-fp");
    let mut answers = vec![otp.expose().to_owned(), "00".repeat(32)];
    let r = join(&format!("https://localhost:{port}"), &dir, &mut |_q| {
        Ok(answers.pop().unwrap())
    })
    .await;
    assert!(matches!(r, Err(JoinError::FingerprintMismatch(_))));
    assert!(!dir.join("key.pem").exists());
    // the code was not spent
    assert!(enroller.consume(&otp, now()).is_ok());
}

#[tokio::test]
async fn bad_otp_is_reported() {
    let (port, _enroller, _, fp) = hub().await;
    let dir = tmp("join-bad-otp");
    let mut answers = vec!["AAAAAAAA".to_owned(), fp];
    let r = join(&format!("https://localhost:{port}"), &dir, &mut |_q| {
        Ok(answers.pop().unwrap())
    })
    .await;
    assert!(matches!(r, Err(JoinError::Rejected { status: 403, .. })));
    assert!(!dir.join("key.pem").exists());
}
