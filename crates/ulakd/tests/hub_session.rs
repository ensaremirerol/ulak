//! The hub loop against an in-test fake hub (plain ws://, ulak-proto frames).
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use ulak_proto::*;
use ulakd::{
    errors::{AgentError, TelemetryError},
    hub::{connect, run_session, AgentHost, SessionCfg, SharedProbe},
    telemetry::{Probe, Probed},
};

#[derive(Default)]
struct FakeHost {
    log: Mutex<Vec<String>>,
    agents: Mutex<Vec<AgentId>>,
    fail_spawn: Mutex<bool>,
    pane: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    prompt_failed: Mutex<Option<AgentId>>,
    prompt_notify: tokio::sync::Notify,
    /// Spawns wait for `release` while set (a clone in progress).
    hold_spawn: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
    /// Attach is refused while set (no tmux session yet).
    no_session: std::sync::atomic::AtomicBool,
}
impl AgentHost for FakeHost {
    async fn spawn(&self, spec: &SpawnSpec) -> Result<(), AgentError> {
        if self.hold_spawn.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        if *self.fail_spawn.lock().unwrap() {
            return Err(AgentError::NotRunning("x".into()));
        }
        self.agents.lock().unwrap().push(spec.agent_id().clone());
        self.log
            .lock()
            .unwrap()
            .push(format!("spawn {}", spec.agent_id().as_str()));
        Ok(())
    }
    async fn kill(&self, id: &AgentId) -> Result<(), AgentError> {
        self.agents.lock().unwrap().retain(|a| a != id);
        self.log
            .lock()
            .unwrap()
            .push(format!("kill {}", id.as_str()));
        Ok(())
    }
    async fn nudge(&self, id: &AgentId, text: &str) -> Result<(), AgentError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("nudge {} {text}", id.as_str()));
        Ok(())
    }
    async fn input(&self, id: &AgentId, d: &[u8]) -> Result<(), AgentError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("input {} {}", id.as_str(), d.len()));
        Ok(())
    }
    async fn list(&self) -> Result<Vec<AgentId>, AgentError> {
        Ok(self.agents.lock().unwrap().clone())
    }
    async fn next_prompt_failure(&self) -> Option<AgentId> {
        loop {
            self.prompt_notify.notified().await;
            if let Some(id) = self.prompt_failed.lock().unwrap().take() {
                return Some(id);
            }
        }
    }
    async fn attach(&self, id: &AgentId) -> Result<mpsc::Receiver<Vec<u8>>, AgentError> {
        if self.no_session.load(Ordering::SeqCst) {
            return Err(AgentError::NotRunning(id.as_str().into()));
        }
        let (tx, rx) = mpsc::channel(4);
        *self.pane.lock().unwrap() = Some(tx);
        Ok(rx)
    }
}

struct FakeProbe(Arc<Mutex<NodeState>>);
impl Probe for FakeProbe {
    fn sample(&mut self) -> Result<Probed, TelemetryError> {
        Ok(Probed {
            telemetry: Telemetry::new(0.1, 5.0, None, 20.0, 1)?,
            state: self.0.lock().unwrap().clone(),
        })
    }
}

fn spec(id: &str) -> SpawnSpec {
    SpawnSpec::new(
        AgentId::new(id).unwrap(),
        AgentProfile::new("claude", vec![], None).unwrap(),
        "/tmp".into(),
        "go".into(),
        Default::default(),
    )
    .unwrap()
}

async fn next_frame<S>(ws: &mut S) -> DaemonToHub
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("frame within 5s")
            .expect("open")
            .unwrap();
        if let Message::Text(t) = m {
            return decode(t.as_str()).unwrap();
        }
    }
}

#[tokio::test]
async fn full_session() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/daemon", listener.local_addr().unwrap());
    let state = Arc::new(Mutex::new(NodeState::Idle));
    let host = Arc::new(FakeHost::default());
    let probe: SharedProbe = Arc::new(Mutex::new(Box::new(FakeProbe(Arc::clone(&state)))));
    let cfg = SessionCfg {
        node_id: NodeId::new("node-1").unwrap(),
        hostname: "box".into(),
        heartbeat: Duration::from_millis(40),
    };
    let daemon = {
        let host = Arc::clone(&host);
        tokio::spawn(async move {
            let ws = connect(&url, None).await.unwrap();
            run_session(ws, &cfg, &*host, &probe).await
        })
    };

    let (tcp, _) = listener.accept().await.unwrap();
    let mut hub = accept_async(tcp).await.unwrap();
    let send = |m: HubToDaemon| Message::text(encode(&m).unwrap());

    // Hello first
    let DaemonToHub::Hello { node } = next_frame(&mut hub).await else {
        panic!("expected hello")
    };
    assert_eq!(node.id.as_str(), "node-1");
    assert_eq!(node.state, NodeState::Idle);

    // then heartbeats
    assert!(matches!(
        next_frame(&mut hub).await,
        DaemonToHub::Heartbeat(_)
    ));

    // state change -> new Hello
    *state.lock().unwrap() = NodeState::Busy(BusyReason::Gaming {
        process: "steam".into(),
    });
    loop {
        match next_frame(&mut hub).await {
            DaemonToHub::Hello { node } => {
                assert_eq!(
                    node.state,
                    NodeState::Busy(BusyReason::Gaming {
                        process: "steam".into()
                    })
                );
                break;
            }
            DaemonToHub::Heartbeat(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    // spawn -> Starting, Running, Hello listing the agent
    hub.send(send(HubToDaemon::Spawn(spec("a1"))))
        .await
        .unwrap();
    let mut seen = HashMap::new();
    let mut hello_has_agent = false;
    while !(seen.contains_key("running") && hello_has_agent) {
        match next_frame(&mut hub).await {
            DaemonToHub::AgentStatus {
                status: AgentStatus::Starting,
                ..
            } => {
                seen.insert("starting", ());
            }
            DaemonToHub::AgentStatus {
                status: AgentStatus::Running,
                agent,
            } => {
                assert_eq!(agent.as_str(), "a1");
                seen.insert("running", ());
            }
            DaemonToHub::Hello { node } => {
                hello_has_agent = node.agents.iter().any(|a| a.as_str() == "a1")
            }
            _ => {}
        }
    }
    assert!(seen.contains_key("starting"));

    // nudge + input
    let a1 = AgentId::new("a1").unwrap();
    hub.send(send(HubToDaemon::Nudge {
        agent: a1.clone(),
        text: "check q-1".into(),
    }))
    .await
    .unwrap();
    hub.send(send(HubToDaemon::PaneInput {
        agent: a1.clone(),
        data: vec![1, 2, 3],
    }))
    .await
    .unwrap();

    // attach -> pane bytes flow; detach stops forwarding
    hub.send(send(HubToDaemon::Attach { agent: a1.clone() }))
        .await
        .unwrap();
    let tx = loop {
        if let Some(tx) = host.pane.lock().unwrap().clone() {
            break tx;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    tx.send(b"hi".to_vec()).await.unwrap();
    loop {
        if let DaemonToHub::PaneOutput { agent, data } = next_frame(&mut hub).await {
            assert_eq!((agent, data), (a1.clone(), b"hi".to_vec()));
            break;
        }
    }
    hub.send(send(HubToDaemon::Detach { agent: a1.clone() }))
        .await
        .unwrap();

    // kill -> Exited
    hub.send(send(HubToDaemon::Kill { agent: a1.clone() }))
        .await
        .unwrap();
    loop {
        if let DaemonToHub::AgentStatus {
            status: AgentStatus::Exited(_),
            agent,
        } = next_frame(&mut hub).await
        {
            assert_eq!(agent, a1);
            break;
        }
    }
    let log = host.log.lock().unwrap().clone();
    assert_eq!(
        log,
        ["spawn a1", "nudge a1 check q-1", "input a1 3", "kill a1"]
    );

    // failed spawn -> Failed
    *host.fail_spawn.lock().unwrap() = true;
    hub.send(send(HubToDaemon::Spawn(spec("a2"))))
        .await
        .unwrap();
    loop {
        if let DaemonToHub::AgentStatus {
            status: AgentStatus::Failed,
            ..
        } = next_frame(&mut hub).await
        {
            break;
        }
    }

    // an agent that vanishes by itself is reported as exited
    *host.fail_spawn.lock().unwrap() = false;
    hub.send(send(HubToDaemon::Spawn(spec("a3"))))
        .await
        .unwrap();
    while !matches!(
        next_frame(&mut hub).await,
        DaemonToHub::AgentStatus {
            status: AgentStatus::Running,
            ..
        }
    ) {}
    host.agents.lock().unwrap().clear();
    loop {
        if let DaemonToHub::AgentStatus {
            status: AgentStatus::Exited(None),
            agent,
        } = next_frame(&mut hub).await
        {
            assert_eq!(agent.as_str(), "a3");
            break;
        }
    }

    // garbage from the hub does not kill the session
    hub.send(Message::text("not json")).await.unwrap();
    assert!(matches!(
        next_frame(&mut hub).await,
        DaemonToHub::Heartbeat(_) | DaemonToHub::Hello { .. }
    ));

    hub.close(None).await.unwrap();
    assert!(daemon.await.unwrap().is_ok());
}

#[tokio::test]
async fn prompt_failure_reports_failed_and_drops_the_agent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/daemon", listener.local_addr().unwrap());
    let host = Arc::new(FakeHost::default());
    let probe: SharedProbe = Arc::new(Mutex::new(Box::new(FakeProbe(Arc::new(Mutex::new(
        NodeState::Idle,
    ))))));
    let cfg = SessionCfg {
        node_id: NodeId::new("node-1").unwrap(),
        hostname: "box".into(),
        heartbeat: Duration::from_millis(40),
    };
    let h = Arc::clone(&host);
    tokio::spawn(async move {
        let ws = connect(&url, None).await.unwrap();
        run_session(ws, &cfg, &*h, &probe).await
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut hub = accept_async(tcp).await.unwrap();
    hub.send(Message::text(
        encode(&HubToDaemon::Spawn(spec("pf"))).unwrap(),
    ))
    .await
    .unwrap();
    // wait for Running, then the prompt fails
    loop {
        if let DaemonToHub::AgentStatus { status, .. } = next_frame(&mut hub).await {
            if status == AgentStatus::Running {
                break;
            }
        }
    }
    *host.prompt_failed.lock().unwrap() = Some(AgentId::new("pf").unwrap());
    host.prompt_notify.notify_one();
    loop {
        if let DaemonToHub::AgentStatus {
            agent,
            status: AgentStatus::Failed,
        } = next_frame(&mut hub).await
        {
            assert_eq!(agent.as_str(), "pf");
            break;
        }
    }
    assert!(host.log.lock().unwrap().contains(&"kill pf".to_owned()));
}

/// Daemon on a fake hub with a spawn of `id` held (clone in progress) and no session yet.
async fn starting_agent(
    id: &str,
) -> (
    Arc<FakeHost>,
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/daemon", listener.local_addr().unwrap());
    let host = Arc::new(FakeHost::default());
    host.hold_spawn.store(true, Ordering::SeqCst);
    host.no_session.store(true, Ordering::SeqCst);
    let probe: SharedProbe = Arc::new(Mutex::new(Box::new(FakeProbe(Arc::new(Mutex::new(
        NodeState::Idle,
    ))))));
    let cfg = SessionCfg {
        node_id: NodeId::new("node-1").unwrap(),
        hostname: "box".into(),
        heartbeat: Duration::from_millis(40),
    };
    let h = Arc::clone(&host);
    tokio::spawn(async move {
        let ws = connect(&url, None).await.unwrap();
        run_session(ws, &cfg, &*h, &probe).await
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut hub = accept_async(tcp).await.unwrap();
    hub.send(send(HubToDaemon::Spawn(spec(id)))).await.unwrap();
    loop {
        if let DaemonToHub::AgentStatus { status, .. } = next_frame(&mut hub).await {
            assert_eq!(status, AgentStatus::Starting);
            break;
        }
    }
    (host, hub)
}

fn send(m: HubToDaemon) -> Message {
    Message::text(encode(&m).unwrap())
}

async fn pane_stays_unset(host: &FakeHost) {
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(host.pane.lock().unwrap().is_none());
}

#[tokio::test]
async fn attach_during_start_streams_once_the_session_appears() {
    let (host, mut hub) = starting_agent("rb").await;
    let id = AgentId::new("rb").unwrap();
    hub.send(send(HubToDaemon::Attach { agent: id.clone() }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(host.pane.lock().unwrap().is_none());
    host.no_session.store(false, Ordering::SeqCst); // tmux session appears, spawn still running
    let tx = loop {
        if let Some(tx) = host.pane.lock().unwrap().clone() {
            break tx;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    tx.send(b"hi".to_vec()).await.unwrap();
    loop {
        if let DaemonToHub::PaneOutput { agent, data } = next_frame(&mut hub).await {
            assert_eq!((agent, data), (id, b"hi".to_vec()));
            break;
        }
    }
}

#[tokio::test]
async fn attach_during_start_streams_when_the_spawn_finishes() {
    let (host, mut hub) = starting_agent("rc").await;
    hub.send(send(HubToDaemon::Attach {
        agent: AgentId::new("rc").unwrap(),
    }))
    .await
    .unwrap();
    host.no_session.store(false, Ordering::SeqCst);
    host.release.notify_one();
    loop {
        if host.pane.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn detach_cancels_a_pending_attach() {
    let (host, mut hub) = starting_agent("rd").await;
    let id = AgentId::new("rd").unwrap();
    hub.send(send(HubToDaemon::Attach { agent: id.clone() }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    hub.send(send(HubToDaemon::Detach { agent: id }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    host.no_session.store(false, Ordering::SeqCst);
    host.release.notify_one();
    pane_stays_unset(&host).await;
}

#[tokio::test]
async fn spawn_failure_drops_a_pending_attach() {
    let (host, mut hub) = starting_agent("re").await;
    hub.send(send(HubToDaemon::Attach {
        agent: AgentId::new("re").unwrap(),
    }))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    *host.fail_spawn.lock().unwrap() = true;
    host.release.notify_one();
    loop {
        if let DaemonToHub::AgentStatus { status, .. } = next_frame(&mut hub).await {
            if status == AgentStatus::Failed {
                break;
            }
        }
    }
    host.no_session.store(false, Ordering::SeqCst);
    pane_stays_unset(&host).await;
}

#[tokio::test]
async fn attach_for_an_unknown_agent_is_not_retried() {
    let (host, mut hub) = starting_agent("rf").await;
    hub.send(send(HubToDaemon::Attach {
        agent: AgentId::new("nobody").unwrap(),
    }))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    host.no_session.store(false, Ordering::SeqCst);
    pane_stays_unset(&host).await;
}

/// A host with profiles, a repo index and cleanup (the other calls are inert).
#[derive(Default)]
struct IndexHost {
    repos: Mutex<Vec<RepoEntry>>,
    maintained: std::sync::atomic::AtomicUsize,
}

fn entry(path: &str) -> RepoEntry {
    RepoEntry {
        full_name: RepoRef::new("octo/a").unwrap(),
        path: path.into(),
        branch: Some("main".into()),
        clean: true,
        ahead: Some(0),
        behind: Some(0),
        last_fetch_ms: None,
        in_use: false,
        managed: true,
    }
}

fn item(path: &str, reason: &str) -> CleanItem {
    CleanItem {
        path: path.into(),
        full_name: None,
        reason: reason.into(),
    }
}

impl AgentHost for IndexHost {
    async fn spawn(&self, _: &SpawnSpec) -> Result<(), AgentError> {
        Ok(())
    }
    async fn kill(&self, _: &AgentId) -> Result<(), AgentError> {
        Ok(())
    }
    async fn nudge(&self, _: &AgentId, _: &str) -> Result<(), AgentError> {
        Ok(())
    }
    async fn input(&self, _: &AgentId, _: &[u8]) -> Result<(), AgentError> {
        Ok(())
    }
    async fn list(&self) -> Result<Vec<AgentId>, AgentError> {
        Ok(vec![])
    }
    async fn attach(&self, _: &AgentId) -> Result<mpsc::Receiver<Vec<u8>>, AgentError> {
        Err(AgentError::NotRunning("x".into()))
    }
    async fn profiles(&self) -> Option<Vec<ProfileInfo>> {
        Some(vec![ProfileInfo {
            name: "claude-work".into(),
            binary: "claude".into(),
            config_dir: Some("/h/.claude-work".into()),
            installed: true,
            login_blocked: false,
        }])
    }
    async fn repo_index(&self) -> Option<Vec<RepoEntry>> {
        Some(self.repos.lock().unwrap().clone())
    }
    async fn clean(&self, req: ulakd::hub::CleanReq) -> Option<ulakd::repos::Cleaned> {
        use ulakd::hub::CleanReq;
        Some(match req {
            CleanReq::All { .. } => {
                self.repos.lock().unwrap().retain(|e| e.path != "/w/a/a");
                ulakd::repos::Cleaned {
                    removed: vec![item("/w/a/a", "up to date")],
                    kept: vec![item("/w/b/a", "has a stash")],
                }
            }
            CleanReq::One { path } => ulakd::repos::Cleaned {
                removed: vec![],
                kept: vec![item(&path, "not an indexed clone of this node")],
            },
        })
    }
    fn maintenance_interval(&self) -> Duration {
        Duration::from_millis(1500)
    }
    async fn maintain(&self) -> Option<ulakd::repos::Cleaned> {
        let first = self.maintained.fetch_add(1, Ordering::SeqCst) == 0;
        Some(ulakd::repos::Cleaned {
            removed: if first {
                vec![item("/w/old/a", "idle")]
            } else {
                vec![]
            },
            kept: vec![],
        })
    }
}

/// The next frame that is not a Heartbeat or Hello.
async fn skip_beats<S>(ws: &mut S) -> DaemonToHub
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match next_frame(ws).await {
            DaemonToHub::Heartbeat(_) | DaemonToHub::Hello { .. } => {}
            f => return f,
        }
    }
}

#[tokio::test]
async fn profiles_repo_index_and_clean_reports() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/daemon", listener.local_addr().unwrap());
    let host = Arc::new(IndexHost::default());
    *host.repos.lock().unwrap() = vec![entry("/w/a/a"), entry("/w/b/a")];
    let state = Arc::new(Mutex::new(NodeState::Idle));
    let probe: SharedProbe = Arc::new(Mutex::new(Box::new(FakeProbe(state))));
    let cfg = SessionCfg {
        node_id: NodeId::new("node-1").unwrap(),
        hostname: "box".into(),
        heartbeat: Duration::from_millis(50),
    };
    let h = Arc::clone(&host);
    let daemon = tokio::spawn(async move {
        let ws = connect(&url, None).await.unwrap();
        run_session(ws, &cfg, &*h, &probe).await
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut hub = accept_async(tcp).await.unwrap();
    assert!(
        matches!(skip_beats(&mut hub).await, DaemonToHub::Profiles { profiles } if profiles[0].name == "claude-work")
    );
    assert!(
        matches!(skip_beats(&mut hub).await, DaemonToHub::RepoIndex { repos } if repos.len() == 2)
    );
    let send = |m: HubToDaemon| Message::text(encode(&m).unwrap());

    // dry run report, then the changed index (profiles unchanged: not resent)
    hub.send(send(HubToDaemon::CleanRepos { dry_run: true }))
        .await
        .unwrap();
    assert_eq!(
        skip_beats(&mut hub).await,
        DaemonToHub::CleanReport {
            dry_run: true,
            removed: vec![item("/w/a/a", "up to date")],
            kept: vec![item("/w/b/a", "has a stash")],
        }
    );
    assert!(
        matches!(skip_beats(&mut hub).await, DaemonToHub::RepoIndex { repos } if repos == [entry("/w/b/a")])
    );
    // one path; the unchanged index is not resent, so the next frame is the automatic cleanup
    hub.send(send(HubToDaemon::CleanRepo {
        path: "/etc".into(),
    }))
    .await
    .unwrap();
    assert!(
        matches!(skip_beats(&mut hub).await, DaemonToHub::CleanReport { dry_run: false, removed, kept }
        if removed.is_empty() && kept[0].path == "/etc")
    );
    assert!(
        matches!(skip_beats(&mut hub).await, DaemonToHub::CleanReport { dry_run: false, removed, .. }
        if removed[0].path == "/w/old/a")
    );
    daemon.abort();
}
