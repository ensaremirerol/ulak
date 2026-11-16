//! Fake hub: host events become frames, BlockerResolved reaches the host, spawns do not block the loop.
use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use ulak_proto::*;
use ulakd::{
    errors::{AgentError, TelemetryError},
    hub::{connect, run_session, AgentHost, HostEvent, SessionCfg, SharedProbe},
    telemetry::{Probe, Probed},
};

struct Host {
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<HostEvent>>,
    resolved: Mutex<Vec<ProfileKey>>,
    spawn_delay: Duration,
}
impl AgentHost for Host {
    async fn spawn(&self, _: &SpawnSpec) -> Result<(), AgentError> {
        tokio::time::sleep(self.spawn_delay).await;
        Err(AgentError::Install("no recipe".into()))
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
        Ok(mpsc::channel(1).1)
    }
    async fn next_event(&self) -> Option<HostEvent> {
        self.events.lock().await.recv().await
    }
    fn blocker_resolved(&self, p: &ProfileKey) {
        self.resolved.lock().unwrap().push(p.clone());
    }
}

struct P;
impl Probe for P {
    fn sample(&mut self) -> Result<Probed, TelemetryError> {
        Ok(Probed {
            telemetry: Telemetry::new(0.1, 5.0, None, 20.0, 1)?,
            state: NodeState::Idle,
        })
    }
}

async fn frame<S>(ws: &mut S) -> DaemonToHub
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
async fn events_resolved_and_nonblocking_spawn() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/daemon", listener.local_addr().unwrap());
    let (etx, erx) = mpsc::unbounded_channel();
    let host = Arc::new(Host {
        events: erx.into(),
        resolved: Mutex::default(),
        spawn_delay: Duration::from_millis(800),
    });
    let probe: SharedProbe = Arc::new(Mutex::new(Box::new(P)));
    let cfg = SessionCfg {
        node_id: NodeId::new("n1").unwrap(),
        hostname: "box".into(),
        heartbeat: Duration::from_millis(50),
    };
    let h = Arc::clone(&host);
    tokio::spawn(async move {
        let ws = connect(&url, None).await.unwrap();
        run_session(ws, &cfg, &*h, &probe).await
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut hub = accept_async(tcp).await.unwrap();
    let send = |m: HubToDaemon| Message::text(encode(&m).unwrap());
    assert!(matches!(frame(&mut hub).await, DaemonToHub::Hello { .. }));

    let agent = AgentId::new("a1").unwrap();
    let profile = AgentProfile::new("claude", vec![], None).unwrap();
    let key = profile.key().unwrap();

    // host events are forwarded as the right frames
    etx.send(HostEvent::Status {
        agent: agent.clone(),
        status: AgentStatus::NeedsLogin,
    })
    .unwrap();
    etx.send(HostEvent::Blocked {
        agent: agent.clone(),
        profile: key.clone(),
        kind: BlockerKind::Login,
        detail: "Please run /login".into(),
    })
    .unwrap();
    let (mut st, mut bl) = (false, false);
    while !(st && bl) {
        match frame(&mut hub).await {
            DaemonToHub::AgentStatus {
                status: AgentStatus::NeedsLogin,
                ..
            } => st = true,
            DaemonToHub::AgentBlocked {
                profile,
                kind,
                detail,
                ..
            } => {
                assert_eq!((profile, kind), (key.clone(), BlockerKind::Login));
                assert_eq!(detail, "Please run /login");
                bl = true;
            }
            _ => {}
        }
    }

    // BlockerResolved reaches the host
    hub.send(send(HubToDaemon::BlockerResolved {
        profile: key.clone(),
    }))
    .await
    .unwrap();
    for _ in 0..100 {
        if !host.resolved.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(*host.resolved.lock().unwrap(), [key]);

    // a slow spawn does not stall heartbeats; its failure is reported afterwards
    let spec = SpawnSpec::new(
        agent.clone(),
        profile,
        "/tmp".into(),
        "go".into(),
        Default::default(),
    )
    .unwrap();
    hub.send(send(HubToDaemon::Spawn(spec))).await.unwrap();
    let mut beats = 0;
    loop {
        match frame(&mut hub).await {
            DaemonToHub::Heartbeat(_) => beats += 1,
            DaemonToHub::AgentStatus {
                status: AgentStatus::Failed,
                ..
            } => break,
            _ => {}
        }
    }
    assert!(beats >= 2, "loop was blocked during spawn ({beats} beats)");
}
