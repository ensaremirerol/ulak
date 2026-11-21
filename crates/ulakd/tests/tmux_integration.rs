//! Runs only when `tmux` is installed. Uses a throwaway server (`-L`).
use std::{collections::BTreeMap, process::Command, time::Duration};
use ulak_proto::*;
use ulakd::{hub::AgentHost, tmux::Tmux};

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn spec(id: &str, cfg: Option<&str>) -> SpawnSpec {
    let mut env = BTreeMap::new();
    env.insert("ULAK_T".to_owned(), "tval".to_owned());
    SpawnSpec::new(
        AgentId::new(id).unwrap(),
        AgentProfile::new(
            "sh",
            vec![
                "-c".into(),
                "echo cfg=$CLAUDE_CONFIG_DIR t=$ULAK_T; exec cat".into(),
            ],
            cfg.map(Into::into),
        )
        .unwrap(),
        std::env::temp_dir(),
        "hello prompt".into(),
        env,
    )
    .unwrap()
}

async fn screen(t: &Tmux, id: &AgentId, needle: &str) -> bool {
    for _ in 0..40 {
        let mut rx = t.attach_agent(id).await.unwrap();
        if let Some(f) = rx.recv().await {
            if String::from_utf8_lossy(&f).contains(needle) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn spawn_type_nudge_kill() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let sock = format!("ulakd-test-{}", std::process::id());
    let t = Tmux::new(Some(sock.clone()))
        .with_delays(Duration::from_millis(200), Duration::from_millis(50));
    assert!(t.list_agents().await.unwrap().is_empty());

    let s = spec("it1", Some("/cfg/dir"));
    t.spawn_agent(&s).await.unwrap();
    let id = s.agent_id();
    let slow = Tmux::new(Some(sock.clone()));
    let s2 = spec("it2", None);
    let t1 = std::time::Instant::now();
    slow.spawn_agent(&s2).await.unwrap(); // default 1.5 s startup delay must not block
    assert!(t1.elapsed() < Duration::from_millis(1000), "spawn blocked");
    slow.kill_agent(s2.agent_id()).await.unwrap();
    assert_eq!(t.list_agents().await.unwrap(), vec![id.clone()]);
    assert!(t.spawn_agent(&s).await.is_err(), "duplicate session");

    assert!(screen(&t, id, "cfg=/cfg/dir t=tval").await);
    assert!(screen(&t, id, "hello prompt").await);

    t.nudge_agent(id, "check q-9").await.unwrap();
    assert!(screen(&t, id, "check q-9").await);
    assert!(t.nudge_agent(id, "bad\ntext").await.is_err());

    t.input_agent(id, b"raw input\r").await.unwrap();
    assert!(screen(&t, id, "raw input").await);

    // A viewer's terminal size reaches the tmux window (clamped to sane bounds).
    let size = |sz: &str| {
        let o = Command::new("tmux")
            .args([
                "-L",
                &sock,
                "display-message",
                "-p",
                "-t",
                &id.tmux_session(),
            ])
            .arg("#{window_width}x#{window_height}")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), sz);
    };
    AgentHost::resize(&t, id, 100, 30).await.unwrap();
    size("100x30");
    AgentHost::resize(&t, id, 9999, 1).await.unwrap();
    size("400x5");
    let mut rx = t.attach_agent(id).await.unwrap();
    let frame = String::from_utf8(rx.recv().await.unwrap()).unwrap();
    assert!(frame.split("\r\n").count() <= 5, "{frame:?}");

    t.kill_agent(id).await.unwrap();
    assert!(t.list_agents().await.unwrap().is_empty());
    let _ = Command::new("tmux")
        .args(["-L", &sock, "kill-server"])
        .output();
}

#[tokio::test]
async fn prompt_typing_failure_is_reported() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let sock = format!("ulakd-test-pf-{}", std::process::id());
    let t = Tmux::new(Some(sock.clone()))
        .with_delays(Duration::from_millis(300), Duration::from_millis(50));
    // `true` exits at once: the session is gone before the prompt can be pasted.
    let s = SpawnSpec::new(
        AgentId::new("pf1").unwrap(),
        AgentProfile::new("true", vec![], None).unwrap(),
        std::env::temp_dir(),
        "hello".into(),
        BTreeMap::new(),
    )
    .unwrap();
    t.spawn_agent(&s).await.unwrap();
    let failed = tokio::time::timeout(Duration::from_secs(10), t.next_prompt_failure())
        .await
        .expect("failure reported in time");
    assert_eq!(failed.as_ref(), Some(s.agent_id()));
    let _ = Command::new("tmux")
        .args(["-L", &sock, "kill-server"])
        .output();
}

/// Reads the stream until `needle` shows up (or 10 s pass).
async fn stream_until(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>, needle: &str) -> String {
    let mut seen = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(b) = rx.recv().await {
            seen.extend_from_slice(&b);
            if String::from_utf8_lossy(&seen).contains(needle) {
                break;
            }
        }
    })
    .await;
    String::from_utf8_lossy(&seen).into_owned()
}

#[tokio::test]
async fn stream_types_through_the_client_resizes_and_leaves_no_client() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let sock = format!("ulakd-stream-{}", std::process::id());
    let t = Tmux::new(Some(sock.clone()))
        .with_delays(Duration::from_millis(200), Duration::from_millis(50));
    let sp = SpawnSpec::new(
        AgentId::new("st1").unwrap(),
        AgentProfile::new("sh", vec!["-i".into()], None).unwrap(),
        std::env::temp_dir(),
        "true".into(),
        BTreeMap::new(),
    )
    .unwrap();
    t.spawn_agent(&sp).await.unwrap();
    let id = sp.agent_id();
    let tmux = |args: &[&str]| {
        let o = Command::new("tmux")
            .args(["-L", &sock])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_owned()
    };
    // 0.1.10 pins the size; streaming must undo it.
    AgentHost::resize(&t, id, 150, 45).await.unwrap();

    let mut rx = AgentHost::attach_stream(&t, id, 100, 30).await.unwrap();
    for _ in 0..40 {
        if !tmux(&["list-clients"]).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        tmux(&["list-clients", "-F", "#{client_width}x#{client_height}"]),
        "100x30"
    );
    assert_eq!(
        tmux(&["show-options", "-v", "-t", &id.tmux_session(), "status"]),
        "off"
    );
    assert_eq!(
        tmux(&["show-options", "-v", "-t", &id.tmux_session(), "mouse"]),
        "on"
    );
    assert_eq!(
        tmux(&["show-options", "-gv", "mouse"]),
        "off",
        "options must stay per session"
    );

    // Input goes through the client, as typed.
    t.input_agent(id, b"echo stream-$((6*7))\r").await.unwrap();
    assert!(stream_until(&mut rx, "stream-42")
        .await
        .contains("stream-42"));
    AgentHost::resize(&t, id, 90, 25).await.unwrap();
    for _ in 0..40 {
        let w = tmux(&[
            "display-message",
            "-p",
            "-t",
            &id.tmux_session(),
            "#{window_width}x#{window_height}",
        ]);
        if w == "90x25" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // tmux applies a pane's tty size on a short timer after the window changes.
    tokio::time::sleep(Duration::from_millis(600)).await;
    t.input_agent(id, b"echo size=$(stty size)\r")
        .await
        .unwrap();
    let seen = stream_until(&mut rx, "size=25 90").await;
    assert!(
        seen.contains("size=25 90"),
        "{:?}",
        &seen[seen.len().saturating_sub(400)..]
    );

    // A second attach replaces the client: still one client.
    let _rx2 = AgentHost::attach_stream(&t, id, 80, 24).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(tmux(&["list-clients", "-F", "#{client_width}"]), "80");
    AgentHost::detach_stream(&t, id);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(tmux(&["list-clients"]), "");
    // Without a client, input falls back to the paste path.
    t.input_agent(id, b"echo pasted\r").await.unwrap();
    assert!(screen(&t, id, "pasted").await);

    t.kill_agent(id).await.unwrap();
    let _ = Command::new("tmux")
        .args(["-L", &sock, "kill-server"])
        .output();
}
