//! The whole stack through the UI model: real `ulak-remote` + `ulakd` binaries, real tmux,
//! `net::start` over mTLS, and an MCP client playing the agent. Skips without tmux.
//!
//! The hub runs with `ULAK_ASK_TIMEOUT_MS`, so `ask_human` returns `pending` by itself; the
//! answer then reaches the agent by nudge and `check_answer`. The node gets a permissive
//! `guard.json`: on a loaded box the default thresholds report it Busy(Load) and spawn waits.
mod common;
use common::{bin, log_file, read_log, start_hub_env};
use serde_json::{json, Value};
use std::{
    path::Path,
    process::{Command as Std, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_rustls::{rustls, rustls::pki_types::pem::PemObject, TlsConnector};
use ulak::{
    app::App,
    model::{ConnStatus, Intent, SpawnForm},
    net::start,
};
use ulak_proto::{AgentId, AgentStatus};
use ulakd::config::Identity;

const STEP: Duration = Duration::from_secs(30);

/// Pumps the app until `cond` holds; panics with `what` and the UI error after `STEP`.
async fn wait(app: &mut App, what: &str, logs: &[&Path], cond: impl Fn(&App) -> bool) {
    let end = Instant::now() + STEP;
    loop {
        app.pump();
        if cond(app) {
            return;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}; nodes {:?}, statuses {:?}, conn {:?}, error {:?}, screens {:?}, selected {:?}, pane {:?}; logs {:?}",
            app.ui.nodes.iter().map(|n| (n.id.as_str().to_owned(), format!("{:?}", n.state), n.agents.len())).collect::<Vec<_>>(),
            app.ui.statuses,
            app.ui.conn,
            app.ui.last_error,
            app.ui.screens.iter().map(|(k, s)| (k.as_str().to_owned(), s.lines().join("|"))).collect::<Vec<_>>(),
            app.ui.selected,
            tmux(&["capture-pane", "-p", "-t", &format!("ulak-{}", app.ui.selected.as_ref().map_or("", |a| a.as_str()))]),
            logs.iter().map(|d| read_log(d)).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Own tmux server per test process (`-L`), so parallel tests and the user's sessions never mix.
fn socket() -> String {
    format!("ulak-ft-{}", std::process::id())
}

fn tmux(args: &[&str]) -> Option<String> {
    let o = Std::new("tmux")
        .args(["-L", &socket()])
        .args(args)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn screen_text(app: &App, a: &AgentId) -> String {
    app.ui
        .screens
        .get(a)
        .map(|s| s.lines().join("\n"))
        .unwrap_or_default()
}

/// One MCP `tools/call` over HTTPS; `None` when the reply did not arrive within `max`.
async fn mcp(
    hub: &str,
    ca_pem: &str,
    token: &str,
    tool: &str,
    args: Value,
    max: Duration,
) -> Option<Value> {
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls::pki_types::CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let addr = hub.trim_start_matches("https://");
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut s = TlsConnector::from(Arc::new(cfg))
        .connect("127.0.0.1".try_into().unwrap(), tcp)
        .await
        .unwrap();
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": args}})
    .to_string();
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    tokio::time::timeout(max, s.read_to_string(&mut out))
        .await
        .ok()?
        .ok();
    let r: Value = serde_json::from_str(out.split_once("\r\n\r\n")?.1).ok()?;
    assert_eq!(r["result"]["isError"], false, "{r}");
    serde_json::from_str(r["result"]["content"][0]["text"].as_str()?).ok()
}

struct KillSession(String);
impl Drop for KillSession {
    fn drop(&mut self) {
        let _ = tmux(&["kill-session", "-t", &self.0]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_drives_agent_question_and_kill_through_the_real_stack() {
    if tmux(&["-V"]).is_none() {
        eprintln!("SKIPPED: tmux is not installed; full-stack test not run");
        return;
    }
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("ulak-full-{pid}"));
    let (hub_dir, node_dir, ui_dir) = (root.join("hub"), root.join("node"), root.join("ui"));
    std::fs::create_dir_all(&root).unwrap();
    let mut hub = start_hub_env(
        &hub_dir,
        &[
            ("ULAK_ASK_TIMEOUT_MS", "1000"),
            ("ULAK_NUDGE_TICK_MS", "200"),
            ("ULAK_NUDGE_EVERY_MS", "500"),
        ],
    )
    .await;
    let l = hub.line("OTP ").await;
    let (notp, rest) = l.split_once(' ').unwrap();
    let nfp = rest.trim_end_matches(')').rsplit(' ').next().unwrap();

    let ulakd = bin("ulakd", "ulakd");
    let joined = Command::new(&ulakd)
        .args(["join", &hub.url])
        .env("ULAKD_DIR", &node_dir)
        .env("ULAKD_OTP", notp)
        .env("ULAKD_FINGERPRINT", nfp)
        .output()
        .await
        .unwrap();
    assert!(joined.status.success(), "{joined:?}");
    std::fs::write(
        node_dir.join("guard.json"),
        r#"{"gpu_threshold":100,"cpu_threshold":100,"load_per_core":1000}"#,
    )
    .unwrap();
    let _daemon = Command::new(&ulakd)
        .arg("run")
        .env("ULAKD_DIR", &node_dir)
        .env("ULAKD_TMUX_SOCKET", socket())
        .stderr(log_file(&node_dir))
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let (uotp, ufp) = hub.otp("ui").await;
    let mut ask = |q: &str| -> std::io::Result<String> {
        Ok(if q.contains("fingerprint") {
            ufp.clone()
        } else {
            uotp.clone()
        })
    };
    ulakd::join::join(&hub.url, &ui_dir, &mut ask)
        .await
        .expect("ui enrollment");
    let id = Identity::load(&ui_dir).unwrap();
    let mut app = App {
        net: Some(start(&id, Arc::new(|| {})).unwrap()),
        ..App::default()
    };

    // The "agent": prints its own token (so the MCP client can act as it), then echoes input.
    let script = root.join("agent.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho TOKEN=$ULAK_AGENT_TOKEN\nexec cat\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let agent = AgentId::new(&format!("ft-{pid}")).unwrap();
    let session = format!("ulak-{}", agent.as_str());
    let _cleanup = KillSession(session.clone());

    wait(
        &mut app,
        "connection and the enrolled node",
        &[&hub_dir, &node_dir],
        |a| a.ui.conn == ConnStatus::Connected && !a.ui.nodes.is_empty(),
    )
    .await;
    let node = app.ui.nodes[0].id.as_str().to_owned();

    // Spawn from the UI model.
    app.dispatch(Intent::Spawn(SpawnForm {
        node,
        agent_id: agent.as_str().into(),
        binary: script.to_str().unwrap().into(),
        workdir: std::env::temp_dir().to_str().unwrap().into(),
        prompt: "boot".into(),
        ..SpawnForm::default()
    }));
    assert!(app.ui.last_error.is_none(), "{:?}", app.ui.last_error);
    wait(
        &mut app,
        "agent Running and listed",
        &[&hub_dir, &node_dir],
        |a| {
            a.ui.statuses.get(&agent) == Some(&AgentStatus::Running)
                && a.ui.node_of(&agent).is_some()
        },
    )
    .await;
    assert!(tmux(&["has-session", "-t", &session]).is_some());

    // Attach; the agent's own output shows up in the Screen.
    app.dispatch(Intent::SelectAgent(agent.clone()));
    wait(
        &mut app,
        "agent token on the screen",
        &[&hub_dir, &node_dir],
        |a| screen_text(a, &agent).contains("TOKEN="),
    )
    .await;
    let token: String = screen_text(&app, &agent)
        .split("TOKEN=")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    assert_eq!(token.len(), 64, "token {token:?}");

    // Typing through PaneInput.
    app.dispatch(Intent::SendKeys(b"hello-ft\n".to_vec()));
    wait(&mut app, "echoed input", &[&hub_dir, &node_dir], |a| {
        screen_text(a, &agent).contains("hello-ft")
    })
    .await;

    // The agent asks; the hub's own timeout turns the call into `pending`.
    let ca = std::fs::read_to_string(ui_dir.join("ca.pem")).unwrap();
    let asked = mcp(
        &hub.url,
        &ca,
        &token,
        "ask_human",
        json!({"question": "blue or green?"}),
        STEP,
    )
    .await
    .expect("ask_human reply");
    assert_eq!(asked["status"], "pending", "{asked}");
    wait(
        &mut app,
        "the question in the UI",
        &[&hub_dir, &node_dir],
        |a| !a.ui.questions.is_empty(),
    )
    .await;
    let q = app.ui.questions[0].id().clone();
    app.dispatch(Intent::AnswerQuestion {
        question: q.clone(),
        text: "the-secret-42".into(),
    });

    // The nudge is typed into the pane: question id, never the answer.
    let end = Instant::now() + STEP;
    let pane = loop {
        let p = tmux(&["capture-pane", "-p", "-J", "-t", &session]).unwrap_or_default();
        if p.contains(q.as_str()) {
            break p;
        }
        assert!(Instant::now() < end, "no nudge in the pane:\n{p}");
        app.pump();
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        !pane.contains("the-secret-42"),
        "answer leaked into pane:\n{pane}"
    );

    let got = mcp(
        &hub.url,
        &ca,
        &token,
        "check_answer",
        json!({"question_id": q.as_str()}),
        STEP,
    )
    .await
    .expect("check_answer reply");
    assert_eq!(got["status"], "answered", "{got}");
    assert_eq!(got["answer"], "the-secret-42");
    wait(
        &mut app,
        "question closed in the UI",
        &[&hub_dir, &node_dir],
        |a| a.ui.questions.is_empty(),
    )
    .await;

    // Kill from the UI.
    app.dispatch(Intent::Kill(agent.clone()));
    let end = Instant::now() + STEP;
    while tmux(&["has-session", "-t", &session]).is_some() {
        assert!(Instant::now() < end, "kill left session {session}");
        app.pump();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = tmux(&["kill-server"]);
    let _ = std::fs::remove_dir_all(root);
}
