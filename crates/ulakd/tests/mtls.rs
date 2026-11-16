//! Real mTLS: a TLS WebSocket server that requires a client certificate signed by a private CA.
use futures_util::StreamExt;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::{
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use ulakd::{
    hub::{client_tls, connect},
    join::{fingerprint, probe_hub},
};

fn leaf(
    cn: &str,
    san: &[&str],
    eku: ExtendedKeyUsagePurpose,
    issuer: &Issuer<'_, KeyPair>,
) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut p =
        CertificateParams::new(san.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    p.distinguished_name.push(DnType::CommonName, cn);
    p.extended_key_usages = vec![eku];
    (
        p.signed_by(&key, issuer).unwrap().pem(),
        key.serialize_pem(),
    )
}

struct Pki {
    ca_pem: String,
    ca_der: Vec<u8>,
    issuer: Issuer<'static, KeyPair>,
}

fn pki(name: &str) -> Pki {
    let key = KeyPair::generate().unwrap();
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.distinguished_name.push(DnType::CommonName, name);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let cert = p.self_signed(&key).unwrap();
    Pki {
        ca_pem: cert.pem(),
        ca_der: cert.der().to_vec(),
        issuer: Issuer::new(p, key),
    }
}

fn der(pem: &str) -> Vec<u8> {
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .unwrap()
        .1
        .contents
}

/// Serves one TLS WebSocket connection; returns the client's certificate CN, if any.
async fn serve(pki: &Pki) -> (u16, tokio::task::JoinHandle<Option<Vec<u8>>>) {
    let (cert, key) = leaf(
        "hub",
        &["localhost"],
        ExtendedKeyUsagePurpose::ServerAuth,
        &pki.issuer,
    );
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(pki.ca_der.clone())).unwrap();
    let provider = Arc::new(aws_lc_rs::default_provider());
    let verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider))
            .build()
            .unwrap();
    let chain = vec![
        CertificateDer::from(der(&cert)),
        CertificateDer::from(pki.ca_der.clone()),
    ];
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, PrivateKeyDer::try_from(der(&key)).unwrap())
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (tcp, _) = l.accept().await.unwrap();
        let tls = acceptor.accept(tcp).await.ok()?;
        let peer = tls.get_ref().1.peer_certificates()?.first()?.to_vec();
        let mut ws = tokio_tungstenite::accept_async(tls).await.ok()?;
        let _ = ws.next().await; // wait for the client to close
        Some(peer)
    });
    (port, h)
}

#[tokio::test]
async fn client_with_device_cert_connects_and_hub_sees_it() {
    let p = pki("test hub CA");
    let (port, server) = serve(&p).await;
    let (cert, key) = leaf(
        "node-1",
        &[],
        ExtendedKeyUsagePurpose::ClientAuth,
        &p.issuer,
    );
    let tls = client_tls(&p.ca_pem, &cert, &key).unwrap();
    let mut ws = connect(&format!("wss://localhost:{port}/ws/daemon"), Some(tls))
        .await
        .unwrap();
    ws.close(None).await.unwrap();
    assert_eq!(server.await.unwrap().unwrap(), der(&cert));
}

#[tokio::test]
async fn client_that_does_not_trust_the_hub_ca_is_refused() {
    let p = pki("test hub CA");
    let other = pki("someone else");
    let (port, _server) = serve(&p).await;
    let (cert, key) = leaf(
        "node-1",
        &[],
        ExtendedKeyUsagePurpose::ClientAuth,
        &p.issuer,
    );
    let tls = client_tls(&other.ca_pem, &cert, &key).unwrap();
    assert!(
        connect(&format!("wss://localhost:{port}/ws/daemon"), Some(tls))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn join_probe_reports_the_ca_fingerprint() {
    let p = pki("test hub CA");
    let (port, _server) = serve(&p).await;
    // no client cert: the handshake completes on the client side before the server rejects it
    let (_stream, prints) = probe_hub("localhost", port).await.unwrap();
    assert!(prints.contains(&fingerprint(&p.ca_der)));
}
