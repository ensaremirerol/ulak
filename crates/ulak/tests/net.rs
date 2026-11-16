//! The net session against a fake in-process hub (plain ws://, no TLS).
use std::{
    sync::{mpsc, Arc},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc::unbounded_channel};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use ulak::{
    model::{apply, ConnStatus, Event, State},
    net::{run_forever, EventTx, Retry, WsTransport},
};
use ulak_proto::{decode, encode, AgentId, HubToUi, UiToHub};
use ulakd::hub::{connect, Backoff};

fn retry() -> Retry {
    Retry {
        backoff: Backoff::new(Duration::from_millis(5), Duration::from_millis(20)),
        stable: Duration::from_secs(60),
    }
}

struct Client {
    events: mpsc::Receiver<Event>,
    out: tokio::sync::mpsc::UnboundedSender<UiToHub>,
}

async fn client(addr: std::net::SocketAddr) -> Client {
    let (etx, events) = mpsc::channel();
    let (out, orx) = unbounded_channel();
    let url = format!("ws://{addr}/ws/ui");
    let connect_ws = move || {
        let url = url.clone();
        async move { Ok(WsTransport(connect(&url, None).await?)) }
    };
    tokio::spawn(run_forever(
        connect_ws,
        orx,
        EventTx::new(etx, Arc::new(|| {})),
        retry(),
    ));
    Client { events, out }
}

async fn next_event(c: &Client) -> Event {
    for _ in 0..400 {
        if let Ok(e) = c.events.try_recv() {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no event");
}

fn snapshot() -> String {
    encode(&HubToUi::Snapshot {
        nodes: vec![],
        questions: vec![],
    })
    .unwrap()
}

#[tokio::test]
async fn snapshot_without_a_hello_reaches_state() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let mut ws = accept_async(l.accept().await.unwrap().0).await.unwrap();
        ws.send_text(snapshot()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let c = client(addr).await;
    let mut s = State::default();
    for _ in 0..3 {
        let e = next_event(&c).await;
        apply(&mut s, e);
    }
    assert_eq!(s.conn, ConnStatus::Connected);
    assert!(s.nodes.is_empty() && s.last_error.is_none());
}

trait SendText {
    async fn send_text(&mut self, t: String);
}
impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> SendText
    for tokio_tungstenite::WebSocketStream<S>
{
    async fn send_text(&mut self, t: String) {
        use futures_util::SinkExt;
        self.send(Message::text(t)).await.unwrap();
    }
}

#[tokio::test]
async fn an_intent_frame_arrives_on_the_wire() {
    use futures_util::StreamExt;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (got_tx, mut got) = unbounded_channel();
    tokio::spawn(async move {
        let mut ws = accept_async(l.accept().await.unwrap().0).await.unwrap();
        while let Some(Ok(Message::Text(t))) = ws.next().await {
            got_tx.send(t.to_string()).unwrap();
        }
    });
    let c = client(addr).await;
    while next_event(&c).await != Event::Connected {}
    let mut s = State::default();
    let frames = s.dispatch(ulak::model::Intent::SelectAgent(
        AgentId::new("a1").unwrap(),
    ));
    for f in frames {
        c.out.send(f).unwrap();
    }
    let t = tokio::time::timeout(Duration::from_secs(5), got.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        decode::<UiToHub>(&t).unwrap(),
        UiToHub::Attach {
            agent: AgentId::new("a1").unwrap()
        }
    );
}

#[tokio::test]
async fn reconnects_after_the_server_drops() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        // First connection: accept, then drop the socket without a close handshake.
        drop(accept_async(l.accept().await.unwrap().0).await.unwrap());
        let mut ws = accept_async(l.accept().await.unwrap().0).await.unwrap();
        ws.send_text(snapshot()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let c = client(addr).await;
    let mut seen = vec![];
    while !matches!(seen.last(), Some(Event::Hub(_))) {
        seen.push(next_event(&c).await);
    }
    let kinds: Vec<_> = seen
        .iter()
        .map(|e| match e {
            Event::Connecting => "connecting",
            Event::Connected => "connected",
            Event::Disconnected(_) => "disconnected",
            Event::Hub(_) => "hub",
            Event::Tick(_) => "tick",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "connecting",
            "connected",
            "disconnected",
            "connecting",
            "connected",
            "hub"
        ]
    );
}
