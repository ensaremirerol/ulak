use crate::ScheduleError;
use ulak_proto::{NodeId, NodeInfo, RepoEntry, RepoRef, SpawnTarget};

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    max_cpu_pct: f32,
    max_gpu_pct: f32,
}

impl Default for Thresholds {
    /// 100 %: never interferes; the node's reported state decides.
    fn default() -> Self {
        Self::new(100.0, 100.0)
    }
}
impl Thresholds {
    /// Opt-in hub-side caps on top of the node's own `NodeState` (ulakd already judges load per
    /// core). Load average is deliberately not checked here: an absolute number is wrong on
    /// multi-core hosts.
    pub fn new(max_cpu_pct: f32, max_gpu_pct: f32) -> Self {
        Self {
            max_cpu_pct,
            max_gpu_pct,
        }
    }
    fn allows(&self, n: &NodeInfo) -> bool {
        let t = &n.telemetry;
        n.state.accepts_spawn()
            && t.cpu_pct() <= self.max_cpu_pct
            && t.gpu_pct().is_none_or(|g| g <= self.max_gpu_pct)
    }
}

/// `Auto` picks the eligible node with the lowest CPU use. An explicit node is
/// used only if eligible; there is no fallback to another node.
pub fn pick_node(
    nodes: &[NodeInfo],
    target: &SpawnTarget,
    limits: &Thresholds,
) -> Result<NodeId, ScheduleError> {
    pick_node_where(nodes, target, limits, |_| false)
}

/// Like `pick_node`, but `Auto` skips nodes for which `blocked` is true; an explicit node that
/// is blocked is `NodeNotEligible`.
pub fn pick_node_where(
    nodes: &[NodeInfo],
    target: &SpawnTarget,
    limits: &Thresholds,
    blocked: impl Fn(&NodeId) -> bool,
) -> Result<NodeId, ScheduleError> {
    pick_node_ranked(nodes, target, limits, blocked, |_| 0)
}

/// Like `pick_node_where`, but `Auto` prefers the lowest `rank`, then the lowest CPU use.
/// Explicit targets ignore `rank`.
pub fn pick_node_ranked(
    nodes: &[NodeInfo],
    target: &SpawnTarget,
    limits: &Thresholds,
    blocked: impl Fn(&NodeId) -> bool,
    rank: impl Fn(&NodeId) -> u8,
) -> Result<NodeId, ScheduleError> {
    match target {
        SpawnTarget::Auto => nodes
            .iter()
            .filter(|n| limits.allows(n) && !blocked(&n.id))
            .min_by(|a, b| {
                rank(&a.id)
                    .cmp(&rank(&b.id))
                    .then(a.telemetry.cpu_pct().total_cmp(&b.telemetry.cpu_pct()))
            })
            .map(|n| n.id.clone())
            .ok_or(ScheduleError::NoEligibleNode),
        SpawnTarget::Node(id) => match nodes.iter().find(|n| &n.id == id) {
            None => Err(ScheduleError::UnknownNode(id.clone())),
            Some(n) if limits.allows(n) && !blocked(&n.id) => Ok(id.clone()),
            Some(_) => Err(ScheduleError::NodeNotEligible(id.clone())),
        },
    }
}

/// How well a node's repo index suits a spawn of `repo`: 0 an unused, clean managed clone (the
/// daemon reuses it), 1 any clone of it, 2 none.
pub fn repo_rank(repos: &[RepoEntry], repo: &RepoRef) -> u8 {
    let mut of = repos.iter().filter(|r| &r.full_name == repo).peekable();
    if of.peek().is_none() {
        2
    } else if of.any(|r| r.managed && r.clean && !r.in_use) {
        0
    } else {
        1
    }
}
