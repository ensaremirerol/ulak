use std::{collections::BTreeMap, sync::Mutex};
use ulak_proto::{NodeId, NodeInfo, NodeState, Telemetry};

/// Live node table, rebuilt from daemon messages. Not persisted.
#[derive(Default)]
pub struct Registry(Mutex<BTreeMap<NodeId, NodeInfo>>);

impl Registry {
    fn nodes(&self) -> std::sync::MutexGuard<'_, BTreeMap<NodeId, NodeInfo>> {
        self.0
            .lock()
            .expect("registry lock is never held across a panic")
    }
    /// Hello and state changes (e.g. gaming started) arrive as a full `NodeInfo`.
    pub fn upsert(&self, n: NodeInfo) {
        self.nodes().insert(n.id.clone(), n);
    }
    /// Unknown nodes are ignored. A heartbeat revives an Offline node as Idle.
    pub fn heartbeat(&self, id: &NodeId, t: Telemetry) {
        if let Some(n) = self.nodes().get_mut(id) {
            n.telemetry = t;
            if n.state == NodeState::Offline {
                n.state = NodeState::Idle;
            }
        }
    }
    /// Drops a node (device revoked); returns it so the caller can announce it.
    pub fn remove(&self, id: &NodeId) -> Option<NodeInfo> {
        self.nodes().remove(id)
    }
    pub fn set_offline(&self, id: &NodeId) {
        if let Some(n) = self.nodes().get_mut(id) {
            n.state = NodeState::Offline;
        }
    }
    /// Mark nodes whose last heartbeat is older than `ttl_ms` as Offline.
    pub fn expire_stale(&self, now_ms: u64, ttl_ms: u64) {
        for n in self.nodes().values_mut() {
            if now_ms.saturating_sub(n.telemetry.at_ms()) > ttl_ms {
                n.state = NodeState::Offline;
            }
        }
    }
    pub fn snapshot(&self) -> Vec<NodeInfo> {
        self.nodes().values().cloned().collect()
    }
}
