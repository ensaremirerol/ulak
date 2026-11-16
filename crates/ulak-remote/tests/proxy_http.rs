//! ULAK_HTTP_ADDR: plain HTTP behind a reverse proxy.
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use ulak_remote::*;

const ORIGIN: &str = "https://hub.example.org";

async fn start(trusted: &str) -> (Arc<Hub>, Running, Running) {
    let dir = std::env::temp_dir().join(format!("ulak-px-{}", uuid::Uuid::now_v7()));
    let web = dir.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html>hi").unwrap();
    let ca = Ca::load_or_create(&dir).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let cfg = HubConfig {
        web_dir: Some(web),
        enroll_per_min: 2,
        rp_id: Some("hub.example.org".into()),
        origin: Some(ORIGIN.into()),
        trusted_proxies: TrustedProxies::parse(trusted).unwrap(),
        ..HubConfig::default()
    };
    let hub = Hub::new(store, ca, &["localhost".into()], cfg).unwrap();
    let tls = hub.bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    // A non-loopback address is fine here (unlike the dev listener); 127.0.0.1 keeps the test local.
    let http = hub.bind_http("127.0.0.1:0".parse().unwrap()).await.unwrap();
    (hub, tls, http)
}

async fn req(run: &Running, method: &str, path: &str, headers: &[&str], body: &str) -> String {
    let mut c = tokio::net::TcpStream::connect(run.local_addr())
        .await
        .unwrap();
    let close = if headers.iter().any(|h| h.starts_with("connection")) {
        ""
    } else {
        "connection: close\r\n"
    };
    let extra: String = headers.iter().map(|h| format!("{h}\r\n")).collect();
    c.write_all(
        format!(
            "{method} {path} HTTP/1.1\r\nhost: localhost\r\n{close}content-length: {}\r\n{extra}\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    // Upgrade requests keep the connection open: stop once the head is in.
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while !out.windows(4).any(|w| w == b"\r\n\r\n") {
        match c.read(&mut buf).await.unwrap() {
            0 => break,
            n => out.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn status(r: &str) -> &str {
    r.split(' ').nth(1).unwrap()
}

#[tokio::test]
async fn http_listener_omits_mtls_routes_and_requires_sessions() {
    let (_h, _tls, http) = start("").await;
    assert_eq!(status(&req(&http, "GET", "/", &[], "").await), "200");
    assert_eq!(
        status(&req(&http, "POST", "/renew", &[], "{}").await),
        "404"
    );
    assert_eq!(
        status(&req(&http, "GET", "/ws/daemon", &[], "").await),
        "404"
    );
    for p in ["/api/devices", "/api/version"] {
        assert_eq!(status(&req(&http, "GET", p, &[], "").await), "401", "{p}");
    }
    let up = [
        "upgrade: websocket",
        "connection: upgrade",
        "sec-websocket-version: 13",
        "sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==",
    ];
    // no session: 401 (a client cert cannot exist on this listener)
    assert_eq!(status(&req(&http, "GET", "/ws/ui", &up, "").await), "401");
    // /enroll is routed (bad body -> 400, not 404)
    assert_eq!(
        status(&req(&http, "POST", "/enroll", &[], "{}").await),
        "400"
    );
}

#[tokio::test]
async fn enroll_rate_limit_is_keyed_by_forwarded_ip_only_from_trusted_proxies() {
    let (_h, _tls, http) = start("127.0.0.1").await;
    let a = ["x-forwarded-for: 6.6.6.6, 203.0.113.1"];
    let b = ["x-forwarded-for: 6.6.6.6, 203.0.113.2"];
    for _ in 0..2 {
        assert_eq!(
            status(&req(&http, "POST", "/enroll", &a, "{}").await),
            "400"
        );
    }
    assert_eq!(
        status(&req(&http, "POST", "/enroll", &a, "{}").await),
        "429"
    );
    // another real client behind the same proxy is not limited
    assert_eq!(
        status(&req(&http, "POST", "/enroll", &b, "{}").await),
        "400"
    );

    // proxy not trusted: X-Forwarded-For is ignored, every request is the peer
    let (_h, _tls, http) = start("").await;
    for (i, want) in ["400", "400", "429"].iter().enumerate() {
        let h = [format!("x-forwarded-for: 203.0.113.{i}")];
        let h: Vec<&str> = h.iter().map(String::as_str).collect();
        assert_eq!(
            status(&req(&http, "POST", "/enroll", &h, "{}").await),
            *want
        );
    }
}

#[tokio::test]
async fn ws_ui_with_a_cookie_checks_origin() {
    let (_h, _tls, http) = start("").await;
    let up = |origin: Option<&'static str>| {
        let mut v = vec![
            "upgrade: websocket",
            "connection: upgrade",
            "sec-websocket-version: 13",
            "sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==",
            "cookie: ulak_session=nope",
        ];
        if let Some(o) = origin {
            v.push(o);
        }
        v
    };
    let r = |o| {
        let http = &http;
        async move { status(&req(http, "GET", "/ws/ui", &up(o), "").await).to_owned() }
    };
    assert_eq!(r(Some("origin: https://evil.example")).await, "403");
    assert_eq!(r(Some("origin: https://hub.example.org:8443")).await, "403");
    // right origin or no Origin (non-browser): proceeds to session check (bad session -> 401)
    assert_eq!(r(Some("origin: https://hub.example.org")).await, "401");
    assert_eq!(r(None).await, "401");
}
