//! ULAK_DEV_HTTP: plain HTTP on loopback only.
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use ulak_remote::*;

async fn hub() -> Arc<Hub> {
    let dir = std::env::temp_dir().join(format!("ulak-dev-{}", uuid::Uuid::now_v7()));
    let web = dir.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html>hi").unwrap();
    let ca = Ca::load_or_create(&dir).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let cfg = HubConfig {
        web_dir: Some(web),
        ..HubConfig::default()
    };
    Hub::new(store, ca, &["localhost".into()], cfg).unwrap()
}

async fn get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
    c.write_all(
        format!("GET {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn dev_http_serves_web_and_requires_a_session_on_loopback_only() {
    let hub = hub().await;
    assert!(hub
        .bind_dev_http("0.0.0.0:0".parse().unwrap())
        .await
        .is_err());
    let run = hub
        .bind_dev_http("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let index = get(run.local_addr(), "/").await;
    assert!(index.starts_with("HTTP/1.1 200"), "{index}");
    assert!(index.contains("content-security-policy"), "{index}");
    assert!(index.ends_with("hi"));
    let api = get(run.local_addr(), "/api/devices").await;
    assert!(api.starts_with("HTTP/1.1 401"), "{api}");
    run.abort();
}
