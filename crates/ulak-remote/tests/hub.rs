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
    let ca = Ca::load_or_create(&dir).unwrap();
    let ca_pem = ca.cert_pem().to_owned();
    let store = Arc::new(Store::open_in_memory().unwrap());
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

#[tokio::test]
async fn enrolled_daemon_connects_and_ui_sees_it() {
    let fx = start(fast()).await;
    let mut u = ui(&fx).await;
    let (id, _d) = daemon(&fx, "Box One", NodeState::Idle, vec![]).await;
    match recv::<HubToUi>(&mut u).await {
        HubToUi::NodeUpdate(n) => assert_eq!(n.id, id.id),
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.hub.registry().snapshot().len(), 1);
}

#[tokio::test]
async fn no_cert_or_wrong_kind_is_rejected() {
    let fx = start(fast()).await;
    assert!(connect(&fx, "/ws/daemon", None).await.is_err());
    assert!(connect(&fx, "/ws/ui", None).await.is_err());
    let ui_id = enroll_as(&fx, DeviceKind::Ui, "phone").await;
    assert!(connect(&fx, "/ws/daemon", Some(&ui_id)).await.is_err());
    let node_id = enroll_as(&fx, DeviceKind::Node, "box").await;
    assert!(connect(&fx, "/ws/ui", Some(&node_id)).await.is_err());
}

#[tokio::test]
async fn revoked_device_is_rejected() {
    let fx = start(fast()).await;
    let id = enroll_as(&fx, DeviceKind::Node, "box").await;
    assert!(connect(&fx, "/ws/daemon", Some(&id)).await.is_ok());
    Devices::revoke(&**fx.hub.store(), &id.id, now()).unwrap();
    assert!(connect(&fx, "/ws/daemon", Some(&id)).await.is_err());
}

#[tokio::test]
async fn enroll_is_rate_limited_and_rejects_bad_codes() {
    let fx = start(HubConfig {
        enroll_per_min: 2,
        ..fast()
    })
    .await;
    let key = rcgen::KeyPair::generate().unwrap().public_key_pem();
    let req = EnrollRequest::new(Otp::new("AAAAAAAA").unwrap(), key, "x".into()).unwrap();
    let body = encode(&req).unwrap();
    assert_eq!(https(&fx, "POST", "/enroll", &[], &body).await.0, 403);
    assert_eq!(https(&fx, "POST", "/enroll", &[], &body).await.0, 403);
    assert_eq!(https(&fx, "POST", "/enroll", &[], &body).await.0, 429);
}

#[tokio::test]
async fn spawn_goes_only_to_an_idle_node_with_token_env() {
    let fx = start(fast()).await;
    let mut u = ui(&fx).await;
    let busy = NodeState::Busy(BusyReason::Gaming {
        process: "game".into(),
    });
    let (busy_id, mut busy_ws) = daemon(&fx, "gamer", busy, vec![]).await;
    let (_idle_id, mut idle_ws) = daemon(&fx, "idler", NodeState::Idle, vec![]).await;
    let a = agent("a-1");
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: spec(&a),
        },
    )
    .await;
    match recv::<HubToDaemon>(&mut idle_ws).await {
        HubToDaemon::Spawn(s) => {
            let t = s.env().get("ULAK_AGENT_TOKEN").unwrap();
            let verified = AgentTokens::verify(
                &**fx.hub.store(),
                &AgentToken::new(t.clone()).unwrap(),
                now(),
            )
            .unwrap();
            assert_eq!(verified, a);
            assert!(s.env()["ULAK_MCP_URL"].ends_with("/mcp"));
        }
        other => panic!("{other:?}"),
    }
    // Explicitly targeting the busy node fails and nothing reaches it.
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Node(busy_id.id),
            spec: spec(&agent("a-2")),
        },
    )
    .await;
    loop {
        match recv::<HubToUi>(&mut u).await {
            HubToUi::Error { .. } => break,
            _ => continue,
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), busy_ws.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn mcp_requires_a_valid_bearer_token() {
    let fx = start(fast()).await;
    let (s, _) = https(&fx, "POST", "/mcp", &[], "{}").await;
    assert_eq!(s, 401);
    let (s, _) = https(
        &fx,
        "POST",
        "/mcp",
        &[("Authorization", "Bearer nope".into())],
        "{}",
    )
    .await;
    assert_eq!(s, 401);
    let t = token(&fx, &agent("a-1"));
    let r = mcp(&fx, &t, "tools/list", json!({})).await;
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn mcp_messages_use_the_token_identity() {
    let fx = start(fast()).await;
    let (a, b) = (agent("a-1"), agent("b-1"));
    let (ta, tb) = (token(&fx, &a), token(&fx, &b));
    let mut u = ui(&fx).await;
    tool(
        &fx,
        &ta,
        "send_message",
        json!({"to": "b-1", "body": "hi", "from": "b-1"}),
    )
    .await;
    assert!(matches!(
        recv::<HubToUi>(&mut u).await,
        HubToUi::MessageLogged(_)
    ));
    let inbox = tool(&fx, &tb, "read_inbox", json!({})).await;
    assert_eq!(inbox[0]["from"], "a-1");
    assert_eq!(inbox[0]["body"], "hi");
    assert_eq!(tool(&fx, &tb, "read_inbox", json!({})).await, json!([]));
    assert_eq!(tool(&fx, &ta, "read_inbox", json!({})).await, json!([]));
}

async fn answer_when_opened(u: &mut Ws, text: &str) -> QuestionId {
    loop {
        if let HubToUi::QuestionOpened(q) = recv::<HubToUi>(u).await {
            send(
                u,
                &UiToHub::Answer(Answer::new(q.id().clone(), text.into()).unwrap()),
            )
            .await;
            return q.id().clone();
        }
    }
}

#[tokio::test]
async fn ask_human_answered_in_time_returns_directly() {
    let fx = start(HubConfig {
        ask_timeout: Duration::from_secs(10),
        ..fast()
    })
    .await;
    let t = token(&fx, &agent("a-1"));
    let mut u = ui(&fx).await;
    let ui_task = tokio::spawn(async move { answer_when_opened(&mut u, "yes").await });
    let r = tool(&fx, &t, "ask_human", json!({"question": "ok?"})).await;
    assert_eq!(r["status"], "answered");
    assert_eq!(r["answer"], "yes");
    ui_task.await.unwrap();
    // Collected, so no nudge ever follows.
    assert!(
        Questions::due_nudges(&**fx.hub.store(), now() + 10_000, 0, 3)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn timeout_then_nudge_then_check_answer() {
    let fx = start(fast()).await;
    let a = agent("a-1");
    let t = token(&fx, &a);
    let (_id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![a.clone()]).await;
    let mut u = ui(&fx).await;

    let r = tool(&fx, &t, "ask_human", json!({"question": "ok?"})).await;
    assert_eq!(r["status"], "pending");
    let qid = r["question_id"].as_str().unwrap().to_owned();

    // Another agent cannot read it.
    let other = token(&fx, &agent("b-1"));
    let r = mcp(
        &fx,
        &other,
        "tools/call",
        json!({"name": "check_answer", "arguments": {"question_id": qid}}),
    )
    .await;
    assert_eq!(r["result"]["isError"], true);

    // Not answered yet: check_answer says pending and no nudge arrives.
    assert_eq!(
        tool(&fx, &t, "check_answer", json!({"question_id": qid})).await["status"],
        "pending"
    );
    assert!(tokio::time::timeout(Duration::from_millis(250), d.next())
        .await
        .is_err());

    let answered = answer_when_opened_pending(&mut u, &qid, "SECRET-ANSWER").await;
    assert_eq!(answered, qid);
    match recv::<HubToDaemon>(&mut d).await {
        HubToDaemon::Nudge { agent, text } => {
            assert_eq!(agent, a);
            assert!(text.contains(&qid));
            assert!(!text.contains("SECRET-ANSWER"));
        }
        other => panic!("{other:?}"),
    }
    let r = tool(&fx, &t, "check_answer", json!({"question_id": qid})).await;
    assert_eq!(r["answer"], "SECRET-ANSWER");
}

async fn answer_when_opened_pending(u: &mut Ws, qid: &str, text: &str) -> String {
    send(
        u,
        &UiToHub::Answer(Answer::new(QuestionId::new(qid).unwrap(), text.into()).unwrap()),
    )
    .await;
    loop {
        if let HubToUi::QuestionClosed { id } = recv::<HubToUi>(u).await {
            return id.as_str().to_owned();
        }
    }
}

#[tokio::test]
async fn server_chain_includes_the_ca() {
    let fx = start(fast()).await;
    let tcp = tokio::net::TcpStream::connect(fx.run.local_addr())
        .await
        .unwrap();
    let s = TlsConnector::from(Arc::new(tls(&fx.ca_pem, None)))
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
        .unwrap();
    let chain = s.get_ref().1.peer_certificates().unwrap();
    let ca = rustls_pemfile::certs(&mut fx.ca_pem.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[1].as_ref(), ca.as_ref());
}

async fn closed(ws: &mut Ws) -> bool {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Err(_) => return false,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return true,
            Ok(Some(Ok(_))) => {}
        }
    }
}

#[tokio::test]
async fn revoking_a_device_closes_its_open_sockets() {
    let fx = start(fast()).await;
    let (d, mut dws) = daemon(&fx, "box", NodeState::Idle, vec![]).await;
    let uid = enroll_as(&fx, DeviceKind::Ui, "phone").await;
    let mut uws = connect(&fx, "/ws/ui", Some(&uid)).await.unwrap();
    assert!(matches!(
        recv::<HubToUi>(&mut uws).await,
        HubToUi::Snapshot { .. }
    ));
    fx.hub.revoke_device(&uid.id, now()).unwrap();
    assert!(closed(&mut uws).await);
    // The other device is untouched until it is revoked too.
    send(
        &mut dws,
        &DaemonToHub::Heartbeat(Telemetry::new(0.1, 5.0, None, 30.0, now()).unwrap()),
    )
    .await;
    fx.hub.revoke_device(&d.id, now()).unwrap();
    assert!(closed(&mut dws).await);
}

#[tokio::test]
async fn pane_output_goes_only_to_attached_ui() {
    let fx = start(fast()).await;
    let mut u1 = ui(&fx).await;
    let mut u2 = ui(&fx).await;
    let a = agent("a-1");
    let (_id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![a.clone()]).await;
    // NodeUpdate means the hub has registered the agent.
    loop {
        if matches!(recv::<HubToUi>(&mut u1).await, HubToUi::NodeUpdate(_)) {
            break;
        }
    }
    send(&mut u1, &UiToHub::Attach { agent: a.clone() }).await;
    assert!(matches!(
        recv::<HubToDaemon>(&mut d).await,
        HubToDaemon::Attach { .. }
    ));
    let out = DaemonToHub::PaneOutput {
        agent: a.clone(),
        data: b"hi".to_vec(),
    };
    send(&mut d, &out).await;
    loop {
        if matches!(recv::<HubToUi>(&mut u1).await, HubToUi::PaneOutput { .. }) {
            break;
        }
    }
    // u2 saw the NodeUpdate broadcast but never the pane bytes.
    while let Ok(Some(Ok(Message::Text(t)))) =
        tokio::time::timeout(Duration::from_millis(300), u2.next()).await
    {
        assert!(!matches!(
            decode::<HubToUi>(&t).unwrap(),
            HubToUi::PaneOutput { .. }
        ));
    }
    // Detach tells the daemon, and later output reaches nobody.
    send(&mut u1, &UiToHub::Detach { agent: a.clone() }).await;
    assert!(matches!(
        recv::<HubToDaemon>(&mut d).await,
        HubToDaemon::Detach { .. }
    ));
    send(&mut d, &out).await;
    while let Ok(Some(Ok(Message::Text(t)))) =
        tokio::time::timeout(Duration::from_millis(300), u1.next()).await
    {
        assert!(!matches!(
            decode::<HubToUi>(&t).unwrap(),
            HubToUi::PaneOutput { .. }
        ));
    }
}

#[tokio::test]
async fn ui_disconnect_detaches_from_the_daemon() {
    let fx = start(fast()).await;
    let mut u = ui(&fx).await;
    let a = agent("a-1");
    let (_id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![a.clone()]).await;
    loop {
        if matches!(recv::<HubToUi>(&mut u).await, HubToUi::NodeUpdate(_)) {
            break;
        }
    }
    send(&mut u, &UiToHub::Attach { agent: a }).await;
    assert!(matches!(
        recv::<HubToDaemon>(&mut d).await,
        HubToDaemon::Attach { .. }
    ));
    drop(u);
    assert!(matches!(
        recv::<HubToDaemon>(&mut d).await,
        HubToDaemon::Detach { .. }
    ));
}

/// Reads UI frames until one matches.
async fn until<T>(u: &mut Ws, mut f: impl FnMut(HubToUi) -> Option<T>) -> T {
    loop {
        if let Some(v) = f(recv::<HubToUi>(u).await) {
            return v;
        }
    }
}

#[tokio::test]
async fn agent_status_is_relayed_to_all_uis() {
    let fx = start(fast()).await;
    let (mut u1, mut u2) = (ui(&fx).await, ui(&fx).await);
    let a = agent("a-1");
    let (id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![a.clone()]).await;
    send(
        &mut d,
        &DaemonToHub::AgentStatus {
            agent: a.clone(),
            status: AgentStatus::Running,
        },
    )
    .await;
    for u in [&mut u1, &mut u2] {
        let (ag, node, st) = until(u, |m| match m {
            HubToUi::AgentStatus {
                agent,
                node,
                status,
            } => Some((agent, node, status)),
            _ => None,
        })
        .await;
        assert_eq!(
            (ag, node, st),
            (a.clone(), id.id.clone(), AgentStatus::Running)
        );
    }
}

#[tokio::test]
async fn kill_reaches_the_node_and_revokes_tokens() {
    let fx = start(fast()).await;
    let mut u = ui(&fx).await;
    let a = agent("a-1");
    let tok = token(&fx, &a);
    let (_id, mut d) = daemon(&fx, "box", NodeState::Idle, vec![a.clone()]).await;
    until(&mut u, |m| {
        matches!(m, HubToUi::NodeUpdate(_)).then_some(())
    })
    .await;
    send(&mut u, &UiToHub::Kill { agent: a.clone() }).await;
    assert_eq!(
        recv::<HubToDaemon>(&mut d).await,
        HubToDaemon::Kill { agent: a.clone() }
    );
    assert!(AgentTokens::verify(&**fx.hub.store(), &tok, now()).is_err());
    // Unknown agent: an error frame, nothing else.
    send(
        &mut u,
        &UiToHub::Kill {
            agent: agent("zz-9"),
        },
    )
    .await;
    until(&mut u, |m| matches!(m, HubToUi::Error { .. }).then_some(())).await;
}

#[tokio::test]
async fn add_device_offers_an_otp_that_enrolls_the_kind() {
    let fx = start(fast()).await;
    let mut u = ui(&fx).await;
    send(
        &mut u,
        &UiToHub::AddDevice {
            kind: DeviceKind::Browser,
        },
    )
    .await;
    let (kind, otp, fp, exp) = until(&mut u, |m| match m {
        HubToUi::OtpOffer {
            kind,
            otp,
            hub_fingerprint,
            expires_ms,
        } => Some((kind, otp, hub_fingerprint, expires_ms)),
        _ => None,
    })
    .await;
    assert_eq!(kind, DeviceKind::Browser);
    assert!(!fp.is_empty() && exp > now());
    assert_eq!(
        fx.hub.enroller().consume(&otp, now()).unwrap(),
        DeviceKind::Browser
    );
}

#[tokio::test]
async fn list_and_revoke_devices() {
    let fx = start(fast()).await;
    let uid = enroll_as(&fx, DeviceKind::Ui, "phone").await;
    let mut u = connect(&fx, "/ws/ui", Some(&uid)).await.unwrap();
    recv::<HubToUi>(&mut u).await;
    let (n, _d) = daemon(&fx, "box", NodeState::Idle, vec![]).await;
    send(&mut u, &UiToHub::ListDevices).await;
    let devs = until(&mut u, |m| match m {
        HubToUi::Devices { devices } => Some(devices),
        _ => None,
    })
    .await;
    assert_eq!(devs.len(), 2);
    let node = devs.iter().find(|d| d.id == n.id).unwrap();
    assert_eq!((node.kind, node.revoked), (DeviceKind::Node, false));
    send(&mut u, &UiToHub::RevokeDevice { id: n.id.clone() }).await;
    let devs = until(&mut u, |m| match m {
        HubToUi::Devices { devices } => Some(devices),
        _ => None,
    })
    .await;
    assert!(devs.iter().find(|d| d.id == n.id).unwrap().revoked);
    assert!(!devs.iter().find(|d| d.id == uid.id).unwrap().revoked);
    // Unknown id is an error frame.
    send(
        &mut u,
        &UiToHub::RevokeDevice {
            id: NodeId::new("nope").unwrap(),
        },
    )
    .await;
    until(&mut u, |m| matches!(m, HubToUi::Error { .. }).then_some(())).await;
}

#[tokio::test]
async fn mailbox_log_filters_by_thread_and_limit() {
    let fx = start(fast()).await;
    let (a, b) = (agent("a-1"), agent("b-1"));
    let ta = token(&fx, &a);
    let mut u = ui(&fx).await;
    for body in ["one", "two", "three"] {
        tool(&fx, &ta, "send_message", json!({"to": "b-1", "body": body})).await;
    }
    let _ = b;
    send(
        &mut u,
        &UiToHub::MailboxLog {
            thread: None,
            limit: 2,
        },
    )
    .await;
    let msgs = until(&mut u, |m| match m {
        HubToUi::MailboxLog { messages } => Some(messages),
        _ => None,
    })
    .await;
    assert_eq!(msgs.len(), 2);
    let t = msgs[0].thread().clone();
    send(
        &mut u,
        &UiToHub::MailboxLog {
            thread: Some(t.clone()),
            limit: 100,
        },
    )
    .await;
    let msgs = until(&mut u, |m| match m {
        HubToUi::MailboxLog { messages } => Some(messages),
        _ => None,
    })
    .await;
    assert!(!msgs.is_empty() && msgs.iter().all(|m| m.thread() == &t));
}

fn not_after_ms(cert_pem: &str) -> u64 {
    let (_, der) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).unwrap();
    der.parse_x509().unwrap().validity().not_after.timestamp() as u64 * 1000
}

async fn renew(fx: &Fx, id: &Identity, pem: &str) -> (u16, String) {
    https_as(
        fx,
        Some(id),
        "POST",
        "/renew",
        &[],
        &json!({"public_key": pem}).to_string(),
    )
    .await
}

#[tokio::test]
async fn renew_is_refused_while_the_cert_has_more_than_the_window_left() {
    let fx = start(fast()).await;
    let id = enroll_as(&fx, DeviceKind::Ui, "phone").await;
    let key = rcgen::KeyPair::generate().unwrap();
    let (status, body) = renew(&fx, &id, &key.public_key_pem()).await;
    assert_eq!(status, 409, "{body}");
    let at = serde_json::from_str::<Value>(&body).unwrap()["earliest_renewal_ms"]
        .as_u64()
        .unwrap();
    // Cert lasts 90 days, window is 30: renewable from about day 60.
    let want = not_after_ms(&id.cert) - 30 * 86_400_000;
    assert_eq!(at, want);
    assert!(at > now() + 55 * 86_400_000);
}

#[tokio::test]
async fn renew_inside_the_window_returns_a_working_cert_and_is_recorded() {
    let fx = start(HubConfig {
        cert_days: 3,
        ..fast()
    })
    .await;
    let id = enroll_as(&fx, DeviceKind::Node, "box").await;
    let key = rcgen::KeyPair::generate().unwrap();
    let (status, body) = renew(&fx, &id, &key.public_key_pem()).await;
    assert_eq!(status, 200, "{body}");
    let resp: EnrollResponse = decode(&body).unwrap();
    assert_eq!(resp.ca_cert, fx.ca_pem);
    let (_, der) = x509_parser::pem::parse_x509_pem(resp.cert.as_bytes()).unwrap();
    let cn = der
        .parse_x509()
        .unwrap()
        .subject()
        .iter_common_name()
        .next()
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(cn, id.id.as_str());
    let d = Devices::list(&**fx.hub.store())
        .unwrap()
        .into_iter()
        .find(|d| d.id == id.id)
        .unwrap();
    assert_eq!(d.kind, DeviceKind::Node);
    assert!(d.renewed_ms.is_some());
    // The new cert + new key authenticate (same kind: still a node).
    let renewed = Identity {
        id: id.id.clone(),
        cert: resp.cert,
        key: key.serialize_pem(),
    };
    assert!(connect(&fx, "/ws/daemon", Some(&renewed)).await.is_ok());
    // Same-key renewal is fine too.
    let old_key = rcgen::KeyPair::from_pem(&renewed.key).unwrap();
    let (status, _) = renew(&fx, &renewed, &old_key.public_key_pem()).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn renew_rejects_revoked_anonymous_and_bad_keys() {
    let fx = start(HubConfig {
        cert_days: 3,
        ..fast()
    })
    .await;
    let id = enroll_as(&fx, DeviceKind::Ui, "phone").await;
    let key = rcgen::KeyPair::generate().unwrap().public_key_pem();
    let (status, _) = https(
        &fx,
        "POST",
        "/renew",
        &[],
        &json!({"public_key": key}).to_string(),
    )
    .await;
    assert_eq!(status, 401);
    let (status, _) = renew(&fx, &id, "not a key").await;
    assert_eq!(status, 400);
    let (status, _) = https_as(&fx, Some(&id), "POST", "/renew", &[], "{}").await;
    assert_eq!(status, 400);
    fx.hub.revoke_device(&id.id, now()).unwrap();
    let (status, _) = renew(&fx, &id, &key).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn revoking_a_node_removes_it_and_tells_uis_it_is_offline() {
    let fx = start(fast()).await;
    let (d, _dws) = daemon(&fx, "box", NodeState::Idle, vec![]).await;
    let mut u = ui(&fx).await;
    while fx.hub.registry().snapshot().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fx.hub.revoke_device(&d.id, now()).unwrap();
    let n = until(&mut u, |m| match m {
        HubToUi::NodeUpdate(n) if n.id == d.id => Some(n),
        _ => None,
    })
    .await;
    assert_eq!(n.state, NodeState::Offline);
    assert!(fx.hub.registry().snapshot().iter().all(|n| n.id != d.id));
}

async fn next_error(u: &mut Ws) -> String {
    loop {
        if let HubToUi::Error { message } = recv::<HubToUi>(u).await {
            return message;
        }
    }
}

#[tokio::test]
async fn idle_node_with_high_load_average_is_spawnable_by_default() {
    // ulakd judges load per core; the hub must not re-judge absolute load1.
    LOAD1.store(90, std::sync::atomic::Ordering::Relaxed);
    let fx = start(fast()).await;
    let mut u = ui(&fx).await;
    let (_id, mut ws) = daemon(&fx, "loaded", NodeState::Idle, vec![]).await;
    LOAD1.store(1, std::sync::atomic::Ordering::Relaxed);
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: spec(&agent("a-9")),
        },
    )
    .await;
    assert!(matches!(
        recv::<HubToDaemon>(&mut ws).await,
        HubToDaemon::Spawn(_)
    ));
}

#[tokio::test]
async fn failed_ui_requests_answer_with_an_error() {
    let fx = start(fast()).await;
    let mut u = ui(&fx).await;
    // No node at all.
    send(
        &mut u,
        &UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: spec(&agent("a-1")),
        },
    )
    .await;
    assert!(!next_error(&mut u).await.is_empty());
    // Unknown agent.
    for m in [
        UiToHub::Attach {
            agent: agent("ghost"),
        },
        UiToHub::Kill {
            agent: agent("ghost"),
        },
        UiToHub::PaneInput {
            agent: agent("ghost"),
            data: b"x".to_vec(),
        },
    ] {
        send(&mut u, &m).await;
        assert!(!next_error(&mut u).await.is_empty(), "{m:?}");
    }
}
