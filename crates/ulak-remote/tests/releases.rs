//! Hub release cache: fill, verification, pruning, serving, auth, rate limit.
#![cfg(unix)]
#![allow(dead_code)]
use axum::{
    extract::{Path as AxPath, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};
use ulak_proto::*;
use ulak_remote::*;

const TARGET: &str = "ulakd-x86_64-unknown-linux-gnu.tar.gz";
const SECRET: &str = "ghp_SECRETTOKEN0123456789";
const REPO: &str = "o/r";

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Fx {
    hub: Arc<Hub>,
    run: Running,
    ca_pem: String,
}

async fn start(cfg: HubConfig) -> Fx {
    let dir = std::env::temp_dir().join(format!("ulak-hub-{}", uuid::Uuid::now_v7()));
    start_in(&dir, Arc::new(Store::open_in_memory().unwrap()), cfg).await
}

async fn start_in(dir: &std::path::Path, store: Arc<Store>, cfg: HubConfig) -> Fx {
    let ca = Ca::load_or_create(dir).unwrap();
    let ca_pem = ca.cert_pem().to_owned();
    let hub = Hub::new(store, ca, &["127.0.0.1".into(), "localhost".into()], cfg).unwrap();
    let run = hub.bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    Fx { hub, run, ca_pem }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn tls(ca_pem: &str, identity: Option<&Identity>) -> ClientConfig {
    let mut roots = RootCertStore::empty();
    for c in rustls_pemfile::certs(&mut ca_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let b = ClientConfig::builder().with_root_certificates(roots);
    match identity {
        None => b.with_no_client_auth(),
        Some(i) => {
            let chain = rustls_pemfile::certs(&mut i.cert.as_bytes())
                .map(|c| c.unwrap())
                .collect();
            let key = rustls_pemfile::private_key(&mut i.key.as_bytes())
                .unwrap()
                .unwrap();
            b.with_client_auth_cert(chain, key).unwrap()
        }
    }
}

#[derive(Clone)]
struct Identity {
    id: NodeId,
    cert: String,
    key: String,
}

/// Minimal HTTPS client: one request, `Connection: close`, returns (status, body).
async fn https(
    fx: &Fx,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    https_as(fx, None, method, path, headers, body).await
}

async fn https_as(
    fx: &Fx,
    id: Option<&Identity>,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    let addr = fx.run.local_addr();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let conn = TlsConnector::from(Arc::new(tls(&fx.ca_pem, id)));
    let mut s = conn
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
        .unwrap();
    let extra: String = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await; // close_notify may be missing; partial data is kept
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    (
        status,
        out.split_once("\r\n\r\n")
            .map(|x| x.1)
            .unwrap_or("")
            .to_owned(),
    )
}

async fn enroll_as(fx: &Fx, kind: DeviceKind, host: &str) -> Identity {
    let offer = fx.hub.enroller().begin(kind, now());
    let key = rcgen::KeyPair::generate().unwrap();
    let req = EnrollRequest::new(offer.otp, key.public_key_pem(), host.into()).unwrap();
    let (status, body) = https(fx, "POST", "/enroll", &[], &encode(&req).unwrap()).await;
    assert_eq!(status, 200, "{body}");
    let resp: EnrollResponse = decode(&body).unwrap();
    assert_eq!(resp.ca_cert, fx.ca_pem);
    let cert = resp.cert;
    let (_, der) = x509_parser::pem::parse_x509_pem(cert.as_bytes()).unwrap();
    let c = der.parse_x509().unwrap();
    let cn = c
        .subject()
        .iter_common_name()
        .next()
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    Identity {
        id: NodeId::new(&cn).unwrap(),
        cert,
        key: key.serialize_pem(),
    }
}

async fn connect(
    fx: &Fx,
    path: &str,
    id: Option<&Identity>,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let url = format!("wss://{}{path}", fx.run.local_addr());
    let c = Connector::Rustls(Arc::new(tls(&fx.ca_pem, id)));
    connect_async_tls_with_config(url, None, false, Some(c))
        .await
        .map(|r| r.0)
}

async fn send<T: serde::Serialize>(ws: &mut Ws, m: &T) {
    ws.send(Message::Text(encode(m).unwrap().into()))
        .await
        .unwrap();
}

// ---- fake GitHub ----

#[derive(Default)]
struct Inner {
    tag: Mutex<String>,
    files: Mutex<HashMap<String, Vec<u8>>>,
    base: Mutex<String>,
    fail: Mutex<bool>,
    /// (path, Authorization header)
    hits: Mutex<Vec<(String, Option<String>)>>,
}
type Gh = Arc<Inner>;

fn signer(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Inner {
    /// Publishes `tag` signed by `signer`; `sums_hash` overrides the hash in SHA256SUMS (still signed).
    fn publish_with(&self, tag: &str, signer: &SigningKey, sums_hash: Option<String>) {
        let tgz = format!("archive of {tag}").into_bytes();
        let hash = sums_hash.unwrap_or_else(|| hex(&Sha256::digest(&tgz)));
        let sums = format!("{hash}  {TARGET}\n");
        let mut f = self.files.lock().unwrap();
        f.insert(
            format!("{TARGET}.sig"),
            signer.sign(&tgz).to_bytes().to_vec(),
        );
        f.insert(
            "SHA256SUMS.sig".into(),
            signer.sign(sums.as_bytes()).to_bytes().to_vec(),
        );
        f.insert(TARGET.into(), tgz);
        f.insert("SHA256SUMS".into(), sums.into_bytes());
        *self.tag.lock().unwrap() = tag.into();
    }
    fn publish(&self, tag: &str) {
        self.publish_with(tag, &signer(1), None);
    }
    fn latest_hits(&self) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with("/releases/latest"))
            .count()
    }
}

async fn gh_latest(State(s): State<Gh>, h: HeaderMap) -> Response {
    s.hits.lock().unwrap().push((
        "/releases/latest".into(),
        h.get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
    ));
    if *s.fail.lock().unwrap() {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
    }
    let base = s.base.lock().unwrap().clone();
    let assets: Vec<_> = s
        .files
        .lock()
        .unwrap()
        .iter()
        .map(|(n, d)| {
            format!(
                r#"{{"name":"{n}","url":"{base}/assets/{n}","size":{}}}"#,
                d.len()
            )
        })
        .collect();
    let body = format!(
        r#"{{"tag_name":"{}","draft":false,"prerelease":false,"published_at":"2026-01-01T00:00:00Z","assets":[{}]}}"#,
        s.tag.lock().unwrap(),
        assets.join(",")
    );
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn gh_asset(State(s): State<Gh>, AxPath(name): AxPath<String>, h: HeaderMap) -> Response {
    s.hits.lock().unwrap().push((
        format!("/assets/{name}"),
        h.get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
    ));
    s.files
        .lock()
        .unwrap()
        .get(&name)
        .cloned()
        .unwrap()
        .into_response()
}

async fn fake() -> Gh {
    let s: Gh = Arc::default();
    let app = Router::new()
        .route("/repos/o/r/releases/latest", get(gh_latest))
        .route("/assets/{name}", get(gh_asset))
        .with_state(Arc::clone(&s));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    *s.base.lock().unwrap() = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    s
}

// ---- hub ----

struct Rig {
    fx: Fx,
    gh: Gh,
    rel: std::path::PathBuf,
}

fn cfg(gh: &Gh, rel: &std::path::Path, token: Option<&str>) -> HubConfig {
    HubConfig {
        github_api: gh.base.lock().unwrap().clone(),
        release_dir: Some(rel.to_owned()),
        release_repo: REPO.into(),
        release_key: Some(signer(1).verifying_key()),
        release_token: token.map(|t| GitToken::new(t).unwrap()),
        release_interval: Duration::from_secs(3600),
        ..HubConfig::default()
    }
}

/// Starts a hub and waits for its startup fetch (the loop's first tick) to finish.
async fn rig_tok(tag: Option<&str>, token: Option<&str>) -> Rig {
    let gh = fake().await;
    if let Some(t) = tag {
        gh.publish(t);
    }
    let rel = std::env::temp_dir().join(format!("ulak-rel-{}", uuid::Uuid::now_v7()));
    let fx = start(cfg(&gh, &rel, token)).await;
    for _ in 0..200 {
        if fx.hub.release_report().checked_at != 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_ne!(
        fx.hub.release_report().checked_at,
        0,
        "startup fetch never finished"
    );
    Rig { fx, gh, rel }
}

async fn rig(tag: Option<&str>) -> Rig {
    rig_tok(tag, None).await
}

async fn get_as(r: &Rig, id: &Identity, path: &str) -> (u16, String) {
    https_as(&r.fx, Some(id), "GET", path, &[], "").await
}

#[tokio::test]
async fn cache_fills_and_serves_verified_assets_to_devices() {
    let r = rig(Some("v0.2.0")).await;
    let rep = r.fx.hub.release_report();
    assert_eq!(rep.versions, vec!["0.2.0"]);
    assert_eq!(rep.error, None);
    let node = enroll_as(&r.fx, DeviceKind::Node, "n").await;
    let (st, body) = get_as(&r, &node, "/api/releases/latest").await;
    assert_eq!(st, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["version"], "0.2.0");
    let names: Vec<_> = v["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&TARGET) && names.contains(&"SHA256SUMS"),
        "{names:?}"
    );
    let (st, body) = get_as(&r, &node, &format!("/api/releases/0.2.0/{TARGET}")).await;
    assert_eq!((st, body.as_str()), (200, "archive of v0.2.0"));
    // unknown names and versions, files the listing does not mention, traversal
    for p in [
        "/api/releases/0.2.0/nope",
        "/api/releases/0.2.0/meta.json",
        "/api/releases/0.9.9/SHA256SUMS",
        "/api/releases/0.2.0/..%2fmeta.json",
        "/api/releases/..%2f0.2.0/SHA256SUMS",
        "/api/releases/0.2.0/%2e%2e%2f0.2.0%2fmeta.json",
        "/api/releases/0.2.0/..",
    ] {
        let (st, _) = get_as(&r, &node, p).await;
        assert!(st == 404 || st == 400, "{p} -> {st}");
    }
}

#[tokio::test]
async fn bad_signature_and_bad_checksum_are_never_served() {
    for (what, sig_key, hash) in [
        ("signature", 9u8, None),
        ("checksum", 1, Some("0".repeat(64))),
    ] {
        let gh = fake().await;
        gh.publish_with("v0.2.0", &signer(sig_key), hash);
        let rel = std::env::temp_dir().join(format!("ulak-rel-{}", uuid::Uuid::now_v7()));
        let fx = start(cfg(&gh, &rel, None)).await;
        for _ in 0..200 {
            if fx.hub.release_report().checked_at != 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let rep = fx.hub.release_report();
        assert!(rep.versions.is_empty(), "{what}");
        assert!(
            rep.error.as_deref().unwrap_or("").contains(what),
            "{what}: {:?}",
            rep.error
        );
        assert!(!rel.join("0.2.0").exists(), "{what}");
        let r = Rig { fx, gh, rel };
        let node = enroll_as(&r.fx, DeviceKind::Node, "n").await;
        assert_eq!(get_as(&r, &node, "/api/releases/latest").await.0, 404);
        assert_eq!(
            get_as(&r, &node, &format!("/api/releases/0.2.0/{TARGET}"))
                .await
                .0,
            404
        );
    }
}

#[tokio::test]
async fn pruning_keeps_two_and_startup_drops_tampered_versions() {
    let r = rig(Some("v0.2.0")).await;
    for t in ["v0.3.0", "v0.4.0"] {
        r.gh.publish(t);
        r.fx.hub.refresh_releases(true).await;
    }
    assert_eq!(r.fx.hub.release_report().versions, vec!["0.4.0", "0.3.0"]);
    assert!(!r.rel.join("0.2.0").exists());
    // same version again: no refetch of assets
    let before = r.gh.hits.lock().unwrap().len();
    r.fx.hub.refresh_releases(true).await;
    assert_eq!(
        r.gh.hits.lock().unwrap().len(),
        before + 1,
        "only the listing is fetched"
    );
    // a hub restarted over a tampered cache serves only what still verifies
    std::fs::write(r.rel.join("0.4.0").join(TARGET), b"evil").unwrap();
    let dir = std::env::temp_dir().join(format!("ulak-hub-{}", uuid::Uuid::now_v7()));
    r.gh.hits.lock().unwrap().clear();
    *r.gh.fail.lock().unwrap() = true;
    let fx2 = start_in(
        &dir,
        Arc::new(Store::open_in_memory().unwrap()),
        cfg(&r.gh, &r.rel, None),
    )
    .await;
    let rep = fx2.hub.release_report();
    assert_eq!(
        rep.versions.first().map(String::as_str),
        Some("0.3.0"),
        "{rep:?}"
    );
    assert!(!r.rel.join("0.4.0").exists());
}

#[tokio::test]
async fn only_active_certificate_devices_may_fetch() {
    let r = rig(Some("v0.2.0")).await;
    let path = format!("/api/releases/0.2.0/{TARGET}");
    for p in ["/api/releases/latest", path.as_str()] {
        // anonymous
        assert_eq!(https(&r.fx, "GET", p, &[], "").await.0, 401, "{p}");
        // browser session cookie and agent bearer token carry no certificate
        let t = AgentTokens::issue(
            &**r.fx.hub.store(),
            &AgentId::new("a-1").unwrap(),
            3_600_000,
            now(),
        )
        .unwrap();
        let hdrs = [
            ("Cookie", "ulak_session=abc".to_owned()),
            ("Authorization", format!("Bearer {}", t.expose())),
        ];
        assert_eq!(https(&r.fx, "GET", p, &hdrs, "").await.0, 401, "{p}");
    }
    let ui = enroll_as(&r.fx, DeviceKind::Ui, "p").await;
    assert_eq!(get_as(&r, &ui, "/api/releases/latest").await.0, 200);
    let node = enroll_as(&r.fx, DeviceKind::Node, "n").await;
    assert_eq!(get_as(&r, &node, &path).await.0, 200);
    r.fx.hub.revoke_device(&node.id, now()).unwrap();
    assert_eq!(get_as(&r, &node, "/api/releases/latest").await.0, 403);
    assert_eq!(get_as(&r, &node, &path).await.0, 403);
}

#[tokio::test]
async fn releases_are_rate_limited_per_device() {
    let r = rig(Some("v0.2.0")).await;
    let node = enroll_as(&r.fx, DeviceKind::Node, "n").await;
    let other = enroll_as(&r.fx, DeviceKind::Node, "m").await;
    let mut codes = Vec::new();
    for _ in 0..32 {
        codes.push(get_as(&r, &node, "/api/releases/latest").await.0);
    }
    assert_eq!(codes.iter().filter(|c| **c == 200).count(), 30, "{codes:?}");
    assert_eq!(codes[31], 429);
    assert_eq!(get_as(&r, &other, "/api/releases/latest").await.0, 200);
}

#[tokio::test]
async fn token_is_sent_to_github_only_and_never_leaks() {
    let r = rig_tok(Some("v0.2.0"), Some(SECRET)).await;
    let hits = r.gh.hits.lock().unwrap().clone();
    assert!(hits.len() >= 2);
    assert!(
        hits.iter()
            .all(|(_, a)| a.as_deref() == Some(&format!("Bearer {SECRET}")[..])),
        "{hits:?}"
    );
    // a failing refresh: the error shown to the UI must not carry the token
    *r.gh.fail.lock().unwrap() = true;
    r.gh.publish("v0.3.0");
    let rep = r.fx.hub.refresh_releases(true).await;
    assert!(rep.error.is_some());
    let mut ui = {
        let id = enroll_as(&r.fx, DeviceKind::Ui, "phone").await;
        connect(&r.fx, "/ws/ui", Some(&id)).await.unwrap()
    };
    let mut frames = String::new();
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ui.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            frames += &t;
            if t.contains("release_cache") {
                break;
            }
        }
    }
    assert!(
        frames.contains("release_cache") && !frames.contains(SECRET),
        "{frames}"
    );
    assert!(!format!("{rep:?}").contains(SECRET));
    // nothing on disk mentions it
    for e in walk(&r.rel) {
        assert!(
            !String::from_utf8_lossy(&std::fs::read(&e).unwrap()).contains(SECRET),
            "{e:?}"
        );
    }
}

fn walk(d: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(d).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

#[tokio::test]
async fn check_update_refreshes_at_most_once_a_minute() {
    let r = rig(Some("v0.2.0")).await;
    assert_eq!(r.gh.latest_hits(), 1, "startup fetch");
    let id = enroll_as(&r.fx, DeviceKind::Ui, "phone").await;
    let mut ui = connect(&r.fx, "/ws/ui", Some(&id)).await.unwrap();
    for _ in 0..3 {
        send(
            &mut ui,
            &UiToHub::CheckUpdate {
                node: Some(id.id.clone()),
            },
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        r.gh.latest_hits(),
        1,
        "within 60 s of the last attempt: no new fetch"
    );
    // an admin force still fetches
    r.fx.hub.refresh_releases(true).await;
    assert_eq!(r.gh.latest_hits(), 2);
}

#[tokio::test]
async fn admin_releases_reports_and_refresh_fetches() {
    use ulak_remote::admin::{call, AdminRequest, AdminResponse};
    let r = rig(Some("v0.2.0")).await;
    let dir = std::env::temp_dir().join(format!("ulak-adm-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("admin.sock");
    let sock = r.fx.hub.serve_admin(&path, vec!["x".into()]).await.unwrap();
    let ask = |req| {
        let p = path.clone();
        async move {
            tokio::task::spawn_blocking(move || call(&p, req))
                .await
                .unwrap()
                .unwrap()
        }
    };
    match ask(AdminRequest::Releases).await {
        AdminResponse::Releases {
            versions,
            checked_at,
            error,
        } => {
            assert_eq!(versions, vec!["0.2.0"]);
            assert_ne!(checked_at, 0);
            assert_eq!(error, None);
        }
        o => panic!("{o:?}"),
    }
    r.gh.publish("v0.3.0");
    match ask(AdminRequest::ReleasesRefresh).await {
        AdminResponse::Releases { versions, .. } => assert_eq!(versions, vec!["0.3.0", "0.2.0"]),
        o => panic!("{o:?}"),
    }
    assert!(admin::render(&ask(AdminRequest::Releases).await, None).contains("0.3.0, 0.2.0"));
    sock.close();
}

#[tokio::test]
async fn bundled_release_seeds_the_cache_without_github() {
    let gh = fake().await;
    gh.publish("v0.5.0");
    *gh.fail.lock().unwrap() = true;
    let base = std::env::temp_dir().join(format!("ulak-bundle-{}", uuid::Uuid::now_v7()));
    let (bundle, rel) = (base.join("bundle"), base.join("rel"));
    std::fs::create_dir_all(bundle.join("0.5.0")).unwrap();
    for (n, b) in gh.files.lock().unwrap().iter() {
        std::fs::write(bundle.join("0.5.0").join(n), b).unwrap();
    }
    std::fs::write(bundle.join("0.5.0").join("meta.json"), "{}").unwrap();
    // a tampered bundled version never verifies, so it is dropped
    std::fs::create_dir_all(bundle.join("0.6.0")).unwrap();
    std::fs::write(bundle.join("0.6.0").join("meta.json"), "{}").unwrap();
    std::fs::write(bundle.join("0.6.0").join(TARGET), b"evil").unwrap();
    let fx = start(HubConfig {
        bundled_releases: Some(bundle.clone()),
        ..cfg(&gh, &rel, None)
    })
    .await;
    let rep = fx.hub.release_report();
    assert_eq!(rep.versions, vec!["0.5.0"], "{rep:?}");
    assert!(!rel.join("0.6.0").exists());
    // no token + bundle: GitHub is never asked, and no error is reported
    fx.hub.refresh_releases(true).await;
    assert_eq!(gh.latest_hits(), 0);
    assert_eq!(fx.hub.release_report().error, None);
}
