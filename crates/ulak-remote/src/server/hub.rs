use super::{mcp, web, ws};
use crate::*;
use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::broadcast, sync::mpsc, task::JoinHandle};
use tokio_rustls::{
    rustls::{
        pki_types::{CertificateDer, PrivateKeyDer},
        server::WebPkiClientVerifier,
        RootCertStore, ServerConfig,
    },
    TlsAcceptor,
};
use ulak_proto::*;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after 1970")
        .as_millis() as u64
}

/// Who is on the other end of a connection: its address and, if it showed a CA-signed cert, the CN.
#[derive(Clone)]
pub(crate) struct Peer {
    pub ip: IpAddr,
    pub cn: Option<NodeId>,
    /// Expiry of the presented client cert, unix ms.
    pub not_after_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct HubConfig {
    /// How long `ask_human` blocks before returning `pending`.
    pub ask_timeout: Duration,
    pub nudge_tick: Duration,
    pub nudge_every_ms: u64,
    pub nudge_max: u32,
    pub expire_tick: Duration,
    pub node_ttl_ms: u64,
    pub agent_token_ttl_ms: u64,
    /// `/enroll` attempts allowed per IP per minute.
    pub enroll_per_min: usize,
    /// Written into `ULAK_MCP_URL`; defaults to `https://<bound addr>/mcp`.
    pub mcp_url: Option<String>,
    pub thresholds: Thresholds,
    /// WebAuthn relying-party id; defaults to the first SAN.
    pub rp_id: Option<String>,
    /// Browser origin passkeys are bound to; defaults to `https://<first SAN>[:port]`.
    pub origin: Option<String>,
    /// Lifetime of a passkey-login session.
    pub session_ttl_ms: u64,
    /// Directory of the web build served at `/`; no directory means no static serving.
    pub web_dir: Option<PathBuf>,
    /// Lifetime of issued client certificates.
    pub cert_days: i64,
    /// `/renew` is accepted only when the current cert has at most this many days left.
    pub renew_window_days: i64,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            ask_timeout: Duration::from_secs(600),
            nudge_tick: Duration::from_secs(5),
            nudge_every_ms: 120_000,
            nudge_max: 3,
            expire_tick: Duration::from_secs(10),
            node_ttl_ms: 30_000,
            agent_token_ttl_ms: 24 * 3_600_000,
            enroll_per_min: 10,
            mcp_url: None,
            rp_id: None,
            origin: None,
            session_ttl_ms: 7 * 24 * 3_600_000,
            web_dir: None,
            cert_days: 90,
            renew_window_days: 30,
            thresholds: Thresholds::default(),
        }
    }
}

pub struct Hub {
    pub(crate) store: Arc<Store>,
    pub(crate) registry: Registry,
    pub(crate) cfg: HubConfig,
    enroller: Enroller,
    tls: Arc<ServerConfig>,
    pub(crate) daemons: Mutex<HashMap<NodeId, mpsc::UnboundedSender<HubToDaemon>>>,
    /// Per connected node: reported version and last update state; dropped on disconnect.
    pub(crate) versions: Mutex<HashMap<NodeId, NodeMeta>>,
    pub(crate) ui: broadcast::Sender<HubToUi>,
    pub(crate) answered: broadcast::Sender<QuestionId>,
    /// Device ids whose open sockets must close (revocation).
    pub(crate) kick: broadcast::Sender<NodeId>,
    /// agent -> (UI connection id -> its outbox): who sent Attach for that agent.
    attached: Mutex<HashMap<AgentId, HashMap<u64, mpsc::UnboundedSender<HubToUi>>>>,
    next_conn: AtomicU64,
    mcp_url: OnceLock<String>,
    first_san: String,
    pub(crate) webauthn: OnceLock<webauthn_rs::Webauthn>,
    /// Pending passkey ceremonies by challenge id (5 min, single use).
    pub(crate) challenges: web::Challenges<web::Challenge>,
    // ponytail: in-memory per-IP window; resets on restart.
    enroll_hits: Mutex<HashMap<IpAddr, Vec<u64>>>,
}

#[derive(Default, Clone)]
pub(crate) struct NodeMeta {
    pub version: Option<VersionInfo>,
    pub update: Option<UpdateState>,
}

/// Handle to the running server. Dropping it does not stop the hub; call `abort`.
pub struct Running {
    addr: SocketAddr,
    tasks: Vec<JoinHandle<()>>,
}
impl Running {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn abort(&self) {
        self.tasks.iter().for_each(|t| t.abort());
    }
}

impl Hub {
    /// `sans` are the DNS names / IPs the server certificate is valid for.
    pub fn new(
        store: Arc<Store>,
        ca: Ca,
        sans: &[String],
        cfg: HubConfig,
    ) -> Result<Arc<Self>, ServerError> {
        let (cert_pem, key_pem) = ca.issue_server_cert(sans, now_ms())?;
        let der = |pem: &str| {
            rustls_pemfile::certs(&mut pem.as_bytes())
                .collect::<Result<Vec<CertificateDer<'static>>, _>>()
                .map_err(|e| ServerError::Pem(e.to_string()))
        };
        let mut roots = RootCertStore::empty();
        for c in der(ca.cert_pem())? {
            roots.add(c).map_err(|e| ServerError::Pem(e.to_string()))?;
        }
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .allow_unauthenticated()
            .build()
            .map_err(|e| ServerError::Pem(e.to_string()))?;
        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .map_err(|e| ServerError::Pem(e.to_string()))?
            .ok_or_else(|| ServerError::Pem("no private key".into()))?;
        // Leaf first, then the CA: `ulakd join` matches the typed CA fingerprint against the chain.
        let mut chain = der(&cert_pem)?;
        chain.extend(der(ca.cert_pem())?);
        let mut tls = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)?;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(Self {
            enroller: Enroller::new(ca, store.clone()).with_cert_days(cfg.cert_days),
            store,
            registry: Registry::default(),
            tls: Arc::new(tls),
            daemons: Mutex::default(),
            versions: Mutex::default(),
            ui: broadcast::channel(256).0,
            answered: broadcast::channel(64).0,
            kick: broadcast::channel(16).0,
            attached: Mutex::default(),
            next_conn: AtomicU64::new(0),
            mcp_url: OnceLock::new(),
            // WebAuthn needs a domain name, so prefer the first SAN that is not an IP address.
            first_san: sans
                .iter()
                .find(|s| s.parse::<IpAddr>().is_err())
                .or(sans.first())
                .cloned()
                .unwrap_or_else(|| "localhost".into()),
            webauthn: OnceLock::new(),
            challenges: web::Challenges::default(),
            enroll_hits: Mutex::default(),
            cfg,
        }))
    }

    pub fn enroller(&self) -> &Enroller {
        &self.enroller
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }
    pub(crate) fn mcp_url(&self) -> &str {
        self.mcp_url.get().map(String::as_str).unwrap_or_default()
    }

    /// Revokes a device and closes its open sockets.
    pub fn revoke_device(&self, id: &NodeId, now_ms: u64) -> Result<(), DeviceError> {
        let was_node = Devices::list(&*self.store)?
            .iter()
            .any(|d| &d.id == id && d.kind == DeviceKind::Node);
        Devices::revoke(&*self.store, id, now_ms)?;
        if was_node {
            // No NodeRemoved variant exists, so UIs are told the node is Offline.
            if let Some(mut n) = self.registry.remove(id) {
                n.state = NodeState::Offline;
                self.ui_send(HubToUi::NodeUpdate(n));
            }
        }
        // No open socket is fine.
        let _ = self.kick.send(id.clone());
        Ok(())
    }

    /// Revokes a passkey, drops its sessions and closes their open sockets.
    pub fn revoke_passkey(&self, id: &NodeId, now_ms: u64) -> Result<(), AuthError> {
        Passkeys::revoke_passkey(&*self.store, id, now_ms)?;
        let _ = self.kick.send(id.clone());
        Ok(())
    }

    pub(crate) fn new_conn(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    fn attachments(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<AgentId, HashMap<u64, mpsc::UnboundedSender<HubToUi>>>>
    {
        self.attached
            .lock()
            .expect("attachment map lock is never held across a panic")
    }

    pub(crate) fn attach(&self, conn: u64, tx: &mpsc::UnboundedSender<HubToUi>, agent: &AgentId) {
        self.attachments()
            .entry(agent.clone())
            .or_default()
            .insert(conn, tx.clone());
    }

    /// Drops `conn`'s attachment to `agent`; true if nobody is attached any more.
    pub(crate) fn detach(&self, conn: u64, agent: &AgentId) -> bool {
        let mut a = self.attachments();
        if let Some(m) = a.get_mut(agent) {
            m.remove(&conn);
            if m.is_empty() {
                a.remove(agent);
                return true;
            }
        }
        false
    }

    /// A UI connection ended: detach it everywhere and stop pane streams nobody watches.
    pub(crate) fn detach_all(&self, conn: u64) {
        let agents: Vec<AgentId> = self.attachments().keys().cloned().collect();
        for agent in agents {
            if self.detach(conn, &agent) {
                if let Ok(node) = self.node_of_agent(&agent) {
                    let _ = self.to_daemon(&node, HubToDaemon::Detach { agent });
                }
            }
        }
    }

    pub(crate) fn pane_output(&self, agent: AgentId, data: Vec<u8>) {
        if let Some(m) = self.attachments().get(&agent) {
            for tx in m.values() {
                let _ = tx.send(HubToUi::PaneOutput {
                    agent: agent.clone(),
                    data: data.clone(),
                });
            }
        }
    }

    pub(crate) fn ui_send(&self, m: HubToUi) {
        // No UI connected is fine.
        let _ = self.ui.send(m);
    }

    pub(crate) fn node_update(&self, id: &NodeId) {
        if let Some(n) = self.registry.snapshot().into_iter().find(|n| &n.id == id) {
            self.ui_send(HubToUi::NodeUpdate(n));
        }
    }

    pub(crate) fn node_of_agent(&self, a: &AgentId) -> Result<NodeId, ServerError> {
        self.registry
            .snapshot()
            .into_iter()
            .find(|n| n.agents.contains(a))
            .map(|n| n.id)
            .ok_or_else(|| ServerError::AgentUnplaced(a.clone()))
    }

    pub(crate) fn to_daemon(&self, node: &NodeId, m: HubToDaemon) -> Result<(), ServerError> {
        let daemons = self
            .daemons
            .lock()
            .expect("daemon map lock is never held across a panic");
        daemons
            .get(node)
            .and_then(|tx| tx.send(m).ok())
            .ok_or_else(|| ServerError::NodeOffline(node.clone()))
    }

    /// The device behind a client cert, if it is active and of the right kind.
    pub(crate) fn authorize(&self, peer: &Peer, kind: DeviceKind) -> Result<NodeId, StatusCode> {
        let id = peer.cn.clone().ok_or(StatusCode::UNAUTHORIZED)?;
        let devices = Devices::list(&*self.store).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        match devices.iter().find(|d| d.id == id) {
            Some(d) if d.revoked_ms.is_none() && d.kind == kind => Ok(id),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }

    pub(crate) fn rate_ok(&self, ip: IpAddr, now: u64) -> bool {
        let mut m = self
            .enroll_hits
            .lock()
            .expect("rate map lock is never held across a panic");
        let hits = m.entry(ip).or_default();
        hits.retain(|t| now.saturating_sub(*t) < 60_000);
        hits.push(now);
        hits.len() <= self.cfg.enroll_per_min
    }

    /// One pass of the nudge loop: tell each node running an agent that its question was answered.
    pub(crate) fn nudge_once(&self, now: u64) {
        let Ok(due) = Questions::due_nudges(
            &*self.store,
            now,
            self.cfg.nudge_every_ms,
            self.cfg.nudge_max,
        ) else {
            return;
        };
        for q in due {
            let Ok(node) = self.node_of_agent(q.from()) else {
                continue;
            };
            let id = q.id().as_str();
            let text = format!(
                "Your question {id} was answered. Call check_answer with question_id {id}."
            );
            let nudge = HubToDaemon::Nudge {
                agent: q.from().clone(),
                text,
            };
            if self.to_daemon(&node, nudge).is_ok() {
                let _ = Questions::record_nudge(&*self.store, q.id(), now);
            }
        }
    }

    fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/enroll", post(enroll))
            .route("/renew", post(renew))
            .route("/mcp", post(mcp::handle))
            .route("/ws/daemon", get(ws::daemon))
            .route("/ws/ui", get(ws::ui))
            .route("/auth/register/start", post(web::register_start))
            .route("/auth/register/finish", post(web::register_finish))
            .route("/auth/login/start", post(web::login_start))
            .route("/auth/login/finish", post(web::login_finish))
            .route("/auth/logout", post(web::logout))
            .route("/api/version", get(web::version))
            .route("/api/devices", get(web::devices))
            .route("/api/devices/otp", post(web::device_otp))
            .route("/api/devices/{id}/revoke", post(web::device_revoke))
            .route("/api/passkeys", get(web::passkeys))
            .route("/api/passkeys/{id}/revoke", post(web::passkey_revoke))
            .fallback(web::static_file)
            .layer(axum::middleware::map_response(web::security_headers))
            .with_state(self.clone())
    }

    /// Binds, then serves TLS and runs the nudge and expiry loops in the background.
    pub async fn bind(self: &Arc<Self>, addr: SocketAddr) -> std::io::Result<Running> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let _ = self.mcp_url.set(
            self.cfg
                .mcp_url
                .clone()
                .unwrap_or_else(|| format!("https://{addr}/mcp")),
        );
        let rp_id = self
            .cfg
            .rp_id
            .clone()
            .unwrap_or_else(|| self.first_san.clone());
        let origin = self
            .cfg
            .origin
            .clone()
            .unwrap_or_else(|| match addr.port() {
                443 => format!("https://{}", self.first_san),
                p => format!("https://{}:{p}", self.first_san),
            });
        // A bad passkey config (e.g. an IP as rp id) must not stop the hub; passkey routes answer 503.
        let wa = webauthn_rs::prelude::Url::parse(&origin)
            .map_err(|e| e.to_string())
            .and_then(|o| {
                webauthn_rs::WebauthnBuilder::new(&rp_id, &o)
                    .and_then(|b| b.rp_name("ulak").build())
                    .map_err(|e| e.to_string())
            });
        match wa {
            Ok(wa) => {
                let _ = self.webauthn.set(wa);
            }
            Err(e) => eprintln!("passkeys disabled (rp_id {rp_id}, origin {origin}): {e}"),
        }
        let acceptor = TlsAcceptor::from(self.tls.clone());
        let router = self.router();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((tcp, remote)) = listener.accept().await else {
                    continue;
                };
                let (acceptor, router) = (acceptor.clone(), router.clone());
                tokio::spawn(async move {
                    // A slow handshake only stalls its own task.
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let (cn, not_after_ms) = tls
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|c| c.first())
                        .and_then(cert_identity)
                        .unzip();
                    let app = router.layer(Extension(Peer {
                        ip: remote.ip(),
                        cn,
                        not_after_ms,
                    }));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), TowerToHyperService::new(app))
                        .with_upgrades()
                        .await;
                });
            }
        });
        let h = self.clone();
        let nudges = tokio::spawn(async move {
            let mut t = tokio::time::interval(h.cfg.nudge_tick);
            loop {
                t.tick().await;
                h.nudge_once(now_ms());
            }
        });
        let h = self.clone();
        let expiry = tokio::spawn(async move {
            let mut t = tokio::time::interval(h.cfg.expire_tick);
            loop {
                t.tick().await;
                h.registry.expire_stale(now_ms(), h.cfg.node_ttl_ms);
                h.challenges.sweep(now_ms());
            }
        });
        Ok(Running {
            addr,
            tasks: vec![accept, nudges, expiry],
        })
    }

    /// DEVELOPMENT ONLY: serves the same router over plain HTTP on a loopback address (so a
    /// browser treats `http://localhost` as a trusted origin for WebAuthn without a CA).
    /// Refuses non-loopback addresses. Peers show no client cert, so only sessions work.
    pub async fn bind_dev_http(self: &Arc<Self>, addr: SocketAddr) -> std::io::Result<Running> {
        if !addr.ip().is_loopback() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "dev http binds loopback addresses only",
            ));
        }
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        eprintln!("WARNING: plain HTTP dev listener on http://{addr} (ULAK_DEV_HTTP): no TLS, development only");
        let router = self.router();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((tcp, remote)) = listener.accept().await else {
                    continue;
                };
                let app = router.clone().layer(Extension(Peer {
                    ip: remote.ip(),
                    cn: None,
                    not_after_ms: None,
                }));
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), TowerToHyperService::new(app))
                        .with_upgrades()
                        .await;
                });
            }
        });
        Ok(Running {
            addr,
            tasks: vec![accept],
        })
    }
}

/// Common name and expiry (unix ms) of a client certificate.
fn cert_identity(cert: &CertificateDer<'_>) -> Option<(NodeId, u64)> {
    let (_, c) = x509_parser::parse_x509_certificate(cert).ok()?;
    let cn = c.subject().iter_common_name().next()?.as_str().ok()?;
    let not_after = u64::try_from(c.validity().not_after.timestamp()).ok()? * 1000;
    Some((NodeId::new(cn).ok()?, not_after))
}

async fn enroll(
    State(h): State<Arc<Hub>>,
    Extension(peer): Extension<Peer>,
    body: Bytes,
) -> Response {
    let now = now_ms();
    if !h.rate_ok(peer.ip, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(req) = serde_json::from_slice::<EnrollRequest>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match h.enroller.redeem(&req, now) {
        Ok(done) => Json(done.response).into_response(),
        Err(EnrollError::BadOtp) => StatusCode::FORBIDDEN.into_response(),
        Err(EnrollError::Locked) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(EnrollError::InvalidKey) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct RenewReq {
    public_key: String,
}

/// mTLS only: an active device swaps its cert for a fresh one, but only near expiry, so a stolen
/// cert cannot be refreshed forever without the window check (and revocation always wins).
async fn renew(
    State(h): State<Arc<Hub>>,
    Extension(peer): Extension<Peer>,
    body: Bytes,
) -> Response {
    let now = now_ms();
    let (Some(id), Some(not_after)) = (peer.cn.clone(), peer.not_after_ms) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !h.rate_ok(peer.ip, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    match Devices::is_active(&*h.store, &id) {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    let Ok(req) = serde_json::from_slice::<RenewReq>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let earliest = not_after.saturating_sub(h.cfg.renew_window_days.max(0) as u64 * 86_400_000);
    if now < earliest {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"earliest_renewal_ms": earliest})),
        )
            .into_response();
    }
    match h.enroller.renew(&id, &req.public_key, now) {
        Ok(r) => Json(r).into_response(),
        Err(EnrollError::InvalidKey) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
