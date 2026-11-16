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
