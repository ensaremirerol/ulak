use super::hub::{now_ms, Hub, Peer};
use crate::*;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
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
    if !send(&mut s, &snap).await {
        return;
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
            let node = pick_node(&h.registry.snapshot(), &target, &h.cfg.thresholds)?;
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

fn devices(h: &Hub) -> Result<HubToUi, ServerError> {
    let devices = Devices::list(&*h.store)?.iter().map(Into::into).collect();
    Ok(HubToUi::Devices { devices })
}
