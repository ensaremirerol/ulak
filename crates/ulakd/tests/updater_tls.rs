//! The updater's real TLS path: a local HTTPS server with a throwaway CA proves chains and
//! hostnames are verified; one ignored test talks to the real api.github.com.
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::{
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
use ulak_proto::{UpdateState, VersionInfo};
use ulakd::{
    errors::UpdateError,
    update::{config::UpdateConfig, http::Http, Updater},
};

struct Ca {
    der: Vec<u8>,
    issuer: Issuer<'static, KeyPair>,
}

fn ca() -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.distinguished_name.push(DnType::CommonName, "test ca");
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let cert = p.self_signed(&key).unwrap();
    Ca {
        der: cert.der().to_vec(),
        issuer: Issuer::new(p, key),
    }
}

/// HTTPS server whose cert covers `sans`. Answers `/ok` 200 "hello", `/to?u=<url>` 302 to `u`,
/// `/who` 200 with the Authorization header it saw ("none" if absent).
async fn serve(ca: &Ca, sans: &[&str]) -> String {
    let key = KeyPair::generate().unwrap();
    let p = CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    let cert = p.signed_by(&key, &ca.issuer).unwrap();
    let cfg = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.der().to_vec())],
            PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
        )
        .unwrap();
    let acc = TlsAcceptor::from(Arc::new(cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = l.accept().await.unwrap();
            let acc = acc.clone();
            tokio::spawn(async move {
                let Ok(mut s) = acc.accept(tcp).await else {
                    return;
                };
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let auth = req
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("authorization: ")
                            .or(l.strip_prefix("Authorization: "))
                    })
                    .unwrap_or("none");
                let resp = if let Some(u) = path.strip_prefix("/to?u=") {
                    format!("HTTP/1.1 302 Found\r\nLocation: {u}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                } else {
                    let body = if path == "/who" { auth } else { "hello" };
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    format!("https://127.0.0.1:{port}")
}

const ANY: &str = "*/*";

fn get(h: &Http, url: &str, token: Option<&str>) -> Result<Vec<u8>, UpdateError> {
    h.get(url, token, ANY, 1000)
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_chain_is_accepted_untrusted_and_wrong_host_are_rejected() {
    let (good, other) = (ca(), ca());
    let ok = serve(&good, &["127.0.0.1"]).await;
    let wrong_name = serve(&good, &["other.example"]).await;
    let untrusted = serve(&other, &["127.0.0.1"]).await;
    tokio::task::spawn_blocking(move || {
        let h = Http::with_test_root(&good.der);
        assert_eq!(get(&h, &format!("{ok}/ok"), None).unwrap(), b"hello");
        let e = get(&h, &format!("{wrong_name}/ok"), None).unwrap_err();
        assert!(matches!(e, UpdateError::Http(_)), "hostname mismatch: {e}");
        let e = get(&h, &format!("{untrusted}/ok"), None).unwrap_err();
        assert!(matches!(e, UpdateError::Http(_)), "untrusted CA: {e}");
        // The production roots (Mozilla bundle) do not know the test CA either.
        let e = get(&Http::new(), &format!("{ok}/ok"), None).unwrap_err();
        assert!(matches!(e, UpdateError::Http(_)), "default roots: {e}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn https_redirect_to_another_host_drops_the_token() {
    let c = ca();
    let api = serve(&c, &["127.0.0.1"]).await;
    let cdn = serve(&c, &["127.0.0.1"]).await; // other port: a different origin
    tokio::task::spawn_blocking(move || {
        let h = Http::with_test_root(&c.der);
        assert_eq!(
            get(&h, &format!("{api}/who"), Some("sekret")).unwrap(),
            b"Bearer sekret"
        );
        let via = format!("{api}/to?u={cdn}/who");
        assert_eq!(get(&h, &via, Some("sekret")).unwrap(), b"none");
    })
    .await
    .unwrap();
}

/// Real network: any HTTP status proves handshake + chain against the bundled roots.
#[test]
#[ignore = "needs internet"]
fn real_api_github_com_handshake_works() {
    let e = Http::new().get("https://api.github.com/", None, ANY, 1 << 20);
    match e {
        Ok(b) => eprintln!("api.github.com: 200, {} bytes", b.len()),
        Err(UpdateError::Http(m)) if m.starts_with("HTTP ") => eprintln!("api.github.com: {m}"),
        Err(e) => panic!("TLS or network failure: {e}"),
    }
}

fn updater_with(http: Http) -> Arc<Updater> {
    let dir = std::env::temp_dir().join(format!("ulakd-tls-{}", std::process::id()));
    let info = VersionInfo::new("0.1.0", "g", 1, "linux", "x86_64").unwrap();
    let mut u = Updater::new(
        UpdateConfig {
            repo: "o/r".into(),
            api_base: "http://127.0.0.1:1".into(),
            ..UpdateConfig::default()
        },
        true,
        None,
        info,
        dir.join("ulakd"),
        dir,
        Err(UpdateError::NoKey),
    );
    u.http = Arc::new(http);
    u.initial_delay = Duration::ZERO;
    Arc::new(u)
}

fn failed(u: &Updater) -> String {
    match u.subscribe().borrow().clone() {
        Some(UpdateState::Failed { message }) => message,
        s => panic!("expected Failed, got {s:?}"),
    }
}

#[tokio::test]
async fn a_panicking_http_task_is_a_failed_update_not_a_dead_daemon() {
    let u = updater_with(Http::panicking());
    u.on_check().await;
    assert!(failed(&u).contains("died"));
    u.on_update_now().await;
    assert!(failed(&u).contains("died"));
    // The periodic loop survives it too (it would have panicked this task).
    let l = tokio::spawn(Arc::clone(&u).auto_loop());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!l.is_finished());
    l.abort();
}
