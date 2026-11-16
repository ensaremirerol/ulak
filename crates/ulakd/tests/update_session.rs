//! Fake hub: Version follows Hello, CheckUpdate/UpdateNow produce UpdateStatus frames.
use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use ulak_proto::*;
use ulakd::{
    errors::{AgentError, TelemetryError, UpdateError},
    hub::{connect, run_session_with, AgentHost, SessionCfg, SharedProbe},
    telemetry::{Probe, Probed},
    update::{config::UpdateConfig, Updater},
};

struct Host;
impl AgentHost for Host {
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
        Ok(mpsc::channel(1).1)
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
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("frame within 10s")
            .expect("open")
            .unwrap();
        if let Message::Text(t) = m {
            return decode(t.as_str()).unwrap();
        }
    }
}

#[tokio::test]
async fn version_after_hello_and_update_commands_report_status() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/daemon", listener.local_addr().unwrap());
    let probe: SharedProbe = Arc::new(Mutex::new(Box::new(P)));
    let cfg = SessionCfg {
        node_id: NodeId::new("n1").unwrap(),
        hostname: "box".into(),
        heartbeat: Duration::from_secs(60),
    };
    let dir = tempfile::tempdir().unwrap();
    // nothing listens on port 1: every check fails, which is a status the hub must hear about
    let ucfg = UpdateConfig {
        api_base: "http://127.0.0.1:1".into(),
        ..UpdateConfig::default()
    };
    let info = VersionInfo::current("0.1.0", "abc").unwrap();
    let u = Arc::new(Updater::new(
        ucfg,
        false,
        None,
        info.clone(),
        dir.path().join("ulakd"),
        dir.path().to_owned(),
        Err(UpdateError::NoKey),
    ));
    tokio::spawn(async move {
        let ws = connect(&url, None).await.unwrap();
        run_session_with(ws, &cfg, &Host, &probe, Some(u)).await
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut hub = accept_async(tcp).await.unwrap();
    assert!(matches!(frame(&mut hub).await, DaemonToHub::Hello { .. }));
    assert_eq!(frame(&mut hub).await, DaemonToHub::Version(info));

    for cmd in [HubToDaemon::CheckUpdate, HubToDaemon::UpdateNow] {
        hub.send(Message::text(encode(&cmd).unwrap()))
            .await
            .unwrap();
        loop {
            match frame(&mut hub).await {
                DaemonToHub::UpdateStatus(UpdateState::Failed { message }) => {
                    assert!(message.contains("update server"), "{message}");
                    break;
                }
                DaemonToHub::UpdateStatus(UpdateState::Checking) | DaemonToHub::Heartbeat(_) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}
