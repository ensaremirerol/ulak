//! Hub client: mTLS WebSocket, Hello/Heartbeat, command handling, reconnect with capped backoff.
use crate::{
    config::{daemon_ws_url, Identity},
    errors::{AgentError, HubError},
    renew,
    repos::Cleaned,
    telemetry::{Probe, Probed},
    update::Updater,
};
use futures_util::{future::BoxFuture, stream::FuturesUnordered, FutureExt, SinkExt, StreamExt};
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    ClientConfig, RootCertStore,
};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_tungstenite::{
    connect_async_tls_with_config, tungstenite::Message, Connector, WebSocketStream,
};
use ulak_proto::{
    decode, encode, AgentId, AgentStatus, BlockerKind, DaemonToHub, GitToken, HubProfile,
    HubToDaemon, NodeId, NodeInfo, NodeSettings, ProfileInfo, ProfileKey, RepoEntry, SpawnSpec,
};

/// Something the host wants the hub to hear about, outside of a command's own reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    Status {
        agent: AgentId,
        status: AgentStatus,
    },
    Blocked {
        agent: AgentId,
        profile: ProfileKey,
        kind: BlockerKind,
        detail: String,
    },
}

/// What the hub loop needs from the agent runtime. `Tmux` is the real one.
pub trait AgentHost: Send + Sync + 'static {
    fn spawn(&self, spec: &SpawnSpec) -> impl Future<Output = Result<(), AgentError>> + Send;
    fn kill(&self, id: &AgentId) -> impl Future<Output = Result<(), AgentError>> + Send;
    fn nudge(
        &self,
        id: &AgentId,
        text: &str,
    ) -> impl Future<Output = Result<(), AgentError>> + Send;
    fn input(
        &self,
        id: &AgentId,
        data: &[u8],
    ) -> impl Future<Output = Result<(), AgentError>> + Send;
    fn list(&self) -> impl Future<Output = Result<Vec<AgentId>, AgentError>> + Send;
    /// Resolves when typing the prompt into a spawned agent failed (the spawn itself had
    /// succeeded). Hosts that never report this keep the default: pending forever.
    fn next_prompt_failure(&self) -> impl Future<Output = Option<AgentId>> + Send {
        std::future::pending()
    }
    /// Resolves with the next install/login event; hosts without any keep the default: pending.
    fn next_event(&self) -> impl Future<Output = Option<HostEvent>> + Send {
        std::future::pending()
    }
    /// The user resolved the blocker of `profile`: re-arm login detection.
    fn blocker_resolved(&self, _profile: &ProfileKey) {}
    /// A repo spawn's token, delivered right before its `Spawn`. Never log it.
    fn git_access(&self, _agent: AgentId, _token: GitToken) {}
    /// Pane bytes until the receiver is dropped.
    fn attach(
        &self,
        id: &AgentId,
    ) -> impl Future<Output = Result<mpsc::Receiver<Vec<u8>>, AgentError>> + Send;
    /// The node's named profiles; `None` (default): this host reports none.
    fn profiles(&self) -> impl Future<Output = Option<Vec<ProfileInfo>>> + Send {
        std::future::ready(None)
    }
    /// The node's repo index; `None` (default): this host reports none.
    fn repo_index(&self) -> impl Future<Output = Option<Vec<RepoEntry>>> + Send {
        std::future::ready(None)
    }
    /// Runs a clean request; `None` (default): this host cannot clean.
    fn clean(&self, _req: CleanReq) -> impl Future<Output = Option<Cleaned>> + Send {
        std::future::ready(None)
    }
    /// How often [`AgentHost::maintain`] runs.
    fn maintenance_interval(&self) -> Duration {
        Duration::from_secs(600)
    }
    /// The hub's profiles and settings (replacing the previous ones); default: ignored.
    fn apply_config(&self, _profiles: &[HubProfile], _settings: &NodeSettings) {}
    /// Periodic upkeep (rescan, automatic cleanup); returns what an automatic cleanup did.
    fn maintain(&self) -> impl Future<Output = Option<Cleaned>> + Send {
        std::future::ready(None)
    }
}

/// A clean request from the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanReq {
    All { dry_run: bool },
    One { path: String },
}

impl CleanReq {
    fn dry_run(&self) -> bool {
        matches!(self, Self::All { dry_run: true })
    }
}

/// A background job's result (scans and cleans never block the session loop).
enum Done {
    Indexed(Option<Vec<ProfileInfo>>, Option<Vec<RepoEntry>>),
    Cleaned(bool, Option<Cleaned>),
    Maintained(Option<Cleaned>),
}

pub type SharedProbe = Arc<Mutex<Box<dyn Probe>>>;

#[derive(Debug, Clone)]
pub struct SessionCfg {
    pub node_id: NodeId,
    pub hostname: String,
    pub heartbeat: Duration,
}

/// Client TLS config: trusts only the pinned hub CA and presents the device certificate.
pub fn client_tls(
    ca_pem: &str,
    cert_pem: &str,
    key_pem: &str,
) -> Result<Arc<ClientConfig>, HubError> {
    let tls = |e: &dyn std::fmt::Display| HubError::Tls(e.to_string());
    let mut roots = RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(c.map_err(|e| tls(&e))?).map_err(|e| tls(&e))?;
    }
    let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| tls(&e))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| tls(&e))?;
    let cfg = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| tls(&e))?
    .with_root_certificates(roots)
    .with_client_auth_cert(chain, key)
    .map_err(|e| tls(&e))?;
    Ok(Arc::new(cfg))
}

/// Connects to any `ws://` or `wss://` URL. `tls` is used for `wss`.
pub async fn connect(
    url: &str,
    tls: Option<Arc<ClientConfig>>,
) -> Result<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, HubError> {
    let (ws, _) =
        connect_async_tls_with_config(url, None, false, tls.map(Connector::Rustls)).await?;
    Ok(ws)
}

/// 1 s doubling to a 30 s cap.
#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
    cap: Duration,
    base: Duration,
}

impl Backoff {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            next: base,
            cap,
            base,
        }
    }
    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.cap);
        d
    }
    pub fn reset(&mut self) {
        self.next = self.base;
    }
}

async fn sample(probe: &SharedProbe) -> Result<Probed, crate::errors::TelemetryError> {
    let probe = Arc::clone(probe);
    tokio::task::spawn_blocking(move || {
        probe
            .lock()
            .expect("probe lock is never held across a panic")
            .sample()
    })
    .await
    .expect("probe task does not panic")
}

/// How long an attach waits for its agent's session, and how often it is retried.
const PENDING_CAP: Duration = Duration::from_secs(600);
const PENDING_RETRY: Duration = Duration::from_millis(500);

struct Session<'a, H> {
    cfg: &'a SessionCfg,
    host: &'a H,
    probe: &'a SharedProbe,
    last: Probed,
    known: HashSet<AgentId>,
    attached: HashMap<AgentId, JoinHandle<()>>,
    /// Spawns in flight (clone/install running; the tmux session may not exist yet).
    starting: HashSet<AgentId>,
    /// Attaches that arrived while their agent was starting, with their arrival time.
    pending: HashMap<AgentId, Instant>,
    out: mpsc::UnboundedSender<DaemonToHub>,
    updates: Option<Arc<Updater>>,
    /// Background jobs (index refresh, clean, maintenance). The mutex is never locked (only
    /// `get_mut`): it makes the session `Sync`, which its `&self` awaits need.
    jobs: Mutex<Jobs<'a>>,
    /// An index refresh is in flight / another one was asked for meanwhile.
    indexing: bool,
    reindex: bool,
    sent_profiles: Option<Vec<ProfileInfo>>,
    sent_repos: Option<Vec<RepoEntry>>,
}

/// Spawns in flight (an install can take minutes; the loop must keep serving meanwhile).
type Spawns<'a> = FuturesUnordered<BoxFuture<'a, (AgentId, Result<(), AgentError>)>>;
type Jobs<'a> = FuturesUnordered<BoxFuture<'a, Done>>;

impl<'a, H: AgentHost> Session<'a, H> {
    fn hello(&self) -> DaemonToHub {
        let mut agents: Vec<AgentId> = self.known.iter().cloned().collect();
        agents.sort();
        DaemonToHub::Hello {
            node: NodeInfo::new(
                self.cfg.node_id.clone(),
                self.cfg.hostname.clone(),
                self.last.state.clone(),
                self.last.telemetry.clone(),
                agents,
            ),
        }
    }

    // ponytail: unbounded so a handler never blocks on its own loop; bound it if a slow hub piles up pane output.
    async fn emit(&self, m: DaemonToHub) {
        // the receiver lives in the same loop; it only closes when the session ends
        let _ = self.out.send(m);
    }

    async fn status(&self, agent: &AgentId, status: AgentStatus) {
        self.emit(DaemonToHub::AgentStatus {
            agent: agent.clone(),
            status,
        })
        .await;
    }

    async fn failed(&self, agent: &AgentId, reason: &str) {
        self.emit(DaemonToHub::AgentFailed {
            agent: agent.clone(),
            reason: ulak_proto::sanitize_reason(reason),
        })
        .await;
    }

    /// Re-reads profiles and the repo index in the background; at most one refresh runs, and a
    /// request during it schedules exactly one more.
    fn refresh(&mut self) {
        if self.indexing {
            self.reindex = true;
            return;
        }
        self.indexing = true;
        let host = self.host;
        self.jobs().push(
            async move { Done::Indexed(host.profiles().await, host.repo_index().await) }.boxed(),
        );
    }

    fn jobs(&mut self) -> &mut Jobs<'a> {
        self.jobs
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn report(&self, dry_run: bool, c: Cleaned) {
        let _ = self.out.send(DaemonToHub::CleanReport {
            dry_run,
            removed: c.removed,
            kept: c.kept,
        });
    }

    /// A background job finished: send what changed.
    fn done(&mut self, d: Done) {
        match d {
            Done::Indexed(profiles, repos) => {
                self.indexing = false;
                if profiles.is_some() && profiles != self.sent_profiles {
                    let _ = self.out.send(DaemonToHub::Profiles {
                        profiles: profiles.clone().unwrap_or_default(),
                    });
                    self.sent_profiles = profiles;
                }
                if repos.is_some() && repos != self.sent_repos {
                    let _ = self.out.send(DaemonToHub::RepoIndex {
                        repos: repos.clone().unwrap_or_default(),
                    });
                    self.sent_repos = repos;
                }
                if std::mem::take(&mut self.reindex) {
                    self.refresh();
                }
            }
            Done::Cleaned(dry_run, c) => {
                match c {
                    Some(c) => self.report(dry_run, c),
                    None => eprintln!("ulakd: clean requested but this node cannot clean"),
                }
                self.refresh();
            }
            Done::Maintained(c) => {
                if let Some(c) = c.filter(|c| !c.removed.is_empty()) {
                    self.report(false, c);
                }
                self.refresh();
            }
        }
    }

    fn clean(&mut self, req: CleanReq) {
        let host = self.host;
        self.jobs().push(
            async move {
                let dry = req.dry_run();
                Done::Cleaned(dry, host.clean(req).await)
            }
            .boxed(),
        );
    }

    fn maintain(&mut self) {
        let host = self.host;
        self.jobs()
            .push(async move { Done::Maintained(host.maintain().await) }.boxed());
    }

    fn forget(&mut self, id: &AgentId) {
        self.known.remove(id);
        self.pending.remove(id);
        if let Some(h) = self.attached.remove(id) {
            h.abort();
        }
    }

    /// The agent runs but never got its prompt: report Failed (the hub revokes its token) and
    /// stop the session so no orphan keeps running.
    async fn prompt_failed(&mut self, id: &AgentId) {
        eprintln!("ulakd: prompt for {id:?} was not typed; reporting Failed");
        if let Err(e) = self.host.kill(id).await {
            eprintln!("ulakd: kill {id:?} after prompt failure: {e}");
        }
        self.forget(id);
        self.failed(id, "the prompt could not be typed into the agent")
            .await;
        self.status(id, AgentStatus::Failed).await;
        self.emit(self.hello()).await;
    }

    /// Applies the hub's `Config` to the host, the probe and the updater, then re-sends what
    /// changed. Never logs env values.
    fn config(&mut self, profiles: &[HubProfile], settings: &NodeSettings) {
        self.host.apply_config(profiles, settings);
        self.probe
            .lock()
            .expect("probe lock is never held across a panic")
            .set_busy_pct(settings.cpu_busy_pct, settings.gpu_busy_pct);
        if let Some(u) = &self.updates {
            u.set_auto(settings.auto_update);
        }
        let s = settings;
        let overrides = [
            s.repo_roots.is_some(),
            s.index_interval_mins.is_some(),
            s.clean_auto.is_some(),
            s.clean_after_exit_hours.is_some(),
            s.auto_update.is_some(),
            s.cpu_busy_pct.is_some(),
            s.gpu_busy_pct.is_some(),
        ]
        .into_iter()
        .filter(|x| *x)
        .count();
        eprintln!(
            "ulakd: hub config: {} profile(s), {overrides} setting override(s)",
            profiles.len()
        );
        self.refresh();
    }

    async fn handle(&mut self, cmd: HubToDaemon, spawns: &mut Spawns<'a>) {
        match cmd {
            HubToDaemon::Config { profiles, settings } => self.config(&profiles, &settings),
            HubToDaemon::Spawn(spec) => {
                let id = spec.agent_id().clone();
                self.status(&id, AgentStatus::Starting).await;
                self.starting.insert(id.clone());
                let host = self.host;
                spawns.push(
                    async move {
                        let r = host.spawn(&spec).await;
                        (spec.agent_id().clone(), r)
                    }
                    .boxed(),
                );
            }
            HubToDaemon::GitAccess { agent, token } => self.host.git_access(agent, token),
            HubToDaemon::BlockerResolved { profile } => {
                self.host.blocker_resolved(&profile);
                self.refresh();
            }
            HubToDaemon::Kill { agent } => match self.host.kill(&agent).await {
                Ok(()) => {
                    self.forget(&agent);
                    self.status(&agent, AgentStatus::Exited(None)).await;
                    self.emit(self.hello()).await;
                    self.refresh();
                }
                Err(e) => eprintln!("ulakd: kill {agent:?} failed: {e}"),
            },
            HubToDaemon::Nudge { agent, text } => {
                if let Err(e) = self.host.nudge(&agent, &text).await {
                    eprintln!("ulakd: nudge {agent:?} failed: {e}");
                }
            }
            HubToDaemon::PaneInput { agent, data } => {
                if let Err(e) = self.host.input(&agent, &data).await {
                    eprintln!("ulakd: input {agent:?} failed: {e}");
                }
            }
            HubToDaemon::Attach { agent } => self.attach(agent).await,
            HubToDaemon::Detach { agent } => {
                self.pending.remove(&agent);
                if let Some(h) = self.attached.remove(&agent) {
                    h.abort();
                }
            }
            HubToDaemon::CheckUpdate => {
                if let Some(u) = self.updates.clone() {
                    tokio::spawn(async move { u.on_check().await });
                }
            }
            HubToDaemon::UpdateNow => {
                if let Some(u) = self.updates.clone() {
                    tokio::spawn(async move { u.on_update_now().await });
                }
            }
            HubToDaemon::CleanRepos { dry_run } => self.clean(CleanReq::All { dry_run }),
            HubToDaemon::CleanRepo { path } => self.clean(CleanReq::One { path }),
        }
    }

    /// Starts the pane stream. While the agent is still starting, a refused attach is kept
    /// pending and retried (see `retry_pending`); for any other agent it is an error.
    async fn attach(&mut self, agent: AgentId) {
        match self.host.attach(&agent).await {
            Ok(mut rx) => {
                self.pending.remove(&agent);
                let (out, id) = (self.out.clone(), agent.clone());
                let task = tokio::spawn(async move {
                    while let Some(data) = rx.recv().await {
                        if out
                            .send(DaemonToHub::PaneOutput {
                                agent: id.clone(),
                                data,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                if let Some(old) = self.attached.insert(agent, task) {
                    old.abort();
                }
            }
            Err(_) if self.starting.contains(&agent) => {
                self.pending.entry(agent).or_insert_with(Instant::now);
            }
            Err(e) => eprintln!("ulakd: attach {agent:?} failed: {e}"),
        }
    }

    /// Retries pending attaches (the session may exist before the spawn call returns) and
    /// drops those older than [`PENDING_CAP`].
    async fn retry_pending(&mut self) {
        self.pending.retain(|_, t| t.elapsed() < PENDING_CAP);
        let ids: Vec<AgentId> = self.pending.keys().cloned().collect();
        for id in ids {
            self.attach(id).await;
        }
    }

    async fn spawned(&mut self, id: AgentId, r: Result<(), AgentError>) {
        self.starting.remove(&id);
        self.refresh();
        match r {
            Ok(()) => {
                self.known.insert(id.clone());
                self.status(&id, AgentStatus::Running).await;
                self.emit(self.hello()).await;
                if self.pending.contains_key(&id) {
                    self.attach(id.clone()).await;
                    self.pending.remove(&id); // still refused: give up, the UI can re-attach
                }
            }
            Err(e) => {
                self.pending.remove(&id);
                eprintln!("ulakd: spawn {id:?} failed: {e}");
                self.failed(&id, &e.to_string()).await;
                self.status(&id, AgentStatus::Failed).await;
            }
        }
    }

    async fn host_event(&mut self, e: HostEvent) {
        match e {
            HostEvent::Status { agent, status } => self.status(&agent, status).await,
            HostEvent::Blocked {
                agent,
                profile,
                kind,
                detail,
            } => {
                self.emit(DaemonToHub::AgentBlocked {
                    agent,
                    profile,
                    kind,
                    detail,
                })
                .await;
                self.refresh();
            }
        }
    }

    /// One heartbeat tick: Hello if the state or agent set changed, else Heartbeat.
    async fn tick(&mut self, probe: &SharedProbe) {
        let before = (self.last.state.clone(), self.known.clone());
        match sample(probe).await {
            Ok(p) => self.last = p,
            Err(e) => {
                eprintln!("ulakd: telemetry failed: {e}");
                return;
            }
        }
        if let Ok(now) = self.host.list().await {
            let now: HashSet<AgentId> = now.into_iter().collect();
            let mut gone: Vec<AgentId> = self.known.difference(&now).cloned().collect();
            gone.sort();
            if !gone.is_empty() {
                self.refresh();
            }
            for id in gone {
                self.forget(&id);
                self.status(&id, AgentStatus::Exited(None)).await;
            }
            self.known.extend(now);
        }
        if before == (self.last.state.clone(), self.known.clone()) {
            self.emit(DaemonToHub::Heartbeat(self.last.telemetry.clone()))
                .await;
        } else {
            self.emit(self.hello()).await;
        }
    }
}

/// Runs one connection until the hub closes it. Sends Hello first.
pub async fn run_session<S, H>(
    ws: WebSocketStream<S>,
    cfg: &SessionCfg,
    host: &H,
    probe: &SharedProbe,
) -> Result<(), HubError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    H: AgentHost,
{
    run_session_with(ws, cfg, host, probe, None).await
}

/// [`run_session`] with a self-updater: sends `Version` right after Hello, mirrors its
/// `UpdateStatus` to the hub and handles `CheckUpdate` / `UpdateNow`.
pub async fn run_session_with<S, H>(
    ws: WebSocketStream<S>,
    cfg: &SessionCfg,
    host: &H,
    probe: &SharedProbe,
    updates: Option<Arc<Updater>>,
) -> Result<(), HubError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    H: AgentHost,
{
    let (mut sink, mut stream) = ws.split();
    let (out, mut out_rx) = mpsc::unbounded_channel::<DaemonToHub>();
    let known: HashSet<AgentId> = host.list().await.unwrap_or_default().into_iter().collect();
    let mut s = Session {
        cfg,
        host,
        probe,
        last: sample(probe).await?,
        known,
        attached: HashMap::new(),
        starting: HashSet::new(),
        pending: HashMap::new(),
        out,
        updates: updates.clone(),
        jobs: Mutex::new(Jobs::new()),
        indexing: false,
        reindex: false,
        sent_profiles: None,
        sent_repos: None,
    };
    let mut spawns = Spawns::new();
    sink.send(Message::text(encode(&s.hello())?)).await?;
    let mut status_rx = updates.as_ref().map(|u| u.subscribe());
    if let Some(u) = &updates {
        sink.send(Message::text(encode(&DaemonToHub::Version(
            u.info().clone(),
        ))?))
        .await?;
    }
    if let Some(Some(st)) = status_rx.as_mut().map(|r| r.borrow_and_update().clone()) {
        sink.send(Message::text(encode(&DaemonToHub::UpdateStatus(st))?))
            .await?;
    }
    s.refresh(); // its frames follow Hello, Version and UpdateStatus
    let mut upkeep = tokio::time::interval(host.maintenance_interval());
    upkeep.tick().await; // the first tick fires immediately; the refresh above covers it
    let mut tick = tokio::time::interval(cfg.heartbeat);
    let mut retry = tokio::time::interval(PENDING_RETRY);
    retry.tick().await;
    tick.tick().await; // the first tick fires immediately; Hello already covered it
    let result = loop {
        tokio::select! {
            msg = stream.next() => match msg {
                None | Some(Ok(Message::Close(_))) => break Ok(()),
                Some(Err(e)) => break Err(e.into()),
                Some(Ok(Message::Text(t))) => match decode::<HubToDaemon>(t.as_str()) {
                    Ok(cmd) => {
                        s.handle(cmd, &mut spawns).await;
                        // a hub Config may change the interval
                        let d = host.maintenance_interval();
                        if d != upkeep.period() {
                            upkeep = tokio::time::interval_at(tokio::time::Instant::now() + d, d);
                        }
                    }
                    Err(e) => eprintln!("ulakd: bad hub frame: {e}"),
                },
                Some(Ok(_)) => {}
            },
            _ = tick.tick() => s.tick(probe).await,
            _ = retry.tick(), if !s.pending.is_empty() => s.retry_pending().await,
            Some(id) = host.next_prompt_failure() => s.prompt_failed(&id).await,
            Some(e) = host.next_event() => s.host_event(e).await,
            Some((id, r)) = spawns.next() => s.spawned(id, r).await,
            Some(d) = s.jobs.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).next() => s.done(d),
            _ = upkeep.tick() => s.maintain(),
            Some(()) = async {
                match status_rx.as_mut() {
                    Some(r) => r.changed().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                let st = status_rx.as_mut().and_then(|r| r.borrow_and_update().clone());
                if let Some(st) = st {
                    s.emit(DaemonToHub::UpdateStatus(st)).await;
                }
            }
            Some(m) = out_rx.recv() => {
                if let Err(e) = sink.send(Message::text(encode(&m)?)).await {
                    break Err(e.into());
                }
            }
        }
    };
    for h in s.attached.values() {
        h.abort();
    }
    result
}

/// Connects and reconnects forever (with capped backoff).
pub async fn run_forever<H: AgentHost>(
    id: &Identity,
    hostname: String,
    heartbeat: Duration,
    host: &H,
    probe: SharedProbe,
) -> Result<std::convert::Infallible, HubError> {
    run_forever_in(id, None, hostname, heartbeat, host, probe, None).await
}

/// Like [`run_forever`]; with `dir` it also renews the certificate (at start and every
/// [`renew::CHECK_EVERY`]) and reconnects with the new one.
pub async fn run_forever_in<H: AgentHost>(
    id: &Identity,
    dir: Option<&Path>,
    hostname: String,
    heartbeat: Duration,
    host: &H,
    probe: SharedProbe,
    updates: Option<Arc<Updater>>,
) -> Result<std::convert::Infallible, HubError> {
    let url = daemon_ws_url(&id.hub_url)?;
    let cfg = SessionCfg {
        node_id: id.node_id.clone(),
        hostname,
        heartbeat,
    };
    let mut id = id.clone();
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
    let try_renew = |id: Identity| async move {
        let Some(dir) = dir else { return id };
        match renew::renew_if_due(dir, &id).await {
            Ok(Some(new)) => {
                eprintln!("ulakd: certificate renewed");
                new
            }
            Ok(None) => id,
            Err(e) => {
                eprintln!("ulakd: certificate renewal failed: {e}");
                id
            }
        }
    };
    id = try_renew(id).await;
    loop {
        let tls = url
            .starts_with("wss://")
            .then(|| client_tls(&id.ca_pem, &id.cert_pem, &id.key_pem))
            .transpose()?;
        match connect(&url, tls).await {
            Ok(ws) => {
                let started = Instant::now();
                let session = run_session_with(ws, &cfg, host, &probe, updates.clone());
                tokio::pin!(session);
                let r = loop {
                    tokio::select! {
                        r = &mut session => break Some(r),
                        _ = tokio::time::sleep(renew::CHECK_EVERY), if dir.is_some() => {
                            let new = try_renew(id.clone()).await;
                            if new.cert_pem != id.cert_pem {
                                id = new;
                                break None; // reconnect with the new certificate
                            }
                        }
                    }
                };
                if let Some(Err(e)) = r {
                    eprintln!("ulakd: session ended: {e}");
                }
                if started.elapsed() >= Duration::from_secs(10) {
                    backoff.reset();
                }
            }
            Err(e) => eprintln!("ulakd: connect to {url} failed: {e}"),
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_cap_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let v: Vec<u64> = (0..7).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(v, [1, 2, 4, 8, 16, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }
}
