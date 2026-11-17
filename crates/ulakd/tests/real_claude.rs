//! Manual proof against the REAL `claude` (costs a few subscription tokens), so `#[ignore]`d:
//! `ULAK_REAL_CLAUDE=1 cargo test -p ulakd --test real_claude -- --ignored --nocapture`.
//! Own hub (random port, dirs under target/), own ulakd, private tmux socket.
//! A wrapper script named `claude` plays the agent so the generated config and env of a real
//! hub-driven spawn can be captured; then the real binary is run once with them.
use futures_util::{SinkExt, StreamExt};
use std::{
    path::{Path, PathBuf},
    process::{Command as Std, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};
use ulak_proto::*;

fn target() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_owned()
}

#[tokio::test]
#[ignore]
async fn real_claude_calls_read_inbox_through_a_spawned_config() {
    assert!(Std::new("cargo")
        .args(["build", "-p", "ulak-remote", "--bin", "ulak-remote"])
        .status()
        .unwrap()
        .success());
    let root = target().join(format!("real-claude-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (hub_dir, node_dir, ui_dir) = (root.join("hub"), root.join("node"), root.join("ui"));
    let hub_err = root.join("hub.err");
    std::fs::create_dir_all(&root).unwrap();
    let mut hub = Command::new(target().join("ulak-remote"))
        .env("ULAK_DIR", &hub_dir)
        .env("ULAK_ADDR", "127.0.0.1:0")
        .env("ULAK_BOOTSTRAP_OTP", "node")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&hub_err).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut hin = hub.stdin.take().unwrap();
    let mut out = BufReader::new(hub.stdout.take().unwrap()).lines();
    let mut line = async |prefix: &str| loop {
        let l = timeout(Duration::from_secs(20), out.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("hub exited");
        if let Some(r) = l.strip_prefix(prefix) {
            return r.to_owned();
        }
    };
    let url = line("ulak-remote listening on ").await;
    assert!(!url.contains(":8443"), "must not touch the real hub");
    let sock = format!("ulak-realclaude-{}", std::process::id());
    let ulakd = |dir: &Path| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ulakd"));
        c.env("ULAKD_DIR", dir)
            .env("ULAKD_TMUX_SOCKET", &sock)
            .kill_on_drop(true);
        c
    };
    let otp_line = line("OTP ").await;
    let (otp, rest) = otp_line.split_once(' ').unwrap();
    let fp = rest.trim_end_matches(')').rsplit(' ').next().unwrap();
    let o = ulakd(&node_dir)
        .args(["join", &url])
        .env("ULAKD_OTP", otp)
        .env("ULAKD_FINGERPRINT", fp)
        .output()
        .await
        .unwrap();
    assert!(o.status.success());
    hin.write_all(b"otp ui\n").await.unwrap();
    let otp_line = line("OTP ").await;
    let (uotp, rest) = otp_line.split_once(' ').unwrap();
    let ufp = rest.trim_end_matches(')').rsplit(' ').next().unwrap();
    assert!(ulakd(&ui_dir)
        .args(["join", &url])
        .env("ULAKD_OTP", uotp)
        .env("ULAKD_FINGERPRINT", ufp)
        .output()
        .await
        .unwrap()
        .status
        .success());
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

    // ui socket
    use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let rd = |f: &str| std::fs::read_to_string(ui_dir.join(f)).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(rd("ca.pem").as_bytes()).unwrap())
        .unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![CertificateDer::from_pem_slice(rd("cert.pem").as_bytes()).unwrap()],
            PrivateKeyDer::from_pem_slice(rd("key.pem").as_bytes()).unwrap(),
        )
        .unwrap();
    let (mut ui, _) = connect_async_tls_with_config(
        url.replace("https://", "wss://") + "/ws/ui",
        None,
        false,
        Some(Connector::Rustls(Arc::new(cfg))),
    )
    .await
    .unwrap();
    let node = loop {
        let Some(Ok(Message::Text(t))) = timeout(Duration::from_secs(20), ui.next()).await.unwrap()
        else {
            panic!("ui closed")
        };
        match decode::<HubToUi>(&t).unwrap() {
            HubToUi::Snapshot { nodes, .. } if !nodes.is_empty() => break nodes[0].id.clone(),
            HubToUi::NodeUpdate(n) if n.state == NodeState::Idle => break n.id,
            _ => {}
        }
    };

    // wrapper named `claude`: records argv, env and a copy of the config, then sleeps
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let dump = root.join("dump");
    let wrapper = bin.join("claude");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 0; exit 0; fi\n\
             mkdir -p {d}; printf '%s\\n' \"$@\" > {d}/argv; env > {d}/env\n\
             while [ \"$1\" != --mcp-config ]; do shift; done; cp \"$2\" {d}/cfg.json; sleep 600\n",
            d = dump.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let a = AgentId::new("rc-1").unwrap();
    let spec = SpawnSpec::new(
        a.clone(),
        AgentProfile::new(wrapper.to_str().unwrap(), vec![], None).unwrap(),
        root.clone(),
        "idle".into(),
        Default::default(),
    )
    .unwrap();
    let m = UiToHub::Spawn {
        target: SpawnTarget::Node(node),
        spec,
    };
    ui.send(Message::Text(encode(&m).unwrap().into()))
        .await
        .unwrap();
    for _ in 0..80 {
        if dump.join("cfg.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let cfg_copy = dump.join("cfg.json");
    let env = std::fs::read_to_string(dump.join("env")).expect("agent never started");
    let get = |k: &str| {
        env.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!("{k} missing"))
            .to_owned()
    };
    let (token, ca) = (get("ULAK_AGENT_TOKEN"), get("NODE_EXTRA_CA_CERTS"));
    assert_eq!(ca, node_dir.join("ca.pem").to_str().unwrap());
    let live = node_dir.join("run").join("mcp-rc-1.json");
    assert!(live.exists());
    assert!(!std::fs::read_to_string(&cfg_copy).unwrap().contains(&token));

    // the one paid step: real claude with the generated config
    let o = Command::new(dirs_claude())
        .args([
            "--mcp-config",
            cfg_copy.to_str().unwrap(),
            "--model",
            "haiku",
            "--allowedTools",
            "mcp__ulak__read_inbox",
            "--print",
            "Call the MCP tool read_inbox from the ulak server once and reply only with the word OK, nothing else",
        ])
        .current_dir(&root)
        .env("NODE_EXTRA_CA_CERTS", &ca)
        .env("ULAK_AGENT_TOKEN", &token)
        .output()
        .await
        .unwrap();
    println!(
        "claude: {:?}\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout)
    );
    eprintln!("{}", String::from_utf8_lossy(&o.stderr));
    let log = std::fs::read_to_string(&hub_err).unwrap();
    println!("hub log:\n{log}");

    // cleanup: kill removes the config file
    ui.send(Message::Text(
        encode(&UiToHub::Kill { agent: a.clone() }).unwrap().into(),
    ))
    .await
    .unwrap();
    for _ in 0..40 {
        if !live.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let gone = !live.exists();
    let _ = Std::new("tmux").args(["-L", &sock, "kill-server"]).status();
    daemon.kill().await.unwrap();
    assert!(
        log.contains("rc-1 tools/call read_inbox"),
        "hub never saw read_inbox"
    );
    assert!(gone, "config file left behind after kill");
    let _ = std::fs::remove_dir_all(root);
}

fn dirs_claude() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join(".local/bin/claude")
}
