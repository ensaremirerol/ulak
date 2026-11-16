//! The hub loop against an in-test fake hub (plain ws://, ulak-proto frames).
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
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
}
impl AgentHost for FakeHost {
    async fn spawn(&self, spec: &SpawnSpec) -> Result<(), AgentError> {
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
    async fn attach(&self, _: &AgentId) -> Result<mpsc::Receiver<Vec<u8>>, AgentError> {
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
