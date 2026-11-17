#![allow(dead_code)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};
use ulak_proto::*;
use ulak_remote::*;

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

async fn recv<T: serde::de::DeserializeOwned>(ws: &mut Ws) -> T {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return decode(&t).unwrap();
        }
    }
}

// Test knob: load1 x10 reported by the fake nodes (default 0.1).
static LOAD1: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn node_info(id: &NodeId, state: NodeState, agents: Vec<AgentId>) -> NodeInfo {
    NodeInfo::new(
        id.clone(),
        id.as_str().into(),
        state,
        Telemetry::new(
            LOAD1.load(std::sync::atomic::Ordering::Relaxed) as f32 / 10.0,
            5.0,
            None,
            30.0,
            now(),
        )
        .unwrap(),
        agents,
    )
}

async fn daemon(fx: &Fx, host: &str, state: NodeState, agents: Vec<AgentId>) -> (Identity, Ws) {
    let id = enroll_as(fx, DeviceKind::Node, host).await;
    let mut ws = connect(fx, "/ws/daemon", Some(&id)).await.unwrap();
    send(
        &mut ws,
        &DaemonToHub::Hello {
            node: node_info(&id.id, state, agents),
        },
    )
    .await;
    (id, ws)
}

async fn ui(fx: &Fx) -> Ws {
    let id = enroll_as(fx, DeviceKind::Ui, "phone").await;
    let mut ws = connect(fx, "/ws/ui", Some(&id)).await.unwrap();
    assert!(matches!(
        recv::<HubToUi>(&mut ws).await,
        HubToUi::Snapshot { .. }
    ));
    assert!(matches!(
        recv::<HubToUi>(&mut ws).await,
        HubToUi::Blockers { .. }
    ));
    assert!(matches!(
        recv::<HubToUi>(&mut ws).await,
        HubToUi::HubVersion(_)
    ));
    ws
}

fn agent(s: &str) -> AgentId {
    AgentId::new(s).unwrap()
}

fn spec(a: &AgentId) -> SpawnSpec {
    let p = AgentProfile::new("claude", vec![], None).unwrap();
    SpawnSpec::new(a.clone(), p, "/tmp".into(), "go".into(), Default::default()).unwrap()
}

async fn mcp(fx: &Fx, token: &AgentToken, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let (status, out) = https(
        fx,
        "POST",
        "/mcp",
        &[("Authorization", format!("Bearer {}", token.expose()))],
        &body,
    )
    .await;
    assert_eq!(status, 200, "{out}");
    serde_json::from_str(&out).unwrap()
}

async fn tool(fx: &Fx, token: &AgentToken, name: &str, args: Value) -> Value {
    let r = mcp(
        fx,
        token,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn fast() -> HubConfig {
    HubConfig {
        ask_timeout: Duration::from_millis(300),
        nudge_tick: Duration::from_millis(50),
        nudge_every_ms: 100,
        ..HubConfig::default()
    }
}

fn token(fx: &Fx, a: &AgentId) -> AgentToken {
    AgentTokens::issue(&**fx.hub.store(), a, 3_600_000, now()).unwrap()
}

async fn until<T>(u: &mut Ws, mut f: impl FnMut(HubToUi) -> Option<T>) -> T {
    loop {
        if let Some(v) = f(recv::<HubToUi>(u).await) {
            return v;
        }
    }
}

// ---- fake GitHub ----

const SECRET: &str = "ghp_SECRETTOKEN0123456789";

struct Req {
    path: String,
    headers: Vec<(String, String)>,
}

struct Fake {
    addr: std::net::SocketAddr,
    seen: Arc<std::sync::Mutex<Vec<Req>>>,
    limited: Arc<std::sync::atomic::AtomicBool>,
}

impl Fake {
    fn api(&self) -> String {
        format!("http://{}", self.addr)
    }
    fn paths(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.path.clone())
            .collect()
    }
}

fn param(path: &str, k: &str) -> usize {
    path.split(['?', '&'])
        .find_map(|p| p.strip_prefix(&format!("{k}=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

/// 135 repos `o/repo0..o/repo134`, paged like GitHub, plus /user and one repo with branches.
fn route(path: &str) -> (u16, Vec<(String, String)>, String) {
    let ok = |b: String| (200, vec![], b);
    if path == "/user" {
        return ok(json!({"login": "octo"}).to_string());
    }
    if path.starts_with("/user/repos?") {
        let (per, page) = (param(path, "per_page"), param(path, "page"));
        let all: Vec<Value> = (0..135)
            .map(|i| {
                json!({"full_name": format!("o/repo{i}"), "private": i % 2 == 0,
                "default_branch": "main", "pushed_at": "2024-05-01T12:00:00Z", "description": "d"})
            })
            .collect();
        let chunk: Vec<_> = all
            .iter()
            .skip((page - 1) * per)
            .take(per)
            .cloned()
            .collect();
        let mut h = vec![];
        if page * per < all.len() {
            h.push(("Link".into(), "<https://x/next>; rel=\"next\"".into()));
        }
        return (200, h, Value::Array(chunk).to_string());
    }
    if path == "/repos/o/n" {
        return ok(json!({"default_branch": "trunk"}).to_string());
    }
    if path == "/repos/o/n/branches?per_page=100" {
        return ok(json!([{"name": "trunk"}, {"name": "dev"}]).to_string());
    }
    (404, vec![], "{}".into())
}

fn fake_github() -> Fake {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let f = Fake {
        addr: l.local_addr().unwrap(),
        seen: Arc::default(),
        limited: Arc::default(),
    };
    let (seen, limited) = (f.seen.clone(), f.limited.clone());
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = Vec::new();
            let mut b = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&b[..n]),
                }
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            let mut lines = text.lines();
            let path = lines
                .next()
                .unwrap_or("")
                .split(' ')
                .nth(1)
                .unwrap_or("")
                .to_owned();
            let headers = lines
                .take_while(|l| !l.is_empty())
                .filter_map(|l| l.split_once(": "))
                .map(|(k, v)| (k.to_lowercase(), v.to_owned()))
                .collect();
            seen.lock().unwrap().push(Req {
                path: path.clone(),
                headers,
            });
            let (code, extra, body) = if limited.load(std::sync::atomic::Ordering::Relaxed) {
                (
                    403,
                    vec![("x-ratelimit-remaining".into(), "0".into())],
                    "{}".into(),
                )
            } else {
                route(&path)
            };
            let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    f
}

fn cfg_in(dir: &std::path::Path, f: &Fake) -> HubConfig {
    HubConfig {
        github_api: f.api(),
        secrets_key_path: Some(dir.join("secrets.key")),
        queue_tick: Duration::from_millis(50),
        ..fast()
    }
}

fn new_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ulak-repo-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A hub whose db, CA and key live in `dir` (so it can be restarted).
async fn start_dir(dir: &std::path::Path, f: &Fake) -> Fx {
    let store = Arc::new(Store::open(&dir.join("hub.db")).unwrap());
    start_in(dir, store, cfg_in(dir, f)).await
}

async fn start_fake() -> (Fx, Fake, std::path::PathBuf) {
    let (f, d) = (fake_github(), new_dir());
    (start_dir(&d, &f).await, f, d)
}

async fn add_cred(u: &mut Ws) -> CredentialId {
    send(
        u,
        &UiToHub::AddCredential {
            name: "work".into(),
            token: GitToken::new(SECRET).unwrap(),
        },
    )
    .await;
    until(u, |m| match m {
        HubToUi::Credentials { credentials } => Some(credentials[0].id.clone()),
        HubToUi::Error { message } => panic!("{message}"),
        _ => None,
    })
    .await
}

fn repo_spec(a: &str, cred: &CredentialId) -> SpawnSpec {
    let p = AgentProfile::new("claude", vec![], None).unwrap();
    let src = Source::Repo {
        repo: RepoRef::new("o/n").unwrap(),
        branch: None,
        credential: cred.clone(),
        allow_push: false,
    };
    SpawnSpec::new_repo(agent(a), p, src, "go".into(), Default::default()).unwrap()
}

fn busy() -> NodeState {
    NodeState::Busy(BusyReason::Gaming {
        process: "game".into(),
    })
}

async fn hello(d: &mut Ws, id: &Identity, state: NodeState) {
    send(
        d,
        &DaemonToHub::Hello {
            node: node_info(&id.id, state, vec![]),
        },
    )
    .await;
}

/// Frames that arrive within `ms`.
async fn drain(u: &mut Ws, ms: u64) -> Vec<HubToUi> {
    let mut out = vec![];
    while let Ok(m) = tokio::time::timeout(Duration::from_millis(ms), async {
        recv::<HubToUi>(u).await
    })
    .await
    {
        out.push(m);
    }
    out
}

async fn queue_of(u: &mut Ws) -> Vec<QueuedSpawn> {
    send(u, &UiToHub::ListQueue).await;
    until(u, |m| match m {
        HubToUi::Queue { items } => Some(items),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn credential_is_validated_encrypted_and_never_echoed() {
    let (fx, f, dir) = start_fake().await;
    let (mut u, mut u2) = (ui(&fx).await, ui(&fx).await);
    let id = add_cred(&mut u).await;
    {
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen[0].path, "/user");
        let auth = &seen.iter().find(|r| r.path == "/user").unwrap().headers;
        assert!(auth.contains(&("authorization".into(), format!("Bearer {SECRET}"))));
        assert!(auth
            .iter()
            .any(|(k, v)| k == "x-github-api-version" && v == "2022-11-28"));
        assert!(seen.iter().all(|r| !r.path.contains(SECRET)));
    }
    send(&mut u2, &UiToHub::ListCredentials).await;
    let frames = {
        let mut v = drain(&mut u2, 300).await;
        v.extend(drain(&mut u, 100).await);
        v
    };
    assert!(frames.iter().any(|m| matches!(m, HubToUi::Credentials { credentials } if credentials[0].login == "octo" && credentials[0].id == id)));
    assert!(frames.iter().all(|m| !encode(m).unwrap().contains(SECRET)));
    // Neither the db (nor its WAL) nor anything else on disk holds the token in the clear.
    for name in ["hub.db", "hub.db-wal"] {
        let bytes = std::fs::read(dir.join(name)).unwrap_or_default();
        assert!(
            !bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
            "{name}"
        );
    }
    use std::os::unix::fs::PermissionsExt;
    let key = std::fs::metadata(dir.join("secrets.key")).unwrap();
    assert_eq!((key.len(), key.permissions().mode() & 0o777), (32, 0o600));
    // A failing GitHub call yields an Error without the token.
    f.limited.store(true, std::sync::atomic::Ordering::Relaxed);
    send(
        &mut u,
        &UiToHub::AddCredential {
            name: "x".into(),
            token: GitToken::new(SECRET).unwrap(),
        },
    )
    .await;
    let e = next_error(&mut u).await;
    assert!(e.contains("rate limit") && !e.contains(SECRET), "{e}");
    send(&mut u, &UiToHub::DeleteCredential { id }).await;
    until(&mut u, |m| match m {
        HubToUi::Credentials { credentials } if credentials.is_empty() => Some(()),
        _ => None,
    })
    .await;
}

#[tokio::test]
async fn key_file_mode_and_api_url_are_enforced() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let (f, dir) = (fake_github(), new_dir());
    let build = |cfg: HubConfig| {
        let ca = Ca::load_or_create(&dir).unwrap();
        Hub::new(
            Arc::new(Store::open_in_memory().unwrap()),
            ca,
            &["localhost".into()],
            cfg,
        )
        .map(|_| ())
    };
    let key = dir.join("secrets.key");
    std::fs::write(&key, [7u8; 32]).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(build(cfg_in(&dir, &f)).is_err());
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(build(cfg_in(&dir, &f)).is_ok());
    let link = dir.join("link.key");
    symlink(&key, &link).unwrap();
    let mut c = cfg_in(&dir, &f);
    c.secrets_key_path = Some(link);
    assert!(build(c).is_err());
    for (api, ok) in [
        ("https://ghe.example.com/api/v3", true),
        ("http://127.0.0.1:9", true),
        ("http://[::1]:9", true),
        ("http://example.com", false),
        ("ftp://127.0.0.1", false),
        ("http://127.0.0.1@evil.com", false),
    ] {
        let mut c = cfg_in(&dir, &f);
        c.github_api = api.into();
        assert_eq!(build(c).is_ok(), ok, "{api}");
    }
}

#[tokio::test]
async fn repos_and_branches_are_listed_with_pagination_and_filter() {
    let (fx, f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    let c = add_cred(&mut u).await;
    let list = |c: &CredentialId, q: Option<&str>, page| UiToHub::ListRepos {
        credential: c.clone(),
        query: q.map(Into::into),
        page,
    };
    let repos = |m| match m {
        HubToUi::Repos {
            repos, has_more, ..
        } => Some((repos, has_more)),
        _ => None,
    };
    send(&mut u, &list(&c, None, 2)).await;
    let (r, more) = until(&mut u, repos).await;
    assert_eq!(
        (r.len(), more, r[0].full_name.as_str()),
        (30, true, "o/repo30")
    );
    assert_eq!(r[0].pushed_at_ms, 1_714_564_800_000);
    assert!(f.paths().iter().any(|p| p == "/user/repos?per_page=30&sort=pushed&affiliation=owner,collaborator,organization_member&page=2"));
    // 46 names contain "REPO1"; scanned in pages of 100, sliced by 30.
    send(&mut u, &list(&c, Some("REPO1"), 1)).await;
    assert!(until(&mut u, repos).await.1);
    send(&mut u, &list(&c, Some("REPO1"), 2)).await;
    let (r, more) = until(&mut u, repos).await;
    assert_eq!((r.len(), more), (16, false));
    send(
        &mut u,
        &UiToHub::ListBranches {
            credential: c.clone(),
            repo: RepoRef::new("o/n").unwrap(),
        },
    )
    .await;
    let (b, d) = until(&mut u, |m| match m {
        HubToUi::Branches {
            branches,
            default_branch,
            ..
        } => Some((branches, default_branch)),
        _ => None,
    })
    .await;
    assert_eq!(
        (b, d.as_str()),
        (vec!["trunk".to_owned(), "dev".to_owned()], "trunk")
    );
    f.limited.store(true, std::sync::atomic::Ordering::Relaxed);
    send(&mut u, &list(&c, None, 1)).await;
    assert!(next_error(&mut u).await.contains("rate limit"));
    // Unknown credential.
    send(&mut u, &list(&CredentialId::new("nope").unwrap(), None, 1)).await;
    assert!(!next_error(&mut u).await.is_empty());
}

#[tokio::test]
async fn repo_spawn_sends_git_access_then_spawn_and_nothing_sensitive_to_uis() {
    let (fx, _f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    let c = add_cred(&mut u).await;
    let (_id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![]).await;
    let spec = repo_spec("r-1", &c);
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: spec.clone(),
        },
    )
    .await;
    match recv::<HubToDaemon>(&mut d).await {
        HubToDaemon::GitAccess { agent: a, token } => {
            assert_eq!((a, token.expose()), (agent("r-1"), SECRET))
        }
        other => panic!("{other:?}"),
    }
    match recv::<HubToDaemon>(&mut d).await {
        HubToDaemon::Spawn(s) => {
            assert_eq!(s.source(), spec.source());
            assert!(s.workdir().as_os_str().is_empty());
            assert!(s.env().contains_key("ULAK_AGENT_TOKEN"));
            assert!(!serde_json::to_string(&s).unwrap().contains(SECRET));
        }
        other => panic!("{other:?}"),
    }
    let frames = drain(&mut u, 300).await;
    assert!(frames.iter().all(|m| !encode(m).unwrap().contains(SECRET)));
    assert!(!frames
        .iter()
        .any(|m| matches!(m, HubToUi::QueueChanged { .. })));
    // A spec naming an unknown credential is an immediate error, never queued.
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: repo_spec("r-2", &CredentialId::new("gone").unwrap()),
        },
    )
    .await;
    assert!(!next_error(&mut u).await.is_empty());
    assert!(queue_of(&mut u).await.is_empty());
    // An empty workdir without a source is refused.
    let p = AgentProfile::new("claude", vec![], None).unwrap();
    let bad = SpawnSpec::new(agent("r-3"), p, "".into(), "go".into(), Default::default()).unwrap();
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: bad,
        },
    )
    .await;
    assert!(!next_error(&mut u).await.is_empty());
}

#[tokio::test]
async fn queued_until_a_node_turns_idle_then_placed_fifo() {
    let (fx, _f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    let (id, mut d) = daemon(&fx, "box", busy(), vec![]).await;
    for a in ["q-1", "q-2"] {
        send(
            &mut u,
            &UiToHub::Spawn {
                target: SpawnTarget::Auto,
                spec: spec(&agent(a)),
            },
        )
        .await;
    }
    let mut ids = vec![];
    while ids.len() < 2 {
        if let HubToUi::QueueChanged { item } = recv::<HubToUi>(&mut u).await {
            assert_eq!(item.state, QueueState::Queued);
            ids.push(item.id);
        }
    }
    // Duplicate agent id among queued items.
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: spec(&agent("q-1")),
        },
    )
    .await;
    assert!(next_error(&mut u).await.contains("q-1"));
    hello(&mut d, &id, NodeState::Idle).await;
    for a in ["q-1", "q-2"] {
        match recv::<HubToDaemon>(&mut d).await {
            HubToDaemon::Spawn(s) => assert_eq!(s.agent_id(), &agent(a)),
            other => panic!("{other:?}"),
        }
    }
    let mut gone = vec![];
    while gone.len() < 2 {
        if let HubToUi::QueueRemoved { id } = recv::<HubToUi>(&mut u).await {
            gone.push(id);
        }
    }
    assert_eq!(gone, ids);
    assert!(queue_of(&mut u).await.is_empty());
}

#[tokio::test]
async fn explicit_node_is_pinned_and_cancel_removes() {
    let (fx, _f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    let (b, mut bw) = daemon(&fx, "bee", busy(), vec![]).await;
    let (_i, mut iw) = daemon(&fx, "eye", NodeState::Idle, vec![]).await;
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Node(b.id.clone()),
            spec: spec(&agent("p-1")),
        },
    )
    .await;
    let item = until(&mut u, |m| match m {
        HubToUi::QueueChanged { item } => Some(item),
        _ => None,
    })
    .await;
    assert_eq!(item.target, SpawnTarget::Node(b.id.clone()));
    assert!(tokio::time::timeout(Duration::from_millis(300), iw.next())
        .await
        .is_err());
    hello(&mut bw, &b, NodeState::Idle).await;
    assert!(matches!(
        recv::<HubToDaemon>(&mut bw).await,
        HubToDaemon::Spawn(_)
    ));
    // Cancel: queue a pinned item for a busy node, then remove it.
    let (c, _cw) = daemon(&fx, "cee", busy(), vec![]).await;
    let busy_node = c.id.clone();
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Node(busy_node),
            spec: spec(&agent("p-3")),
        },
    )
    .await;
    let q = until(&mut u, |m| match m {
        HubToUi::QueueChanged { item } if item.agent == agent("p-3") => Some(item),
        _ => None,
    })
    .await;
    send(&mut u, &UiToHub::CancelQueued { id: q.id.clone() }).await;
    until(&mut u, |m| {
        matches!(m, HubToUi::QueueRemoved { id } if id == q.id).then_some(())
    })
    .await;
    assert!(queue_of(&mut u).await.is_empty());
    send(&mut u, &UiToHub::CancelQueued { id: q.id }).await;
    assert!(!next_error(&mut u).await.is_empty());
}

#[tokio::test]
async fn queue_survives_a_hub_restart_and_is_replayed_on_connect() {
    let (fx, f, dir) = start_fake().await;
    let mut u = ui(&fx).await;
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: spec(&agent("s-1")),
        },
    )
    .await;
    let item = until(&mut u, |m| match m {
        HubToUi::QueueChanged { item } => Some(item),
        _ => None,
    })
    .await;
    fx.run.abort();
    drop(u);
    drop(fx);
    let fx2 = start_dir(&dir, &f).await;
    // `ui()` takes the first three frames; Queue follows the version replays.
    let mut u2 = ui(&fx2).await;
    let items = until(&mut u2, |m| match m {
        HubToUi::Queue { items } => Some(items),
        _ => None,
    })
    .await;
    assert_eq!(items, vec![item]);
}

#[tokio::test]
async fn failed_placement_stays_listed_and_is_not_sent() {
    let (fx, _f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    let c = add_cred(&mut u).await;
    let (id, mut d) = daemon(&fx, "box", busy(), vec![]).await;
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: repo_spec("f-1", &c),
        },
    )
    .await;
    until(&mut u, |m| {
        matches!(m, HubToUi::QueueChanged { .. }).then_some(())
    })
    .await;
    send(&mut u, &UiToHub::DeleteCredential { id: c }).await;
    hello(&mut d, &id, NodeState::Idle).await;
    let failed = until(&mut u, |m| match m {
        HubToUi::QueueChanged { item } if matches!(item.state, QueueState::Failed(_)) => Some(item),
        _ => None,
    })
    .await;
    assert_eq!(queue_of(&mut u).await, vec![failed]);
    assert!(tokio::time::timeout(Duration::from_millis(300), d.next())
        .await
        .is_err());
}

#[tokio::test]
async fn agent_statuses_are_replayed_after_reconnect() {
    let (fx, _f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    // Nothing known yet: no AgentStatuses frame (a fresh connect only gets the base frames).
    let (id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![agent("a-1")]).await;
    send(
        &mut d,
        &DaemonToHub::AgentStatus {
            agent: agent("a-1"),
            status: AgentStatus::Running,
        },
    )
    .await;
    until(&mut u, |m| {
        matches!(m, HubToUi::AgentStatus { .. }).then_some(())
    })
    .await;
    let mut u2 = ui(&fx).await;
    let st = until(&mut u2, |m| match m {
        HubToUi::AgentStatuses { statuses } => Some(statuses),
        _ => None,
    })
    .await;
    assert_eq!(st, vec![(agent("a-1"), id.id, AgentStatus::Running)]);
}
async fn next_error(u: &mut Ws) -> String {
    loop {
        if let HubToUi::Error { message } = recv::<HubToUi>(u).await {
            return message;
        }
    }
}

#[test]
fn queue_cap_and_known_agent_are_rejected() {
    let s = Store::open_in_memory().unwrap();
    for i in 0..QUEUE_CAP {
        s.enqueue(spec(&agent(&format!("c-{i}"))), SpawnTarget::Auto, &[], 1)
            .unwrap();
    }
    let over = s.enqueue(spec(&agent("c-over")), SpawnTarget::Auto, &[], 1);
    assert!(matches!(over, Err(QueueError::Full(_))));
    let s = Store::open_in_memory().unwrap();
    let known = [agent("k-1")];
    let dup = s.enqueue(spec(&known[0]), SpawnTarget::Auto, &known, 1);
    assert!(matches!(dup, Err(QueueError::DuplicateAgent(_))));
}

#[tokio::test]
async fn attach_is_routed_for_a_running_agent_not_yet_in_the_node_list() {
    let (fx, _f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    let (_id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![]).await;
    send(
        &mut d,
        &DaemonToHub::AgentStatus {
            agent: agent("a-1"),
            status: AgentStatus::Running,
        },
    )
    .await;
    until(&mut u, |m| {
        matches!(m, HubToUi::AgentStatus { .. }).then_some(())
    })
    .await;
    send(
        &mut u,
        &UiToHub::Attach {
            agent: agent("a-1"),
        },
    )
    .await;
    assert!(matches!(
        recv::<HubToDaemon>(&mut d).await,
        HubToDaemon::Attach { .. }
    ));
    send(
        &mut u,
        &UiToHub::Attach {
            agent: agent("nope"),
        },
    )
    .await;
    assert!(next_error(&mut u).await.contains("nope"));
}

#[tokio::test]
async fn agent_failed_is_relayed_with_the_daemons_node_replayed_and_not_forgeable() {
    let (fx, _f, _d) = start_fake().await;
    let mut u = ui(&fx).await;
    let (id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![agent("a-1")]).await;
    send(
        &mut d,
        &DaemonToHub::AgentFailed {
            agent: agent("a-1"),
            reason: "clone\u{1b}[31m failed".into(),
        },
    )
    .await;
    send(
        &mut d,
        &DaemonToHub::AgentStatus {
            agent: agent("a-1"),
            status: AgentStatus::Failed,
        },
    )
    .await;
    let (node, reason) = until(&mut u, |m| match m {
        HubToUi::AgentFailed { node, reason, .. } => Some((node, reason)),
        _ => None,
    })
    .await;
    assert_eq!(node, id.id);
    assert_eq!(
        reason, "clone[31m failed",
        "control characters are stripped"
    );
    // a reconnecting UI gets AgentStatuses, then the replay
    let mut u2 = ui(&fx).await;
    until(&mut u2, |m| {
        matches!(m, HubToUi::AgentStatuses { .. }).then_some(())
    })
    .await;
    match recv::<HubToUi>(&mut u2).await {
        HubToUi::AgentFailed { agent: a, node, .. } => {
            assert_eq!((a, node), (agent("a-1"), id.id.clone()))
        }
        other => panic!("expected the replay, got {other:?}"),
    }
    // a UI cannot forge it: the frame is not a UiToHub variant, and nobody sees it
    let forged = r#"{"type":"agent_failed","agent":"a-1","node":"evil","reason":"x"}"#;
    assert!(decode::<UiToHub>(forged).is_err());
    u.send(Message::Text(forged.into())).await.unwrap();
    let seen = drain(&mut u2, 300).await;
    assert!(!seen
        .iter()
        .any(|m| matches!(m, HubToUi::AgentFailed { .. })));
    // a later non-failed status drops the remembered reason
    send(
        &mut d,
        &DaemonToHub::AgentStatus {
            agent: agent("a-1"),
            status: AgentStatus::Running,
        },
    )
    .await;
    until(&mut u2, |m| {
        matches!(m, HubToUi::AgentStatus { .. }).then_some(())
    })
    .await;
    let mut u3 = ui(&fx).await;
    let seen = drain(&mut u3, 300).await;
    assert!(!seen
        .iter()
        .any(|m| matches!(m, HubToUi::AgentFailed { .. })));
}
