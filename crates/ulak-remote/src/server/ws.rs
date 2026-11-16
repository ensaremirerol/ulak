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
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{broadcast, mpsc};
use ulak_proto::*;

pub(crate) async fn daemon(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    ws: WebSocketUpgrade,
) -> Response {
    match h.authorize(&p, DeviceKind::Node) {
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
        h.versions
            .lock()
            .expect("versions lock is never held across a panic")
            .remove(&id);
        h.registry.set_offline(&id);
        h.node_update(&id);
    }
}

fn on_daemon(h: &Hub, id: &NodeId, m: DaemonToHub) -> Result<(), ServerError> {
    match m {
        DaemonToHub::Hello { node } => {
            if &node.id != id {
                return Err(ServerError::SpoofedHello);
            }
            h.registry.upsert(node.clone());
            h.ui_send(HubToUi::NodeUpdate(node));
        }
        DaemonToHub::Heartbeat(t) => {
            h.registry.heartbeat(id, t);
            h.node_update(id);
        }
        DaemonToHub::AgentStatus { agent, status } => {
            h.ui_send(HubToUi::AgentStatus {
                agent: agent.clone(),
                node: id.clone(),
                status: status.clone(),
            });
            if matches!(status, AgentStatus::Exited(_) | AgentStatus::Failed) {
                AgentTokens::revoke_agent(&*h.store, &agent)?;
            }
        }
        DaemonToHub::PaneOutput { agent, data } => h.pane_output(agent, data),
        DaemonToHub::Version(info) => {
            if info.proto() < MIN_DAEMON_PROTO {
                return Err(ServerError::DaemonTooOld {
                    node: id.as_str().to_owned(),
                    got: info.proto(),
                    min: MIN_DAEMON_PROTO,
                });
            }
            h.versions
                .lock()
                .expect("versions lock is never held across a panic")
                .entry(id.clone())
                .or_default()
                .version = Some(info.clone());
            h.ui_send(HubToUi::NodeVersion {
                node: id.clone(),
                info,
            });
        }
        DaemonToHub::UpdateStatus(status) => {
            h.versions
                .lock()
                .expect("versions lock is never held across a panic")
                .entry(id.clone())
                .or_default()
                .update = Some(status.clone());
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
    {
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
    loop {
        tokio::select! {
            m = s.recv() => match m {
                Some(Ok(Message::Text(t))) => {
                    let r = decode::<UiToHub>(&t).map_err(ServerError::from).and_then(|m| on_ui(&h, conn, &tx, m));
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
            Some(m) = own.recv() => if !send(&mut s, &m).await { break },
            () = kicked(&mut kick, &id, &sess) => break,
        }
    }
    h.detach_all(conn);
}

fn on_ui(
    h: &Hub,
    conn: u64,
    tx: &mpsc::UnboundedSender<HubToUi>,
    m: UiToHub,
) -> Result<(), ServerError> {
    let now = now_ms();
    match m {
        UiToHub::Spawn { target, spec } => {
            let profile = spec.profile().key()?;
            let nodes = h.registry.snapshot();
            // Fail closed: a store error counts as blocked.
            let blocked =
                |n: &NodeId| !matches!(Blockers::is_blocked(&*h.store, n, &profile), Ok(None));
            let node = match pick_node_where(&nodes, &target, &h.cfg.thresholds, blocked) {
                Ok(n) => n,
                Err(e) => {
                    if let SpawnTarget::Node(n) = &target {
                        if let Some(b) = Blockers::is_blocked(&*h.store, n, &profile)? {
                            return Err(blocked_err(&b));
                        }
                    } else if pick_node(&nodes, &target, &h.cfg.thresholds).is_ok() {
                        return Err(ServerError::Blocked(format!(
                            "every eligible node is blocked for {}; resolve the blockers first",
                            profile.as_str()
                        )));
                    }
                    return Err(e.into());
                }
            };
            let token =
                AgentTokens::issue(&*h.store, spec.agent_id(), h.cfg.agent_token_ttl_ms, now)?;
            let mut env: BTreeMap<String, String> = spec.env().clone();
            env.insert("ULAK_AGENT_TOKEN".into(), token.expose().to_owned());
            env.insert("ULAK_MCP_URL".into(), h.mcp_url().to_owned());
            let spec = SpawnSpec::new(
                spec.agent_id().clone(),
                spec.profile().clone(),
                spec.workdir().clone(),
                spec.prompt().to_owned(),
                env,
            )?;
            let agent = spec.agent_id().clone();
            if let Err(e) = h.to_daemon(&node, HubToDaemon::Spawn(spec)) {
                AgentTokens::revoke_agent(&*h.store, &agent)?;
                return Err(e);
            }
        }
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
        UiToHub::Detach { agent } => {
            if h.detach(conn, &agent) {
                let node = h.node_of_agent(&agent)?;
                h.to_daemon(&node, HubToDaemon::Detach { agent })?;
            }
        }
        UiToHub::PaneInput { agent, data } => {
            let node = h.node_of_agent(&agent)?;
            h.to_daemon(&node, HubToDaemon::PaneInput { agent, data })?;
        }
        UiToHub::Kill { agent } => {
            let node = h.node_of_agent(&agent)?;
            h.to_daemon(
                &node,
                HubToDaemon::Kill {
                    agent: agent.clone(),
                },
            )?;
            AgentTokens::revoke_agent(&*h.store, &agent)?;
        }
        UiToHub::ResolveBlocker { id } => {
            let b = Blockers::resolve(&*h.store, &id, now)?;
            h.ui_send(HubToUi::BlockerClosed { id });
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
        UiToHub::CheckUpdate { node } => update_route(h, node, || HubToDaemon::CheckUpdate)?,
        UiToHub::UpdateNode { node } => update_route(h, node, || HubToDaemon::UpdateNow)?,
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

const RESOLVED_NUDGE: &str = "Login finished. Please continue.";

fn blocked_err(b: &Blocker) -> ServerError {
    ServerError::Blocked(format!(
        "{} is blocked on node {} ({:?}: {}); resolve the blocker first",
        b.profile().as_str(),
        b.node().as_str(),
        b.kind(),
        b.detail()
    ))
}

fn devices(h: &Hub) -> Result<HubToUi, ServerError> {
    let devices = Devices::list(&*h.store)?.iter().map(Into::into).collect();
    Ok(HubToUi::Devices { devices })
}
