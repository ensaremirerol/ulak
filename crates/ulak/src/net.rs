//! Hub connection. `Transport` is the port (a web build swaps in a browser WebSocket);
//! `run_session` and `run_forever` are generic over it. `start` is the native mTLS adapter
//! that runs on its own thread and wakes the UI through a callback.
use crate::model::Event;
#[cfg(not(target_arch = "wasm32"))]
use futures_util::{SinkExt, StreamExt};
use std::{
    future::Future,
    sync::{mpsc, Arc},
};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::{tungstenite::Message as WsMessage, WebSocketStream};
#[cfg(not(target_arch = "wasm32"))]
use ulak_proto::{decode, encode};
use ulak_proto::{HubToUi, ProtoError, UiToHub};
#[cfg(not(target_arch = "wasm32"))]
use ulakd::{
    config::{daemon_ws_url, Identity},
    errors::{ConfigError, HubError},
    hub::{client_tls, connect, Backoff},
};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    #[cfg(not(target_arch = "wasm32"))]
    #[error("websocket: {0}")]
    Ws(Box<tokio_tungstenite::tungstenite::Error>),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    Hub(#[from] HubError),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[cfg(not(target_arch = "wasm32"))]
    #[error("runtime: {0}")]
    Runtime(#[from] std::io::Error),
    /// Browser WebSocket failure (`web.rs`).
    #[cfg(target_arch = "wasm32")]
    #[error("browser: {0}")]
    Web(String),
}

#[cfg(not(target_arch = "wasm32"))]
impl From<tokio_tungstenite::tungstenite::Error> for NetError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Ws(Box::new(e))
    }
}

/// One live connection to the hub, in frames.
pub trait Transport {
    fn send(&mut self, frame: &UiToHub) -> impl Future<Output = Result<(), NetError>>;
    /// `Ok(None)` when the hub closed the connection.
    fn recv(&mut self) -> impl Future<Output = Result<Option<HubToUi>, NetError>>;
}

/// `Transport` over a tokio-tungstenite stream (plain, TLS, or in-memory).
#[cfg(not(target_arch = "wasm32"))]
pub struct WsTransport<S>(pub WebSocketStream<S>);

#[cfg(not(target_arch = "wasm32"))]
impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> Transport for WsTransport<S> {
    async fn send(&mut self, frame: &UiToHub) -> Result<(), NetError> {
        Ok(self.0.send(WsMessage::text(encode(frame)?)).await?)
    }
    async fn recv(&mut self) -> Result<Option<HubToUi>, NetError> {
        loop {
            match self.0.next().await {
                None | Some(Ok(WsMessage::Close(_))) => return Ok(None),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(WsMessage::Text(t))) => return Ok(Some(decode(t.as_str())?)),
                Some(Ok(_)) => {} // ping/pong handled by tungstenite; binary ignored
            }
        }
    }
}

/// Where events go: an std channel the UI drains in `poll`, plus a wake callback.
#[derive(Clone)]
pub struct EventTx {
    tx: mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl EventTx {
    pub fn new(tx: mpsc::Sender<Event>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { tx, wake }
    }
    /// False when the UI side is gone.
    pub fn send(&self, e: Event) -> bool {
        let ok = self.tx.send(e).is_ok();
        (self.wake)();
        ok
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    HubClosed,
    UiGone,
}

/// Pumps one connection until it closes or fails. Frames from `out` go to the hub; hub frames
/// become events. A frame that does not decode is reported as a hub error, not fatal.
pub async fn run_session<T: Transport>(
    t: &mut T,
    out: &mut UnboundedReceiver<UiToHub>,
    events: &EventTx,
) -> Result<Exit, NetError> {
    loop {
        tokio::select! {
            frame = out.recv() => match frame {
                Some(f) => t.send(&f).await?,
                None => return Ok(Exit::UiGone),
            },
            got = t.recv() => match got {
                Ok(Some(f)) => {
                    if !events.send(Event::Hub(f)) { return Ok(Exit::UiGone); }
                }
                Ok(None) => return Ok(Exit::HubClosed),
                Err(NetError::Proto(e)) => {
                    events.send(Event::Hub(HubToUi::Error { message: format!("bad hub frame: {e}") }));
                }
                Err(e) => return Err(e),
            },
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct Retry {
    pub backoff: Backoff,
    /// A connection that lived this long resets the backoff.
    pub stable: Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for Retry {
    fn default() -> Self {
        Self {
            backoff: Backoff::new(Duration::from_secs(1), Duration::from_secs(30)),
            stable: Duration::from_secs(10),
        }
    }
}

/// Connect, run, report, back off, repeat; returns only when the UI side is gone.
/// Frames queued while disconnected are dropped on reconnect (the UI re-syncs from the
/// `Connected` event instead).
#[cfg(not(target_arch = "wasm32"))]
pub async fn run_forever<T, F, Fut>(
    mut connect: F,
    mut out: UnboundedReceiver<UiToHub>,
    events: EventTx,
    mut retry: Retry,
) where
    T: Transport,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, NetError>>,
{
    loop {
        if !events.send(Event::Connecting) {
            return;
        }
        match connect().await {
            Ok(mut t) => {
                while out.try_recv().is_ok() {}
                if !events.send(Event::Connected) {
                    return;
                }
                let started = Instant::now();
                let r = run_session(&mut t, &mut out, &events).await;
                if matches!(r, Ok(Exit::UiGone)) {
                    return;
                }
                let why = r
                    .err()
                    .map_or_else(|| "closed by hub".to_owned(), |e| e.to_string());
                if !events.send(Event::Disconnected(why)) {
                    return;
                }
                if started.elapsed() >= retry.stable {
                    retry.backoff.reset();
                }
            }
            Err(e) => {
                if !events.send(Event::Disconnected(e.to_string())) {
                    return;
                }
            }
        }
        tokio::time::sleep(retry.backoff.next_delay()).await;
    }
}

/// `https://host:port` -> `wss://host:port/ws/ui` (`http` -> `ws`).
#[cfg(not(target_arch = "wasm32"))]
pub fn ui_ws_url(hub: &str) -> Result<String, ConfigError> {
    let d = daemon_ws_url(hub)?;
    Ok(d.trim_end_matches("daemon").to_owned() + "ui")
}

/// `$ULAK_DIR`; else Windows `%APPDATA%\ulak`, macOS `~/Library/Application Support/ulak`,
/// elsewhere `$XDG_CONFIG_HOME/ulak`, else `~/.config/ulak`.
#[cfg(not(target_arch = "wasm32"))]
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ULAK_DIR") {
        return d.into();
    }
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("APPDATA") {
        return PathBuf::from(d).join("ulak");
    }
    #[cfg(target_os = "macos")]
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join("Library/Application Support/ulak");
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("ulak");
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".config/ulak")
}

/// Which identity the UI runs with.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    Own,
    Ulakd,
}

#[cfg(not(target_arch = "wasm32"))]
impl IdentitySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Own => "identity: ulak",
            Self::Ulakd => "identity: ulakd (this machine)",
        }
    }
}

/// All four enrollment files present.
#[cfg(not(target_arch = "wasm32"))]
pub fn identity_complete(dir: &std::path::Path) -> bool {
    ["cert.pem", "key.pem", "ca.pem", "hub_url"]
        .iter()
        .all(|f| dir.join(f).is_file())
}

/// Own dir if complete, else ulakd's dir if complete (read-only fallback), else an error
/// that points at `ulak join`.
#[cfg(not(target_arch = "wasm32"))]
pub fn pick_identity(
    own: &std::path::Path,
    ulakd: &std::path::Path,
) -> Result<(PathBuf, IdentitySource), String> {
    if identity_complete(own) {
        Ok((own.to_owned(), IdentitySource::Own))
    } else if identity_complete(ulakd) {
        Ok((ulakd.to_owned(), IdentitySource::Ulakd))
    } else {
        Err(format!(
            "no identity in {} or {}: run `ulak join <hub-url>`",
            own.display(),
            ulakd.display()
        ))
    }
}

/// The UI's end of the connection.
pub struct NetHandle {
    pub events: mpsc::Receiver<Event>,
    out: UnboundedSender<UiToHub>,
}

impl NetHandle {
    pub fn new(events: mpsc::Receiver<Event>, out: UnboundedSender<UiToHub>) -> Self {
        Self { events, out }
    }
    pub fn send(&self, frames: Vec<UiToHub>) {
        for f in frames {
            // Err only when the net thread is gone; the UI shows that through its last event.
            let _ = self.out.send(f);
        }
    }
}

/// Starts the native client on a background thread: mTLS (or plain for `http://` hubs) to
/// `<hub>/ws/ui` with reconnect. `wake` is called after every event.
#[cfg(not(target_arch = "wasm32"))]
pub fn start(id: &Identity, wake: Arc<dyn Fn() + Send + Sync>) -> Result<NetHandle, NetError> {
    start_in(id, None, wake)
}

/// [`start`] plus certificate renewal for the enrollment in `dir`: checked before every connect
/// and daily; the renewed cert is used from the next (re)connect on.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_in(
    id: &Identity,
    dir: Option<PathBuf>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<NetHandle, NetError> {
    let url = ui_ws_url(&id.hub_url)?;
    client_tls(&id.ca_pem, &id.cert_pem, &id.key_pem)?; // fail early on a bad identity
    let (etx, erx) = mpsc::channel();
    let (otx, orx) = unbounded_channel();
    let events = EventTx::new(etx, wake);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let id = Arc::new(std::sync::Mutex::new(id.clone()));
    std::thread::Builder::new()
        .name("ulak-net".into())
        .spawn(move || {
            let renew = {
                let (id, dir) = (id.clone(), dir.clone());
                move || {
                    let (id, dir) = (id.clone(), dir.clone());
                    async move {
                        let Some(dir) = dir else { return };
                        let cur = id.lock().expect("id lock").clone();
                        match ulakd::renew::renew_if_due(&dir, &cur).await {
                            Ok(Some(new)) => *id.lock().expect("id lock") = new,
                            Ok(None) => {}
                            Err(e) => eprintln!("ulak: certificate renewal failed: {e}"),
                        }
                    }
                }
            };
            let connect_ws = || {
                let (url, id, renew) = (url.clone(), id.clone(), renew.clone());
                async move {
                    renew().await;
                    let cur = id.lock().expect("id lock").clone();
                    let tls = url
                        .starts_with("wss://")
                        .then(|| client_tls(&cur.ca_pem, &cur.cert_pem, &cur.key_pem))
                        .transpose()?;
                    Ok(WsTransport(connect(&url, tls).await?))
                }
            };
            rt.block_on(async {
                if dir.is_some() {
                    let renew = renew.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(ulakd::renew::CHECK_EVERY).await;
                            renew().await;
                        }
                    });
                }
                run_forever(connect_ws, orx, events, Retry::default()).await
            });
        })?;
    Ok(NetHandle::new(erx, otx))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod identity_tests {
    use super::*;

    fn dir(files: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ulak-id-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        for f in files {
            std::fs::write(d.join(f), "x").unwrap();
        }
        d
    }
    const ALL: [&str; 4] = ["cert.pem", "key.pem", "ca.pem", "hub_url"];

    #[test]
    fn identity_fallback() {
        let (own, ulakd, part) = (dir(&ALL), dir(&ALL), dir(&["cert.pem"]));
        assert_eq!(pick_identity(&own, &ulakd).unwrap().1, IdentitySource::Own);
        let (p, s) = pick_identity(&part, &ulakd).unwrap();
        assert_eq!((p, s), (ulakd.clone(), IdentitySource::Ulakd));
        assert!(s.label().contains("ulakd (this machine)"));
        let e = pick_identity(&part, &dir(&[])).unwrap_err();
        assert!(e.contains("ulak join"), "{e}");
    }
}
