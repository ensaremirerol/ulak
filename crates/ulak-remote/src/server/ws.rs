use super::config::{check_name, check_profile, check_settings, MAX_HUB_PROFILES};
use super::hub::{now_ms, Hub, Peer};
use crate::*;
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        RawQuery, State,
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
    Extension,
};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::{broadcast, mpsc};
use ulak_proto::*;

pub(crate) async fn daemon(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    ws: WebSocketUpgrade,
) -> Response {
    match h.authorize(&p) {
        Ok(id) => ws.on_upgrade(move |s| daemon_loop(h, id, s)),
        Err(code) => code.into_response(),
    }
}

pub(crate) async fn ui(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Cross-site WebSocket hijacking: a browser always sends Origin, and cookies ride along
    // automatically, so a present Origin must be ours unless the caller brought a bearer token.
    if let Some(o) = headers.get(axum::http::header::ORIGIN) {
        let bearer = headers.contains_key(axum::http::header::AUTHORIZATION);
        let ours = h.origin.get().map(|s| s.trim_end_matches('/'));
        if !bearer && ours != o.to_str().ok() {
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
    }
    // A UI client cert or a passkey session; both get the same capabilities.
    match h.ui_caller(&p, &headers, q.as_deref()) {
        Ok(id) => {
            // A logout closes only this session's sockets.
            let sess = super::web::token_of(&headers, q.as_deref())
                .map_or_else(|| id.clone(), |t| super::web::session_key(&t));
            ws.on_upgrade(move |s| ui_loop(h, id, sess, s))
        }
        Err(code) => code.into_response(),
    }
}

async fn send<T: serde::Serialize>(s: &mut WebSocket, m: &T) -> bool {
    match encode(m) {
        Ok(t) => s.send(Message::Text(t.into())).await.is_ok(),
        Err(_) => true,
    }
}

/// Resolves when `id` is revoked.
async fn kicked(rx: &mut broadcast::Receiver<NodeId>, a: &NodeId, b: &NodeId) {
    loop {
        match rx.recv().await {
            Ok(k) if &k == a || &k == b => return,
            Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
            _ => {}
        }
    }
}

async fn daemon_loop(h: Arc<Hub>, id: NodeId, mut s: WebSocket) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut kick = h.kick.subscribe();
    h.daemons
        .lock()
        .expect("daemon map lock is never held across a panic")
        .insert(id.clone(), tx.clone());
    h.set_connected(&id, Some(now_ms()));
    loop {
        tokio::select! {
            m = s.recv() => match m {
                Some(Ok(Message::Text(t))) => match decode::<DaemonToHub>(&t).map_err(ServerError::from).and_then(|m| on_daemon(&h, &id, m)) {
                    Err(ServerError::SpoofedHello) => break,
                    Err(e @ ServerError::DaemonTooOld { .. }) => {
                        h.ui_send(HubToUi::Error { message: e.to_string() });
                        let frame = CloseFrame { code: 1008, reason: e.to_string().into() };
                        let _ = s.send(Message::Close(Some(frame))).await;
                        break;
                    }
                    Err(e) => eprintln!("daemon {}: {e}", id.as_str()),
                    Ok(()) => {}
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            Some(out) = rx.recv() => if !send(&mut s, &out).await { break },
            () = kicked(&mut kick, &id, &id) => break,
        }
    }
    let mut daemons = h
        .daemons
        .lock()
        .expect("daemon map lock is never held across a panic");
    // A newer connection from the same node owns the slot now.
    if daemons.get(&id).is_some_and(|t| t.same_channel(&tx)) {
        daemons.remove(&id);
        drop(daemons);
        h.set_connected(&id, None);
        let meta = h
            .versions
            .lock()
            .expect("versions lock is never held across a panic")
            .remove(&id);
        h.registry.set_offline(&id);
        h.node_update(&id);
        // Only what was reported is cleared, so older daemons cause no extra frames.
        if let Some(m) = meta {
            if m.profiles.is_some_and(|p| !p.is_empty()) {
                h.ui_send(HubToUi::NodeProfiles {
                    node: id.clone(),
                    profiles: vec![],
                });
            }
            if m.repos.is_some_and(|r| !r.is_empty()) {
                h.ui_send(HubToUi::NodeRepos {
                    node: id.clone(),
                    repos: vec![],
                });
            }
        }
    }
}

fn on_daemon(h: &Hub, id: &NodeId, m: DaemonToHub) -> Result<(), ServerError> {
    match m {
        // The node is always the authenticated daemon; oversize reports are dropped whole.
        DaemonToHub::Profiles { profiles } => {
            if profiles.len() > MAX_PROFILES || !profiles.iter().all(profile_fits) {
                return Err(ServerError::BadArgs(
                    "oversize profiles report dropped".into(),
                ));
            }
            h.meta(id, |m| m.profiles = Some(profiles.clone()));
            h.ui_send(HubToUi::NodeProfiles {
                node: id.clone(),
                profiles,
            });
            h.queue_kick.notify_one();
        }
        DaemonToHub::RepoIndex { repos } => {
            if repos.len() > MAX_REPOS || !repos.iter().all(repo_fits) {
                return Err(ServerError::BadArgs("oversize repo index dropped".into()));
            }
            h.meta(id, |m| m.repos = Some(repos.clone()));
            h.ui_send(HubToUi::NodeRepos {
                node: id.clone(),
                repos,
            });
        }
        DaemonToHub::InstallStatus {
            name,
            done,
            ok,
            detail,
        } => {
            if name.chars().count() > 64 {
                return Err(ServerError::BadArgs(
                    "oversize install status dropped".into(),
                ));
            }
            h.ui_send(HubToUi::InstallStatus {
                node: id.clone(),
                name,
                done,
                ok,
                detail: ulak_proto::sanitize_reason(&detail),
            });
        }
        DaemonToHub::CleanReport {
            dry_run,
            removed,
            kept,
        } => {
            if removed.len() + kept.len() > MAX_CLEAN_ITEMS
                || !removed
                    .iter()
                    .chain(&kept)
                    .all(|c| short(&c.path) && short(&c.reason))
            {
                return Err(ServerError::BadArgs("oversize clean report dropped".into()));
            }
            h.ui_send(HubToUi::CleanReport {
                node: id.clone(),
                dry_run,
                removed,
                kept,
            });
        }
        DaemonToHub::Hello { node } => {
            if &node.id != id {
                return Err(ServerError::SpoofedHello);
            }
            h.record_telemetry(id, node.telemetry.clone(), now_ms());
            h.registry.upsert(node.clone());
            h.ui_send(HubToUi::NodeUpdate(node));
            h.queue_kick.notify_one();
        }
        DaemonToHub::Heartbeat(t) => {
            h.record_telemetry(id, t.clone(), now_ms());
            h.registry.heartbeat(id, t);
            h.node_update(id);
            h.queue_kick.notify_one();
        }
        DaemonToHub::AgentStatus { agent, status } => {
            h.ui_send(HubToUi::AgentStatus {
                agent: agent.clone(),
                node: id.clone(),
                status: status.clone(),
            });
            h.set_status(agent.clone(), id.clone(), status.clone());
            if matches!(status, AgentStatus::Exited(_) | AgentStatus::Failed) {
                AgentTokens::revoke_agent(&*h.store, &agent)?;
                h.queue_kick.notify_one();
            }
        }
        DaemonToHub::AgentFailed { agent, reason } => {
            let reason = sanitize_reason(&reason);
            h.set_failure(agent.clone(), reason.clone());
            h.ui_send(HubToUi::AgentFailed {
                agent,
                node: id.clone(),
                reason,
            });
        }
        DaemonToHub::PaneOutput { agent, data } => h.pane_output(agent, data),
        DaemonToHub::PaneStream { agent, data } => h.pane_stream(id, agent, data),
        DaemonToHub::Version(info) => {
            if info.proto() < MIN_DAEMON_PROTO {
                return Err(ServerError::DaemonTooOld {
                    node: id.as_str().to_owned(),
                    got: info.proto(),
                    min: MIN_DAEMON_PROTO,
                });
            }
            h.meta(id, |m| m.version = Some(info.clone()));
            h.ui_send(HubToUi::NodeVersion {
                node: id.clone(),
                info,
            });
            // Only now: the daemon has identified itself.
            h.push_config(id)?;
        }
        DaemonToHub::UpdateStatus(status) => {
            h.meta(id, |m| m.update = Some(status.clone()));
            h.ui_send(HubToUi::UpdateStatus {
                node: id.clone(),
                status,
            });
        }
        // The node is always the authenticated daemon, never taken from the payload.
        DaemonToHub::AgentBlocked {
            agent,
            profile,
            kind,
            detail,
        } => {
            let (b, new) = Blockers::open(
                &*h.store,
                id,
                &profile,
                kind,
                Some(&agent),
                detail,
                now_ms(),
            )?;
            if new {
                h.ui_send(HubToUi::BlockerOpened(b));
            }
        }
    }
    Ok(())
}

async fn ui_loop(h: Arc<Hub>, id: NodeId, sess: NodeId, mut s: WebSocket) {
    let mut rx = h.ui.subscribe();
    let mut kick = h.kick.subscribe();
    let conn = h.new_conn();
    let (tx, mut own) = mpsc::unbounded_channel();
    // Stream bytes handed to `own` but not yet written to this socket (see `Hub::pane_stream`).
    let queued = Arc::new(AtomicUsize::new(0));
    let snap = Questions::pending(&*h.store).map(|questions| HubToUi::Snapshot {
        nodes: h.registry.snapshot(),
        questions,
    });
    let snap = snap.unwrap_or_else(|e| HubToUi::Error {
        message: e.to_string(),
    });
    let open = Blockers::list_open(&*h.store).map_or_else(
        |e| HubToUi::Error {
            message: e.to_string(),
        },
        |blockers| HubToUi::Blockers { blockers },
    );
    if !send(&mut s, &snap).await
        || !send(&mut s, &open).await
        || !send(&mut s, &HubToUi::HubVersion(crate::version_info())).await
        || (h.cfg.release_dir.is_some() && !send(&mut s, &h.release_frame()).await)
    {
        return;
    }
    let config = h.hub_config().unwrap_or_else(|e| HubToUi::Error {
        message: e.to_string(),
    });
    if !send(&mut s, &config).await {
        return;
    }
    let known: Vec<_> = h
        .versions
        .lock()
        .expect("versions lock is never held across a panic")
        .iter()
        .map(|(n, m)| (n.clone(), m.clone()))
        .collect();
    for (node, m) in known {
        if let Some(info) = m.version {
            if !send(
                &mut s,
                &HubToUi::NodeVersion {
                    node: node.clone(),
                    info,
                },
            )
            .await
            {
                return;
            }
        }
        if let Some(status) = m.update {
            if !send(&mut s, &HubToUi::UpdateStatus { node, status }).await {
                return;
            }
        }
    }
    let statuses = h.statuses();
    if !statuses.is_empty() && !send(&mut s, &HubToUi::AgentStatuses { statuses }).await {
        return;
    }
    for f in h.failed_frames() {
        if !send(&mut s, &f).await {
            return;
        }
    }
    match Queue::queue_list(&*h.store) {
        Ok(q) if !q.is_empty() => {
            let items = q.into_iter().map(|e| e.item).collect();
            if !send(&mut s, &HubToUi::Queue { items }).await {
                return;
            }
        }
        _ => {}
    }
    let reports: Vec<_> = h
        .versions
        .lock()
        .expect("versions lock is never held across a panic")
        .iter()
        .map(|(n, m)| (n.clone(), m.profiles.clone(), m.repos.clone()))
        .collect();
    for (node, profiles, repos) in reports {
        if let Some(profiles) = profiles.filter(|p| !p.is_empty()) {
            let node = node.clone();
            if !send(&mut s, &HubToUi::NodeProfiles { node, profiles }).await {
                return;
            }
        }
        if let Some(repos) = repos.filter(|r| !r.is_empty()) {
            if !send(&mut s, &HubToUi::NodeRepos { node, repos }).await {
                return;
            }
        }
    }
    loop {
        tokio::select! {
            m = s.recv() => match m {
                Some(Ok(Message::Text(t))) => {
                    let r = decode::<UiToHub>(&t).map_err(ServerError::from).and_then(|m| on_ui(&h, conn, &tx, &queued, m));
                    if let Err(e) = r {
                        if !send(&mut s, &HubToUi::Error { message: e.to_string() }).await { break }
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            out = rx.recv() => match out {
                Ok(m) => if !send(&mut s, &m).await { break },
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Some(m) = own.recv() => {
                if !send(&mut s, &m).await { break }
                if let HubToUi::PaneStream { data, .. } = &m {
                    queued.fetch_sub(data.len(), Ordering::Relaxed);
                }
            }
            () = kicked(&mut kick, &id, &sess) => break,
        }
    }
    h.detach_all(conn);
}

/// Runs blocking work (GitHub, crypto, sqlite) off the socket task; the reply goes to the
/// requester only, an error becomes an `Error` frame.
fn reply_blocking(
    h: &Arc<Hub>,
    tx: &mpsc::UnboundedSender<HubToUi>,
    f: impl FnOnce(&Hub) -> Result<HubToUi, ServerError> + Send + 'static,
) {
    let out = tx.clone();
    run_blocking(h, tx, move |h| {
        let _ = out.send(f(h)?);
        Ok(())
    });
}

/// `reply_blocking` for work that replies by broadcast: only an error goes to the requester.
fn run_blocking(
    h: &Arc<Hub>,
    tx: &mpsc::UnboundedSender<HubToUi>,
    f: impl FnOnce(&Hub) -> Result<(), ServerError> + Send + 'static,
) {
    let (h, tx) = (h.clone(), tx.clone());
    tokio::spawn(async move {
        let message = match tokio::task::spawn_blocking(move || f(&h)).await {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e.to_string(),
            Err(_) => "internal error".into(),
        };
        let _ = tx.send(HubToUi::Error { message });
    });
}

fn credentials(h: &Hub) -> Result<HubToUi, ServerError> {
    Ok(HubToUi::Credentials {
        credentials: Credentials::list_credentials(&*h.store)?,
    })
}

fn on_ui(
    h: &Arc<Hub>,
    conn: u64,
    tx: &mpsc::UnboundedSender<HubToUi>,
    queued: &Arc<AtomicUsize>,
    m: UiToHub,
) -> Result<(), ServerError> {
    let now = now_ms();
    match m {
        UiToHub::SaveProfile { node, profile } => {
            check_profile(&profile)?;
            run_blocking(h, tx, move |h| {
                if let Some(n) = &node {
                    h.known_node(n)?;
                }
                h.config_change(node.as_ref(), |s| {
                    s.upsert_hub_profile(node.as_ref(), &profile, MAX_HUB_PROFILES)
                })
            });
        }
        UiToHub::DeleteProfile { node, name } => {
            check_name(&name)?;
            run_blocking(h, tx, move |h| {
                h.config_change(node.as_ref(), |s| {
                    s.delete_hub_profile(node.as_ref(), &name)
                })
            });
        }
        UiToHub::SaveNodeSettings { node, settings } => {
            check_settings(&settings)?;
            run_blocking(h, tx, move |h| {
                // Clearing a revoked node's leftovers is allowed.
                if settings != NodeSettings::default() {
                    h.known_node(&node)?;
                }
                h.config_change(Some(&node), |s| s.save_node_settings(&node, &settings))
            });
        }
        UiToHub::NodeStats { node } => {
            let _ = tx.send(h.node_stats(node, now));
        }
        UiToHub::Spawn { target, spec } => match h.place(&target, &spec, now, &HashSet::new()) {
            Ok(_) => {}
            // Nothing can take it now: wait for a node (pinned to the one asked for).
            Err(ServerError::Schedule(
                ScheduleError::NoEligibleNode | ScheduleError::NodeNotEligible(_),
            )) => h.enqueue(target, spec)?,
            Err(e) => return Err(e),
        },
        UiToHub::Answer(a) => {
            let id = a.question().clone();
            Questions::answer(&*h.store, a, now)?;
            h.ui_send(HubToUi::QuestionClosed { id: id.clone() });
            // No waiting ask_human is fine.
            let _ = h.answered.send(id);
        }
        UiToHub::Attach { agent } => {
            let node = h.node_of_agent(&agent)?;
            h.to_daemon(
                &node,
                HubToDaemon::Attach {
                    agent: agent.clone(),
                },
            )?;
            h.attach(conn, tx, &agent);
        }
        UiToHub::AttachStream { agent, cols, rows } => {
            let node = h.node_of_agent(&agent)?;
            h.attach_stream(conn, tx, queued, &agent, (cols, rows));
            // Also for a second viewer: the node restarts its client, so this one gets a full repaint.
            h.to_daemon(&node, HubToDaemon::AttachStream { agent, cols, rows })?;
        }
        UiToHub::Detach { agent } => {
            if h.detach_stream(conn, &agent) {
                if let Ok(node) = h.node_of_agent(&agent) {
                    h.to_daemon(
                        &node,
                        HubToDaemon::DetachStream {
                            agent: agent.clone(),
                        },
                    )?;
                }
            }
            if h.detach(conn, &agent) {
                let node = h.node_of_agent(&agent)?;
                h.to_daemon(&node, HubToDaemon::Detach { agent })?;
            }
        }
        UiToHub::Resize { agent, cols, rows } => {
            h.stream_resized(&agent, (cols, rows));
            let node = h.node_of_agent(&agent)?;
            h.to_daemon(&node, HubToDaemon::Resize { agent, cols, rows })?;
        }
        UiToHub::PaneInput { agent, data } => {
            let node = h.node_of_agent(&agent)?;
            h.to_daemon(&node, HubToDaemon::PaneInput { agent, data })?;
        }
        UiToHub::Kill { agent } => {
            match h.node_of_agent(&agent) {
                Ok(node) => h.to_daemon(
                    &node,
                    HubToDaemon::Kill {
                        agent: agent.clone(),
                    },
                )?,
                // Never started (e.g. waiting on a failed install) or its node is gone: nothing
                // runs, so the hub closes it itself; a connected node still gets the Kill.
                Err(e) => {
                    let known = h.statuses().into_iter().find(|(a, ..)| *a == agent);
                    let Some((_, node, _)) = known else {
                        return Err(e);
                    };
                    let _ = h.to_daemon(
                        &node,
                        HubToDaemon::Kill {
                            agent: agent.clone(),
                        },
                    );
                    h.ui_send(HubToUi::AgentStatus {
                        agent: agent.clone(),
                        node: node.clone(),
                        status: AgentStatus::Exited(None),
                    });
                    h.set_status(agent.clone(), node, AgentStatus::Exited(None));
                }
            }
            AgentTokens::revoke_agent(&*h.store, &agent)?;
            h.queue_kick.notify_one();
        }
        UiToHub::ResolveBlocker { id } => {
            let b = Blockers::resolve(&*h.store, &id, now)?;
            h.ui_send(HubToUi::BlockerClosed { id });
            h.queue_kick.notify_one();
            // Best effort: the node or agent may be gone.
            let _ = h.to_daemon(
                b.node(),
                HubToDaemon::BlockerResolved {
                    profile: b.profile().clone(),
                },
            );
            if let Some(agent) = b.agent() {
                // Constant text only, never user text.
                let _ = h.to_daemon(
                    b.node(),
                    HubToDaemon::Nudge {
                        agent: agent.clone(),
                        text: RESOLVED_NUDGE.into(),
                    },
                );
            }
        }
        UiToHub::CheckUpdate { node } => {
            // Refresh the hub's cache first (at most once a minute), then ask the nodes.
            let (h, tx) = (h.clone(), tx.clone());
            tokio::spawn(async move {
                h.refresh_releases(false).await;
                if let Err(e) = update_route(&h, node, || HubToDaemon::CheckUpdate) {
                    let _ = tx.send(HubToUi::Error {
                        message: e.to_string(),
                    });
                }
            });
        }
        UiToHub::UpdateNode { node } => update_route(h, node, || HubToDaemon::UpdateNow)?,
        UiToHub::InstallProfile { node, name } => {
            let nodes = match node {
                Some(n) => vec![n],
                None => h.installable_nodes(&name),
            };
            if nodes.is_empty() {
                return Err(ServerError::BadArgs(format!(
                    "no connected node can install {name}"
                )));
            }
            for n in nodes {
                h.to_daemon(&n, HubToDaemon::InstallProfile { name: name.clone() })?;
            }
        }
        UiToHub::CleanNode { node, dry_run } => {
            h.to_daemon(&node, HubToDaemon::CleanRepos { dry_run })?;
        }
        UiToHub::CleanRepo { node, path } => {
            // Only a managed clone the node itself indexed; the daemon checks again.
            let indexed = h
                .versions
                .lock()
                .expect("versions lock is never held across a panic")
                .get(&node)
                .and_then(|m| m.repos.as_ref())
                .is_some_and(|r| r.iter().any(|e| e.managed && e.path == path));
            if !indexed {
                return Err(ServerError::BadArgs(format!(
                    "{path:?} is not a managed clone indexed on {}",
                    node.as_str()
                )));
            }
            h.to_daemon(&node, HubToDaemon::CleanRepo { path })?;
        }
        UiToHub::AddDevice { kind } => {
            let o = h.enroller().begin(kind, now);
            // Secrets go to the asking UI only, never the broadcast.
            let _ = tx.send(HubToUi::OtpOffer {
                kind,
                otp: o.otp,
                hub_fingerprint: o.hub_fingerprint,
                expires_ms: o.expires_ms,
            });
        }
        UiToHub::RevokeDevice { id } => {
            h.revoke_device(&id, now)?;
            let _ = tx.send(devices(h)?);
        }
        UiToHub::ListDevices => {
            let _ = tx.send(devices(h)?);
        }
        UiToHub::MailboxLog { thread, limit } => {
            let messages = Mailbox::log(&*h.store, thread.as_ref(), limit.min(1000))?;
            let _ = tx.send(HubToUi::MailboxLog { messages });
        }
        UiToHub::AddCredential { name, token } => {
            let name = name.trim().to_owned();
            if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
                return Err(ServerError::BadArgs(
                    "credential name must be 1-100 printable characters".into(),
                ));
            }
            reply_blocking(h, tx, move |h| {
                let login = h.github.login(&token)?;
                let id = CredentialId::new(&uuid::Uuid::now_v7().to_string())
                    .map_err(CredentialError::from)?;
                let (nonce, ct) = h.secrets.seal(&id, &token)?;
                let info = CredentialInfo {
                    id,
                    name,
                    kind: CredentialKind::Github,
                    login,
                    created_ms: now_ms(),
                };
                Credentials::insert_credential(&*h.store, &info, &nonce, &ct)?;
                credentials(h)
            });
        }
        UiToHub::DeleteCredential { id } => {
            Credentials::delete_credential(&*h.store, &id)?;
            let _ = tx.send(credentials(h)?);
        }
        UiToHub::ListCredentials => {
            let _ = tx.send(credentials(h)?);
        }
        UiToHub::ListRepos {
            credential,
            query,
            page,
        } => {
            if query.as_ref().is_some_and(|q| q.chars().count() > 100) {
                return Err(ServerError::BadArgs("query is too long".into()));
            }
            reply_blocking(h, tx, move |h| {
                let token = h.git_token(&credential)?;
                let (repos, has_more) = h.github.repos(&token, query.as_deref(), page)?;
                Ok(HubToUi::Repos {
                    credential,
                    query,
                    repos,
                    has_more,
                })
            });
        }
        UiToHub::ListBranches { credential, repo } => {
            reply_blocking(h, tx, move |h| {
                let token = h.git_token(&credential)?;
                let (branches, default_branch) = h.github.branches(&token, &repo)?;
                Ok(HubToUi::Branches {
                    credential,
                    repo,
                    branches,
                    default_branch,
                })
            });
        }
        UiToHub::CancelQueued { id } => {
            Queue::cancel_queued(&*h.store, &id)?;
            h.ui_send(HubToUi::QueueRemoved { id });
        }
        UiToHub::ListQueue => {
            let items = Queue::queue_list(&*h.store)?
                .into_iter()
                .map(|e| e.item)
                .collect();
            let _ = tx.send(HubToUi::Queue { items });
        }
    }
    Ok(())
}

/// One node (error if it is not connected) or every connected node.
fn update_route(
    h: &Hub,
    node: Option<NodeId>,
    msg: impl Fn() -> HubToDaemon,
) -> Result<(), ServerError> {
    match node {
        Some(n) => h.to_daemon(&n, msg()),
        None => {
            let nodes: Vec<NodeId> = h
                .daemons
                .lock()
                .expect("daemon map lock is never held across a panic")
                .keys()
                .cloned()
                .collect();
            nodes.iter().for_each(|n| {
                // A node that vanished meanwhile is fine.
                let _ = h.to_daemon(n, msg());
            });
            Ok(())
        }
    }
}

const MAX_PROFILES: usize = 64;
const MAX_REPOS: usize = 200;
const MAX_CLEAN_ITEMS: usize = 400;

fn short(s: &str) -> bool {
    s.len() <= 1024
}
fn profile_fits(p: &ProfileInfo) -> bool {
    short(&p.name) && short(&p.binary) && p.config_dir.as_deref().is_none_or(short)
}
fn repo_fits(r: &RepoEntry) -> bool {
    short(&r.path) && r.branch.as_deref().is_none_or(short)
}

const RESOLVED_NUDGE: &str = "Login finished. Please continue.";

fn devices(h: &Hub) -> Result<HubToUi, ServerError> {
    let devices = Devices::list(&*h.store)?.iter().map(Into::into).collect();
    Ok(HubToUi::Devices { devices })
}
