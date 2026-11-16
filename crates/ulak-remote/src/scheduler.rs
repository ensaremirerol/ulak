use crate::ScheduleError;
use ulak_proto::{NodeId, NodeInfo, SpawnTarget};

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
    match target {
        SpawnTarget::Auto => nodes
            .iter()
            .filter(|n| limits.allows(n))
            .min_by(|a, b| a.telemetry.cpu_pct().total_cmp(&b.telemetry.cpu_pct()))
            .map(|n| n.id.clone())
            .ok_or(ScheduleError::NoEligibleNode),
        SpawnTarget::Node(id) => match nodes.iter().find(|n| &n.id == id) {
            None => Err(ScheduleError::UnknownNode(id.clone())),
            Some(n) if limits.allows(n) => Ok(id.clone()),
            Some(_) => Err(ScheduleError::NodeNotEligible(id.clone())),
        },
    }
}
