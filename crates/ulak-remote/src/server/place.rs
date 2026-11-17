//! Placing a spawn on a node, and the queue of spawns waiting for one.
use super::hub::Hub;
use crate::*;
use std::collections::{BTreeMap, HashSet};
use ulak_proto::*;

const STATUS_CAP: usize = 500;

fn blocked_err(b: &Blocker) -> ServerError {
    ServerError::Blocked(format!(
        "{} is blocked on node {} ({:?}: {}); resolve the blocker first",
        b.profile().as_str(),
        b.node().as_str(),
        b.kind(),
        b.detail()
    ))
}

impl Hub {
    /// Decrypts the stored token of a credential.
    pub(crate) fn git_token(&self, id: &CredentialId) -> Result<GitToken, ServerError> {
        let (nonce, ct) = Credentials::credential_secret(&*self.store, id)?;
        Ok(self.secrets.open(id, &nonce, &ct)?)
    }

    /// Picks a node (skipping `exclude`) and sends it the spawn. A repo credential is decrypted
    /// first, so a bad one fails before any agent token exists. Sends `GitAccess` then `Spawn`
    /// on the same daemon channel; the agent token is revoked if a send fails.
    pub(crate) fn place(
        &self,
        target: &SpawnTarget,
        spec: &SpawnSpec,
        now: u64,
        exclude: &HashSet<NodeId>,
    ) -> Result<NodeId, ServerError> {
        spec.check()?;
        let git = match spec.source() {
            Source::Repo { credential, .. } => Some(self.git_token(&credential)?),
            Source::Folder { .. } => None,
        };
        let profile = spec.profile().key()?;
        let nodes = self.registry.snapshot();
        // Fail closed: a store error counts as blocked.
        let blocked = |n: &NodeId| {
            exclude.contains(n)
                || !matches!(Blockers::is_blocked(&*self.store, n, &profile), Ok(None))
        };
        let node = match pick_node_where(&nodes, target, &self.cfg.thresholds, blocked) {
            Ok(n) => n,
            Err(e) => {
                if let SpawnTarget::Node(n) = target {
                    if let Some(b) = Blockers::is_blocked(&*self.store, n, &profile)? {
                        return Err(blocked_err(&b));
                    }
                } else if pick_node(&nodes, target, &self.cfg.thresholds).is_ok() {
                    return Err(ServerError::Blocked(format!(
                        "every eligible node is blocked for {}; resolve the blockers first",
                        profile.as_str()
                    )));
                }
                return Err(e.into());
            }
        };
        let token = AgentTokens::issue(
            &*self.store,
            spec.agent_id(),
            self.cfg.agent_token_ttl_ms,
            now,
        )?;
        let mut env: BTreeMap<String, String> = spec.env().clone();
        env.insert("ULAK_AGENT_TOKEN".into(), token.expose().to_owned());
        env.insert("ULAK_MCP_URL".into(), self.mcp_url().to_owned());
        let agent = spec.agent_id().clone();
        let sent = (|| {
            if let Some(token) = git {
                self.to_daemon(
                    &node,
                    HubToDaemon::GitAccess {
                        agent: agent.clone(),
                        token,
                    },
                )?;
            }
            self.to_daemon(&node, HubToDaemon::Spawn(spec.clone().with_env(env)))
        })();
        if let Err(e) = sent {
            AgentTokens::revoke_agent(&*self.store, &agent)?;
            return Err(e);
        }
        Ok(node)
    }

    /// Stores a spawn that cannot be placed now, tells the UIs and wakes the scheduler.
    pub(crate) fn enqueue(&self, target: SpawnTarget, spec: SpawnSpec) -> Result<(), ServerError> {
        let known: Vec<AgentId> = self
            .registry
            .snapshot()
            .into_iter()
            .flat_map(|n| n.agents)
            .collect();
        let item = Queue::enqueue(&*self.store, spec, target, &known, super::hub::now_ms())?;
        self.ui_send(HubToUi::QueueChanged { item });
        self.queue_kick.notify_one();
        Ok(())
    }

    /// One scheduler pass: oldest first, at most one item per node.
    pub(crate) fn queue_once(&self, now: u64) {
        let Ok(entries) = Queue::queue_list(&*self.store) else {
            return;
        };
        let mut used = HashSet::new();
        for e in entries {
            if e.item.state != QueueState::Queued {
                continue;
            }
            match self.place(&e.item.target, &e.spec, now, &used) {
                Ok(node) => {
                    used.insert(node);
                    match Queue::mark_placed(&*self.store, &e.item.id, now) {
                        Ok(()) => self.ui_send(HubToUi::QueueRemoved { id: e.item.id }),
                        Err(err) => eprintln!("queue: {err}"),
                    }
                }
                Err(ServerError::Schedule(_) | ServerError::Blocked(_)) => {}
                Err(err) => {
                    if let Ok(item) = Queue::mark_failed(&*self.store, &e.item.id, &err.to_string())
                    {
                        self.ui_send(HubToUi::QueueChanged { item });
                    }
                }
            }
        }
    }

    pub(crate) fn set_status(&self, agent: AgentId, node: NodeId, status: AgentStatus) {
        let mut m = self
            .statuses
            .lock()
            .expect("status map lock is never held across a panic");
        if !matches!(status, AgentStatus::Failed) {
            self.failures
                .lock()
                .expect("failure map lock is never held across a panic")
                .remove(&agent);
        }
        m.insert(agent, (node, status));
        if m.len() > STATUS_CAP {
            m.retain(|_, (_, s)| !matches!(s, AgentStatus::Exited(_) | AgentStatus::Failed));
        }
        if m.len() > STATUS_CAP {
            if let Some(k) = m.keys().next().cloned() {
                m.remove(&k);
            }
        }
    }

    pub(crate) fn set_failure(&self, agent: AgentId, reason: String) {
        let mut f = self
            .failures
            .lock()
            .expect("failure map lock is never held across a panic");
        f.insert(agent, reason);
        if f.len() > STATUS_CAP {
            let s = self
                .statuses
                .lock()
                .expect("status map lock is never held across a panic");
            f.retain(|a, _| s.contains_key(a));
        }
        if f.len() > STATUS_CAP {
            if let Some(k) = f.keys().next().cloned() {
                f.remove(&k);
            }
        }
    }

    /// `AgentFailed` frames for agents whose status is `Failed`.
    pub(crate) fn failed_frames(&self) -> Vec<HubToUi> {
        let s = self.statuses();
        let f = self
            .failures
            .lock()
            .expect("failure map lock is never held across a panic");
        s.into_iter()
            .filter(|(_, _, st)| matches!(st, AgentStatus::Failed))
            .filter_map(|(agent, node, _)| {
                let reason = f.get(&agent)?.clone();
                Some(HubToUi::AgentFailed {
                    agent,
                    node,
                    reason,
                })
            })
            .collect()
    }

    pub(crate) fn statuses(&self) -> Vec<(AgentId, NodeId, AgentStatus)> {
        let m = self
            .statuses
            .lock()
            .expect("status map lock is never held across a panic");
        m.iter()
            .map(|(a, (n, s))| (a.clone(), n.clone(), s.clone()))
            .collect()
    }
}
