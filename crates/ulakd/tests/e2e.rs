//! Real `ulak-remote` and real `ulakd` binaries talking over loopback mTLS.
//! The tmux half (spawn, nudge, kill) only runs when `tmux` is installed.
use futures_util::{SinkExt, StreamExt};
use std::{
    path::{Path, PathBuf},
    process::{Command as Std, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};
use ulak_proto::*;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn hub_bin() -> PathBuf {
    let ok = Std::new(std::env::var("CARGO").unwrap_or("cargo".into()))
        .args(["build", "-p", "ulak-remote", "--bin", "ulak-remote"])
        .status()
        .unwrap()
        .success();
    assert!(ok, "building ulak-remote failed");
    // target/debug/deps/e2e-xxxx -> target/debug/ulak-remote
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ulak-remote")
}

struct Hub {
    child: Child,
    stdin: ChildStdin,
    out: tokio::io::Lines<BufReader<ChildStdout>>,
    url: String,
}

impl Hub {
    async fn line(&mut self, prefix: &str) -> String {
        loop {
            let l = timeout(Duration::from_secs(20), self.out.next_line())
                .await
                .expect("hub output timed out")
                .unwrap()
                .expect("hub exited");
            if let Some(rest) = l.strip_prefix(prefix) {
                return rest.to_owned();
            }
        }
    }
    /// Asks the admin console for a new code; returns (otp, fingerprint).
    async fn otp(&mut self, kind: &str) -> (String, String) {
        self.stdin
            .write_all(format!("otp {kind}\n").as_bytes())
            .await
            .unwrap();
        let l = self.line("OTP ").await;
        let (otp, rest) = l.split_once(' ').unwrap();
        let fp = rest.trim_end_matches(')').rsplit(' ').next().unwrap();
        (otp.to_owned(), fp.to_owned())
    }
}

async fn start_hub(dir: &Path) -> Hub {
    let mut child = Command::new(hub_bin())
        .env("ULAK_DIR", dir)
        .env("ULAK_ADDR", "127.0.0.1:0")
        .env("ULAK_BOOTSTRAP_OTP", "node")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let out = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut hub = Hub {
        child,
        stdin,
        out,
        url: String::new(),
    };
    hub.url = hub.line("ulak-remote listening on ").await;
    hub
}

/// Private tmux server per test process: never the user's or a parallel test's.
fn socket() -> String {
    format!("ulak-e2e-{}", std::process::id())
}

fn ulakd(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ulakd"));
    c.env("ULAKD_DIR", dir)
        .env("ULAKD_TMUX_SOCKET", socket())
        .kill_on_drop(true);
    c
}

async fn join(url: &str, dir: &Path, otp: &str, fp: &str) -> String {
    let out = ulakd(dir)
        .args(["join", url])
        .env("ULAKD_OTP", otp)
        .env("ULAKD_FINGERPRINT", fp)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap()
}

async fn ui_connect(url: &str, dir: &Path) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(read(dir, "ca.pem").as_bytes()).unwrap())
        .unwrap();
    let chain = vec![CertificateDer::from_pem_slice(read(dir, "cert.pem").as_bytes()).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(read(dir, "key.pem").as_bytes()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .unwrap();
    let url = url.replace("https://", "wss://") + "/ws/ui";
    connect_async_tls_with_config(url, None, false, Some(Connector::Rustls(Arc::new(cfg))))
        .await
        .map(|r| r.0)
}

async fn next_ui(ws: &mut Ws, secs: u64) -> Option<HubToUi> {
    loop {
        match timeout(Duration::from_secs(secs), ws.next()).await.ok()?? {
            Ok(Message::Text(t)) => return Some(decode(&t).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn node_in(m: &HubToUi) -> Option<NodeInfo> {
    match m {
        HubToUi::Snapshot { nodes, .. } => nodes.first().cloned(),
        HubToUi::NodeUpdate(n) => Some(n.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn hub_and_daemon_end_to_end() {
    let root = std::env::temp_dir().join(format!("ulak-e2e-{}", std::process::id()));
    let (hub_dir, node_dir, ui_dir) = (root.join("hub"), root.join("node"), root.join("ui"));
    let mut hub = start_hub(&hub_dir).await;
    let (otp, fp) = {
        let l = hub.line("OTP ").await;
        let (otp, rest) = l.split_once(' ').unwrap();
        (
            otp.to_owned(),
            rest.trim_end_matches(')')
                .rsplit(' ')
                .next()
                .unwrap()
                .to_owned(),
        )
    };

    // join: this fails unless the hub serves the CA in its TLS chain
    let said = join(&hub.url, &node_dir, &otp, &fp).await;
    assert!(said.contains("enrolled as"), "{said}");
    let node_id = read(&node_dir, "cert.pem");
    assert!(node_id.contains("BEGIN CERTIFICATE"));

    // a ui device enrolled with a second code (also via `ulakd join`)
    let (uotp, ufp) = hub.otp("ui").await;
    let said = join(&hub.url, &ui_dir, &uotp, &ufp).await;
    let ui_id = said.split_whitespace().nth(2).unwrap().to_owned();

    // Permissive guard: a loaded box would otherwise report the node Busy(Load) and refuse spawns.
    std::fs::write(
        node_dir.join("guard.json"),
        r#"{"gpu_threshold":100,"cpu_threshold":100,"load_per_core":1000}"#,
    )
    .unwrap();
    let mut daemon = ulakd(&node_dir)
        .arg("run")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    // the hub sees the node Idle, then a later heartbeat
    let mut ui = ui_connect(&hub.url, &ui_dir).await.unwrap();
    let mut first = None;
    while first.is_none() {
        let m = next_ui(&mut ui, 15).await.expect("no node seen");
        first = node_in(&m).filter(|n| n.state == NodeState::Idle);
    }
    let first = first.unwrap();
    let node = first.id.clone();
    let mut beat = false;
    while !beat {
        let m = next_ui(&mut ui, 15).await.expect("no heartbeat");
        beat = node_in(&m).is_some_and(|n| n.telemetry.at_ms() > first.telemetry.at_ms());
    }

    // tmux half
    if Std::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        tmux_half(&mut ui, &node).await;
    } else {
        eprintln!("SKIPPED: tmux is not installed; spawn/nudge/kill not exercised");
    }

    // revoke: the live socket closes and the device cannot come back
    hub.stdin
        .write_all(format!("revoke {ui_id}\n").as_bytes())
        .await
        .unwrap();
    while next_ui(&mut ui, 10).await.is_some() {}
    assert!(ui_connect(&hub.url, &ui_dir).await.is_err());
    hub.stdin
        .write_all(format!("revoke {}\n", node.as_str()).as_bytes())
        .await
        .unwrap();
    // the daemon's reconnects are refused (403); give the hub time to mark it offline
    tokio::time::sleep(Duration::from_secs(3)).await;
    // fresh ui device sees the node offline or gone
    let (uotp, ufp) = hub.otp("ui").await;
    let ui2_dir = root.join("ui2");
    join(&hub.url, &ui2_dir, &uotp, &ufp).await;
    let mut ui2 = ui_connect(&hub.url, &ui2_dir).await.unwrap();
    let snap = next_ui(&mut ui2, 10).await.unwrap();
    let n = node_in(&snap);
    assert!(
        n.is_none_or(|n| n.state != NodeState::Idle),
        "revoked node still idle: {snap:?}"
    );
    daemon.kill().await.unwrap();
    let _ = hub.child.kill().await;
    let _ = std::fs::remove_dir_all(root);
}

async fn tmux_half(ui: &mut Ws, node: &NodeId) {
    let a = AgentId::new("e2e-1").unwrap();
    let spec = SpawnSpec::new(
        a.clone(),
        AgentProfile::new("sh", vec!["-c".into(), "exec cat".into()], None).unwrap(),
        std::env::temp_dir(),
        "boot".into(),
        Default::default(),
    )
    .unwrap();
    let tx = |m: UiToHub| Message::Text(encode(&m).unwrap().into());
    ui.send(tx(UiToHub::Spawn {
        target: SpawnTarget::Node(node.clone()),
        spec,
    }))
    .await
    .unwrap();
    let session = format!("ulak-{}", a.as_str());
    let has = || {
        Std::new("tmux")
            .args(["-L", &socket(), "has-session", "-t", &session])
            .status()
            .is_ok_and(|s| s.success())
    };
    for _ in 0..120 {
        if has() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(has(), "tmux session {session} missing");
    // The hub only routes Attach once a Hello lists the agent.
    loop {
        match next_ui(ui, 15).await.expect("hub never listed the agent") {
            HubToUi::NodeUpdate(n) if n.agents.contains(&a) => break,
            _ => {}
        }
    }
    ui.send(tx(UiToHub::Attach { agent: a.clone() }))
        .await
        .unwrap();
    ui.send(tx(UiToHub::PaneInput {
        agent: a.clone(),
        data: b"ping-e2e\n".to_vec(),
    }))
    .await
    .unwrap();
    let mut seen = false;
    while !seen {
        if let HubToUi::PaneOutput { data, .. } = next_ui(ui, 15).await.expect("no pane output") {
            seen = String::from_utf8_lossy(&data).contains("ping-e2e");
        }
    }
    ui.send(tx(UiToHub::Kill { agent: a.clone() }))
        .await
        .unwrap();
    for _ in 0..40 {
        if !has() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(!has(), "kill left session {session}");
}
