//! Hub profiles, node settings and node stats (0.1.7).
use futures_util::{SinkExt, StreamExt};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
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

fn new_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ulak-hub-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn start() -> Fx {
    start_in(&new_dir(), Arc::new(Store::open_in_memory().unwrap())).await
}

async fn start_in(dir: &std::path::Path, store: Arc<Store>) -> Fx {
    let ca = Ca::load_or_create(dir).unwrap();
    let ca_pem = ca.cert_pem().to_owned();
    let hub = Hub::new(
        store,
        ca,
        &["127.0.0.1".into(), "localhost".into()],
        HubConfig::default(),
    )
    .unwrap();
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

struct Identity {
    id: NodeId,
    cert: String,
    key: String,
}

async fn enroll_as(fx: &Fx, kind: DeviceKind, host: &str) -> Identity {
    let offer = fx.hub.enroller().begin(kind, now());
    let key = rcgen::KeyPair::generate().unwrap();
    let req = EnrollRequest::new(offer.otp, key.public_key_pem(), host.into()).unwrap();
    let body = encode(&req).unwrap();
    let addr = fx.run.local_addr();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let conn = TlsConnector::from(Arc::new(tls(&fx.ca_pem, None)));
    let mut s = conn
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
        .unwrap();
    let http = format!(
        "POST /enroll HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(http.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;
    let resp: EnrollResponse = decode(out.split_once("\r\n\r\n").unwrap().1).unwrap();
    let (_, der) = x509_parser::pem::parse_x509_pem(resp.cert.as_bytes()).unwrap();
    let c = der.parse_x509().unwrap();
    let cn = c.subject().iter_common_name().next().unwrap();
    Identity {
        id: NodeId::new(cn.as_str().unwrap()).unwrap(),
        cert: resp.cert,
        key: key.serialize_pem(),
    }
}

async fn connect(fx: &Fx, path: &str, id: &Identity) -> Ws {
    let url = format!("wss://{}{path}", fx.run.local_addr());
    let c = Connector::Rustls(Arc::new(tls(&fx.ca_pem, Some(id))));
    connect_async_tls_with_config(url, None, false, Some(c))
        .await
        .unwrap()
        .0
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

async fn drain<T: serde::de::DeserializeOwned>(ws: &mut Ws, ms: u64) -> Vec<T> {
    let mut out = vec![];
    while let Ok(m) = tokio::time::timeout(Duration::from_millis(ms), recv::<T>(ws)).await {
        out.push(m);
    }
    out
}

fn telemetry(at: u64) -> Telemetry {
    Telemetry::new(0.1, 5.0, None, 30.0, at).unwrap()
}

/// A connected, hello'd node that has not sent `Version` yet.
async fn daemon(fx: &Fx, host: &str) -> (Identity, Ws) {
    let id = enroll_as(fx, DeviceKind::Node, host).await;
    let mut ws = connect(fx, "/ws/daemon", &id).await;
    let node = NodeInfo::new(
        id.id.clone(),
        host.into(),
        NodeState::Idle,
        telemetry(now()),
        vec![],
    );
    send(&mut ws, &DaemonToHub::Hello { node }).await;
    (id, ws)
}

async fn version(d: &mut Ws) {
    let v = VersionInfo::new("0.1.7", "deadbee", PROTO_VERSION, "linux", "x86_64").unwrap();
    send(d, &DaemonToHub::Version(v)).await;
}

/// A daemon after `Version`, with the `Config` it got for it.
async fn daemon_v(fx: &Fx, host: &str) -> (Identity, Ws, (Vec<HubProfile>, NodeSettings)) {
    let (id, mut d) = daemon(fx, host).await;
    version(&mut d).await;
    let c = config_of(&mut d).await;
    (id, d, c)
}

async fn config_of(d: &mut Ws) -> (Vec<HubProfile>, NodeSettings) {
    loop {
        if let HubToDaemon::Config { profiles, settings } = recv(d).await {
            return (profiles, settings);
        }
    }
}

async fn no_config(d: &mut Ws) {
    let got: Vec<HubToDaemon> = drain(d, 200).await;
    assert!(
        !got.iter().any(|m| matches!(m, HubToDaemon::Config { .. })),
        "{got:?}"
    );
}

type Cfg = (Vec<ScopedProfile>, Vec<NodeSettingsEntry>);

/// A UI after its connect burst up to and including `HubConfig`.
async fn ui(fx: &Fx) -> (Ws, Cfg) {
    let id = enroll_as(fx, DeviceKind::Ui, "phone").await;
    let mut ws = connect(fx, "/ws/ui", &id).await;
    assert!(matches!(recv(&mut ws).await, HubToUi::Snapshot { .. }));
    assert!(matches!(recv(&mut ws).await, HubToUi::Blockers { .. }));
    assert!(matches!(recv(&mut ws).await, HubToUi::HubVersion(_)));
    let HubToUi::HubConfig { profiles, settings } = recv(&mut ws).await else {
        panic!("HubConfig is part of the connect replay");
    };
    (ws, (profiles, settings))
}

async fn hub_config(u: &mut Ws) -> Cfg {
    loop {
        match recv(u).await {
            HubToUi::HubConfig { profiles, settings } => return (profiles, settings),
            HubToUi::Error { message } => panic!("{message}"),
            _ => {}
        }
    }
}

async fn error(u: &mut Ws) -> String {
    loop {
        if let HubToUi::Error { message } = recv(u).await {
            return message;
        }
    }
}

fn hp(name: &str, binary: &str) -> HubProfile {
    HubProfile {
        name: name.into(),
        binary: binary.into(),
        args: vec![],
        config_dir: None,
        env: BTreeMap::new(),
    }
}

fn scoped(node: Option<&NodeId>, p: HubProfile) -> ScopedProfile {
    ScopedProfile {
        node: node.cloned(),
        profile: p,
    }
}

async fn save(u: &mut Ws, node: Option<&NodeId>, p: HubProfile) -> Cfg {
    send(
        u,
        &UiToHub::SaveProfile {
            node: node.cloned(),
            profile: p,
        },
    )
    .await;
    hub_config(u).await
}

#[tokio::test]
async fn changes_reach_uis_and_only_the_affected_daemons_merged() {
    let fx = start().await;
    let (mut u, (p0, s0)) = ui(&fx).await;
    assert!(p0.is_empty() && s0.is_empty());
    let (mut other_ui, _) = ui(&fx).await;
    let (a, mut da, first) = daemon_v(&fx, "a").await;
    assert_eq!(first, (vec![], NodeSettings::default()));
    let (b, mut db, _) = daemon_v(&fx, "b").await;
    // Connected but no Version yet: nothing is pushed to it.
    let (_c, mut dc) = daemon(&fx, "c").await;

    let (profiles, _) = save(&mut u, None, hp("claude", "claude")).await;
    assert_eq!(profiles, vec![scoped(None, hp("claude", "claude"))]);
    // Every UI gets the broadcast.
    assert_eq!(hub_config(&mut other_ui).await.0, profiles);
    assert_eq!(config_of(&mut da).await.0, vec![hp("claude", "claude")]);
    assert_eq!(config_of(&mut db).await.0, vec![hp("claude", "claude")]);
    no_config(&mut dc).await;

    // Node-scoped beats global of the same name, only on that node; sorted by name.
    save(&mut u, None, hp("zed", "zed")).await;
    config_of(&mut da).await;
    config_of(&mut db).await;
    let (profiles, _) = save(&mut u, Some(&a.id), hp("claude", "/opt/claude-a")).await;
    assert_eq!(
        profiles,
        vec![
            scoped(None, hp("claude", "claude")),
            scoped(None, hp("zed", "zed")),
            scoped(Some(&a.id), hp("claude", "/opt/claude-a")),
        ]
    );
    assert_eq!(
        config_of(&mut da).await.0,
        vec![hp("claude", "/opt/claude-a"), hp("zed", "zed")]
    );
    no_config(&mut db).await;

    // Settings go to their node only.
    let s = NodeSettings {
        index_interval_mins: Some(30),
        auto_update: Some(false),
        ..NodeSettings::default()
    };
    send(
        &mut u,
        &UiToHub::SaveNodeSettings {
            node: b.id.clone(),
            settings: s.clone(),
        },
    )
    .await;
    let (_, settings) = hub_config(&mut u).await;
    assert_eq!(
        settings,
        vec![NodeSettingsEntry {
            node: b.id.clone(),
            settings: s.clone()
        }]
    );
    assert_eq!(
        config_of(&mut db).await,
        (vec![hp("claude", "claude"), hp("zed", "zed")], s.clone())
    );
    no_config(&mut da).await;

    // A late Version gets the merged state.
    version(&mut dc).await;
    assert_eq!(
        config_of(&mut dc).await,
        (
            vec![hp("claude", "claude"), hp("zed", "zed")],
            NodeSettings::default()
        )
    );

    // Deleting the node's own profile brings the global one back.
    send(
        &mut u,
        &UiToHub::DeleteProfile {
            node: Some(a.id.clone()),
            name: "claude".into(),
        },
    )
    .await;
    assert_eq!(hub_config(&mut u).await.0.len(), 2);
    assert_eq!(
        config_of(&mut da).await.0,
        vec![hp("claude", "claude"), hp("zed", "zed")]
    );
    no_config(&mut db).await;
    // Deleting something absent is an error.
    send(
        &mut u,
        &UiToHub::DeleteProfile {
            node: Some(a.id.clone()),
            name: "claude".into(),
        },
    )
    .await;
    assert!(error(&mut u).await.contains("not found"));

    // All-None settings delete the row.
    send(
        &mut u,
        &UiToHub::SaveNodeSettings {
            node: b.id.clone(),
            settings: NodeSettings::default(),
        },
    )
    .await;
    assert!(hub_config(&mut u).await.1.is_empty());
    assert_eq!(config_of(&mut db).await.1, NodeSettings::default());
    assert!(HubProfiles::list_node_settings(&**fx.hub.store())
        .unwrap()
        .is_empty());

    // Deleting a global profile reaches every node that sent Version.
    send(
        &mut u,
        &UiToHub::DeleteProfile {
            node: None,
            name: "zed".into(),
        },
    )
    .await;
    assert_eq!(
        hub_config(&mut u).await.0,
        vec![scoped(None, hp("claude", "claude"))]
    );
    for d in [&mut da, &mut db, &mut dc] {
        assert_eq!(config_of(d).await.0, vec![hp("claude", "claude")]);
    }
}

#[tokio::test]
async fn invalid_requests_are_refused_and_nothing_is_stored() {
    let fx = start().await;
    let (mut u, _) = ui(&fx).await;
    let (n, _d, _) = daemon_v(&fx, "n").await;
    let ghost = NodeId::new("ghost").unwrap();
    let mut env = hp("p", "claude");
    env.env.insert("ulak_mcp_url".into(), "x".into());
    let mut args = hp("p", "claude");
    args.args = vec!["-v".into(); 33];
    let cases = [
        (None, hp("bad name", "claude"), "profile name"),
        (None, env, "reserved"),
        (None, args, "32 args"),
        (None, hp("p", ""), "binary"),
        (Some(ghost.clone()), hp("p", "claude"), "unknown node"),
    ];
    for (node, profile, want) in cases {
        send(&mut u, &UiToHub::SaveProfile { node, profile }).await;
        let e = error(&mut u).await;
        assert!(e.contains(want), "{e}");
    }
    for (node, settings, want) in [
        (
            n.id.clone(),
            NodeSettings {
                index_interval_mins: Some(0),
                ..NodeSettings::default()
            },
            "1-1440",
        ),
        (
            n.id.clone(),
            NodeSettings {
                cpu_busy_pct: Some(101.0),
                ..NodeSettings::default()
            },
            "1-100",
        ),
        (
            ghost.clone(),
            NodeSettings {
                auto_update: Some(true),
                ..NodeSettings::default()
            },
            "unknown node",
        ),
    ] {
        send(&mut u, &UiToHub::SaveNodeSettings { node, settings }).await;
        let e = error(&mut u).await;
        assert!(e.contains(want), "{e}");
    }
    // A revoked node is unknown too.
    fx.hub.revoke_device(&n.id, now()).unwrap();
    send(
        &mut u,
        &UiToHub::SaveProfile {
            node: Some(n.id.clone()),
            profile: hp("p", "claude"),
        },
    )
    .await;
    assert!(error(&mut u).await.contains("unknown node"));
    let store = fx.hub.store();
    assert!(HubProfiles::list_hub_profiles(&**store).unwrap().is_empty());
    assert!(HubProfiles::list_node_settings(&**store)
        .unwrap()
        .is_empty());

    // At most 64 profiles; replacing an existing one still works at the cap.
    for i in 0..64 {
        save(&mut u, None, hp(&format!("p{i}"), "claude")).await;
    }
    send(
        &mut u,
        &UiToHub::SaveProfile {
            node: None,
            profile: hp("p64", "claude"),
        },
    )
    .await;
    assert!(error(&mut u).await.contains("at most 64"));
    let (profiles, _) = save(&mut u, None, hp("p0", "other")).await;
    assert_eq!(profiles.len(), 64);
}

#[tokio::test]
async fn config_survives_a_hub_restart_and_is_replayed_on_connect() {
    let dir = new_dir();
    let open = || Arc::new(Store::open(&dir.join("hub.db")).unwrap());
    let fx = start_in(&dir, open()).await;
    let (mut u, _) = ui(&fx).await;
    let (n, _d, _) = daemon_v(&fx, "n").await;
    save(&mut u, Some(&n.id), hp("work", "claude")).await;
    let s = NodeSettings {
        repo_roots: Some(vec!["~/src".into()]),
        ..NodeSettings::default()
    };
    send(
        &mut u,
        &UiToHub::SaveNodeSettings {
            node: n.id.clone(),
            settings: s.clone(),
        },
    )
    .await;
    let before = hub_config(&mut u).await;
    fx.run.abort();
    drop(u);
    drop(fx);
    let fx2 = start_in(&dir, open()).await;
    let (_u2, after) = ui(&fx2).await;
    assert_eq!(after, before);
    assert_eq!(after.0, vec![scoped(Some(&n.id), hp("work", "claude"))]);
    assert_eq!(after.1[0].settings, s);
}

#[tokio::test]
async fn node_stats_are_kept_in_order_and_sent_to_the_requester_only() {
    let fx = start().await;
    let (mut u, _) = ui(&fx).await;
    let (mut other, _) = ui(&fx).await;
    // Heartbeat times well past the Hello's, so they are told apart.
    let t0 = now() + 60_000;
    let start = now();
    let (n, mut d) = daemon(&fx, "n").await;
    for i in 1..=3 {
        send(&mut d, &DaemonToHub::Heartbeat(telemetry(t0 + i))).await;
    }
    // Each heartbeat is announced as a NodeUpdate once processed.
    let mut seen = 0;
    while seen < 3 {
        if let HubToUi::NodeUpdate(x) = recv(&mut u).await {
            if x.telemetry.at_ms() > t0 {
                seen += 1;
            }
        }
    }
    send(&mut u, &UiToHub::NodeStats { node: n.id.clone() }).await;
    let (connected_ms, samples) = loop {
        if let HubToUi::NodeStats {
            node,
            connected_ms,
            samples,
        } = recv(&mut u).await
        {
            assert_eq!(node, n.id);
            break (connected_ms, samples);
        }
    };
    let at: Vec<u64> = samples.iter().map(Telemetry::at_ms).collect();
    assert_eq!(at.len(), 4, "hello + 3 heartbeats: {at:?}");
    assert_eq!(&at[1..], &[t0 + 1, t0 + 2, t0 + 3]);
    assert!(connected_ms.is_some_and(|c| c >= start && c <= now()));
    let got: Vec<HubToUi> = drain(&mut other, 200).await;
    assert!(!got.iter().any(|m| matches!(m, HubToUi::NodeStats { .. })));

    // Unknown node: empty. Disconnected: no connection start, history kept.
    send(
        &mut u,
        &UiToHub::NodeStats {
            node: NodeId::new("ghost").unwrap(),
        },
    )
    .await;
    loop {
        if let HubToUi::NodeStats {
            connected_ms,
            samples,
            ..
        } = recv(&mut u).await
        {
            assert!(connected_ms.is_none() && samples.is_empty());
            break;
        }
    }
    drop(d);
    loop {
        if let HubToUi::NodeUpdate(x) = recv(&mut u).await {
            if x.id == n.id && x.state == NodeState::Offline {
                break;
            }
        }
    }
    send(&mut u, &UiToHub::NodeStats { node: n.id.clone() }).await;
    loop {
        if let HubToUi::NodeStats {
            connected_ms,
            samples,
            ..
        } = recv(&mut u).await
        {
            assert!(connected_ms.is_none());
            assert_eq!(samples.len(), 4);
            break;
        }
    }
}

#[tokio::test]
async fn hub_profiles_reported_back_by_a_node_steer_placement() {
    let fx = start().await;
    let (mut u, _) = ui(&fx).await;
    let (_a, mut da, _) = daemon_v(&fx, "a").await;
    let (_b, mut db, _) = daemon_v(&fx, "b").await;
    let report = |name: &str, from_hub: bool| ProfileInfo {
        name: name.into(),
        binary: "claude".into(),
        config_dir: None,
        installed: true,
        login_blocked: false,
        from_hub,
    };
    send(
        &mut da,
        &DaemonToHub::Profiles {
            profiles: vec![report("claude", false), report("hub-work", true)],
        },
    )
    .await;
    send(
        &mut db,
        &DaemonToHub::Profiles {
            profiles: vec![report("claude", false)],
        },
    )
    .await;
    let mut reports = 0;
    while reports < 2 {
        if let HubToUi::NodeProfiles { .. } = recv(&mut u).await {
            reports += 1;
        }
    }
    for i in 0..3 {
        let p = AgentProfile::new("hub-work", vec![], None).unwrap();
        let spec = SpawnSpec::new(
            AgentId::new(&format!("w-{i}")).unwrap(),
            p,
            "/tmp".into(),
            "go".into(),
            Default::default(),
        )
        .unwrap();
        send(
            &mut u,
            &UiToHub::Spawn {
                target: SpawnTarget::Auto,
                spec,
            },
        )
        .await;
        loop {
            if let HubToDaemon::Spawn(s) = recv(&mut da).await {
                assert_eq!(s.agent_id().as_str(), format!("w-{i}"));
                break;
            }
        }
    }
    let got: Vec<HubToDaemon> = drain(&mut db, 200).await;
    assert!(!got.iter().any(|m| matches!(m, HubToDaemon::Spawn(_))));
}
