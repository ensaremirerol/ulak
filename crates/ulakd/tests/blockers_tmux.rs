//! Install and login blockers against a real tmux pane (skipped without tmux).
use std::{process::Command, time::Duration};
use ulak_proto::*;
use ulakd::{
    agents::Agents,
    errors::AgentError,
    hub::{AgentHost, HostEvent},
    tmux::Tmux,
};

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn tmux(sock: &str, a: Agents) -> Tmux {
    Tmux::with_agents(Some(sock.to_owned()), a)
        .with_delays(Duration::from_secs(60), Duration::from_millis(50))
        .with_scan(Duration::from_millis(100), Duration::from_millis(100))
}

async fn ev(t: &Tmux) -> Option<HostEvent> {
    tokio::time::timeout(Duration::from_secs(8), t.next_event())
        .await
        .ok()
        .flatten()
}

async fn quiet(t: &Tmux, ms: u64) -> bool {
    tokio::time::timeout(Duration::from_millis(ms), t.next_event())
        .await
        .is_err()
}

fn kill_server(sock: &str) {
    let _ = Command::new("tmux")
        .args(["-L", sock, "kill-server"])
        .output();
}

fn sh_spec(id: &str, script: &str) -> SpawnSpec {
    SpawnSpec::new(
        AgentId::new(id).unwrap(),
        AgentProfile::new("sh", vec!["-c".into(), script.into()], None).unwrap(),
        std::env::temp_dir(),
        "p".into(),
        Default::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn login_detected_once_rearmed_and_readopted() {
    if !have_tmux() {
        eprintln!("SKIPPED: tmux is not installed");
        return;
    }
    let sock = format!("ulak-blk-{}", std::process::id());
    let agents = Agents::from_json(r#"{"sh":{"login_patterns":["please run /login"]}}"#).unwrap();
    let t = tmux(&sock, agents.clone());
    let spec = sh_spec(
        "l1",
        "echo 'Please Run /login now'; read x; clear; sleep 1; echo 'Please Run /login now'; exec cat",
    );
    let key = spec.profile().key().unwrap();
    t.spawn(&spec).await.unwrap();

    let Some(HostEvent::Status { status, .. }) = ev(&t).await else {
        panic!("no status")
    };
    assert_eq!(status, AgentStatus::NeedsLogin);
    let Some(HostEvent::Blocked {
        kind,
        detail,
        profile,
        ..
    }) = ev(&t).await
    else {
        panic!("no blocker")
    };
    assert_eq!(
        (kind, detail.as_str(), &profile),
        (BlockerKind::Login, "Please Run /login now", &key)
    );
    assert!(
        quiet(&t, 800).await,
        "blocker must be raised once per profile"
    );

    // resolve while the phrase is still on screen: stale text does not re-raise
    t.blocker_resolved(&key);
    assert!(quiet(&t, 800).await, "stale phrase re-raised the blocker");
    // the phrase leaves and comes back: re-armed
    t.input(&spec.agent_id().clone(), b"\n").await.unwrap();
    assert!(matches!(ev(&t).await, Some(HostEvent::Status { .. })));
    assert!(matches!(
        ev(&t).await,
        Some(HostEvent::Blocked {
            kind: BlockerKind::Login,
            ..
        })
    ));

    // a restarted daemon lists and re-adopts the session, and watches it again
    let t2 = tmux(&sock, agents);
    assert_eq!(t2.list().await.unwrap(), [spec.agent_id().clone()]);
    assert_eq!(t2.adopt().await.unwrap(), 1);
    assert!(matches!(
        ev(&t2).await,
        Some(HostEvent::Status {
            status: AgentStatus::NeedsLogin,
            ..
        })
    ));
    assert!(matches!(ev(&t2).await, Some(HostEvent::Blocked { .. })));
    kill_server(&sock);
}

#[tokio::test]
async fn missing_binary_without_recipe_raises_install_blocker() {
    if !have_tmux() {
        eprintln!("SKIPPED: tmux is not installed");
        return;
    }
    let sock = format!("ulak-blk-inst-{}", std::process::id());
    let t = tmux(&sock, Agents::default());
    let spec = SpawnSpec::new(
        AgentId::new("i1").unwrap(),
        AgentProfile::new("antigravity-not-here", vec![], None).unwrap(),
        std::env::temp_dir(),
        "p".into(),
        Default::default(),
    )
    .unwrap();
    let err = t.spawn(&spec).await.unwrap_err();
    assert!(matches!(err, AgentError::Install(_)));
    assert!(matches!(
        ev(&t).await,
        Some(HostEvent::Status {
            status: AgentStatus::Installing,
            ..
        })
    ));
    let Some(HostEvent::Blocked { kind, detail, .. }) = ev(&t).await else {
        panic!("no blocker")
    };
    assert_eq!(kind, BlockerKind::Install);
    assert!(detail.contains("no install command"), "{detail}");
    assert!(t.list().await.unwrap().is_empty());
    kill_server(&sock);
}
