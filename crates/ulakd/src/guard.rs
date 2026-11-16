//! Gaming guard: decides whether the node may accept agents.
use crate::errors::ConfigError;
use serde::Deserialize;
use ulak_proto::{BusyReason, NodeState, Telemetry};

/// `/proc/<pid>/comm` holds at most 15 bytes, so names are compared truncated to that.
const COMM_MAX: usize = 15;

fn comm_key(s: &str) -> String {
    let mut end = s.len().min(COMM_MAX);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_ascii_lowercase()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "GuardRaw")]
pub struct GuardConfig {
    blocklist: Vec<String>,
    gpu_threshold: f32,
    cpu_threshold: f32,
    load_per_core: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardRaw {
    blocklist: Option<Vec<String>>,
    gpu_threshold: Option<f32>,
    cpu_threshold: Option<f32>,
    load_per_core: Option<f32>,
}

impl TryFrom<GuardRaw> for GuardConfig {
    type Error = ConfigError;
    fn try_from(r: GuardRaw) -> Result<Self, ConfigError> {
        let d = Self::default();
        Self::new(
            r.blocklist.unwrap_or(d.blocklist),
            r.gpu_threshold.unwrap_or(d.gpu_threshold),
            r.cpu_threshold.unwrap_or(d.cpu_threshold),
            r.load_per_core.unwrap_or(d.load_per_core),
        )
    }
}

impl Default for GuardConfig {
    /// Blueprint defaults: CPU 80, GPU 60, load1 = 1.0 x cores.
    fn default() -> Self {
        Self {
            blocklist: ["steam", "gamescope", "wine64-preloader", "proton"]
                .map(String::from)
                .to_vec(),
            gpu_threshold: 60.0,
            cpu_threshold: 80.0,
            load_per_core: 1.0,
        }
    }
}

impl GuardConfig {
    pub fn new(
        blocklist: Vec<String>,
        gpu_threshold: f32,
        cpu_threshold: f32,
        load_per_core: f32,
    ) -> Result<Self, ConfigError> {
        let pct = |v: f32| (0.0..=100.0).contains(&v);
        if !pct(gpu_threshold) || !pct(cpu_threshold) {
            return Err(ConfigError::BadGuard("thresholds must be 0..=100".into()));
        }
        if !(load_per_core.is_finite() && load_per_core > 0.0) {
            return Err(ConfigError::BadGuard("load_per_core must be > 0".into()));
        }
        if blocklist.iter().any(|b| b.trim().is_empty()) {
            return Err(ConfigError::BadGuard("empty blocklist entry".into()));
        }
        Ok(Self {
            blocklist,
            gpu_threshold,
            cpu_threshold,
            load_per_core,
        })
    }
}

/// Blocklisted process -> `Busy(Gaming)`; else high GPU/CPU/load -> `Busy(Load)`; else `Idle`.
pub fn evaluate(cfg: &GuardConfig, processes: &[String], t: &Telemetry, cores: u32) -> NodeState {
    let blocked: Vec<String> = cfg.blocklist.iter().map(|b| comm_key(b)).collect();
    if let Some(p) = processes.iter().find(|p| blocked.contains(&comm_key(p))) {
        return NodeState::Busy(BusyReason::Gaming { process: p.clone() });
    }
    let gpu_hot = t.gpu_pct().is_some_and(|g| g > cfg.gpu_threshold);
    let load_hot = t.load1() > cfg.load_per_core * cores.max(1) as f32;
    if gpu_hot || t.cpu_pct() > cfg.cpu_threshold || load_hot {
        NodeState::Busy(BusyReason::Load)
    } else {
        NodeState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(load: f32, cpu: f32, gpu: Option<f32>) -> Telemetry {
        Telemetry::new(load, cpu, gpu, 10.0, 0).unwrap()
    }
    fn procs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn idle() {
        let s = evaluate(
            &GuardConfig::default(),
            &procs(&["bash", "sshd"]),
            &t(0.5, 10.0, Some(5.0)),
            8,
        );
        assert_eq!(s, NodeState::Idle);
    }

    #[test]
    fn gaming_process_wins_even_when_quiet() {
        let s = evaluate(
            &GuardConfig::default(),
            &procs(&["bash", "Steam"]),
            &t(0.1, 1.0, None),
            8,
        );
        assert_eq!(
            s,
            NodeState::Busy(BusyReason::Gaming {
                process: "Steam".into()
            })
        );
    }

    #[test]
    fn truncated_comm_matches() {
        // the kernel shows "wine64-preloade" for "wine64-preloader"
        let s = evaluate(
            &GuardConfig::default(),
            &procs(&["wine64-preloade"]),
            &t(0.1, 1.0, None),
            8,
        );
        assert!(matches!(s, NodeState::Busy(BusyReason::Gaming { .. })));
    }

    #[test]
    fn substring_does_not_match() {
        let s = evaluate(
            &GuardConfig::default(),
            &procs(&["steamer", "mysteam"]),
            &t(0.1, 1.0, None),
            8,
        );
        assert_eq!(s, NodeState::Idle);
    }

    #[test]
    fn load_thresholds() {
        let c = GuardConfig::default();
        assert_eq!(
            evaluate(&c, &[], &t(0.0, 0.0, Some(61.0)), 8),
            NodeState::Busy(BusyReason::Load)
        );
        assert_eq!(
            evaluate(&c, &[], &t(0.0, 0.0, Some(60.0)), 8),
            NodeState::Idle
        );
        assert_eq!(
            evaluate(&c, &[], &t(0.0, 81.0, None), 8),
            NodeState::Busy(BusyReason::Load)
        );
        assert_eq!(
            evaluate(&c, &[], &t(8.5, 0.0, None), 8),
            NodeState::Busy(BusyReason::Load)
        );
        assert_eq!(evaluate(&c, &[], &t(7.9, 0.0, None), 8), NodeState::Idle);
    }

    #[test]
    fn custom_config_from_json() {
        let c: GuardConfig =
            serde_json::from_str(r#"{"blocklist":["lutris"],"gpu_threshold":30}"#).unwrap();
        assert_eq!(
            evaluate(&c, &procs(&["steam"]), &t(0.0, 0.0, None), 8),
            NodeState::Idle
        );
        assert!(matches!(
            evaluate(&c, &procs(&["lutris"]), &t(0.0, 0.0, None), 8),
            NodeState::Busy(BusyReason::Gaming { .. })
        ));
        assert!(serde_json::from_str::<GuardConfig>(r#"{"gpu_threshold":300}"#).is_err());
        assert!(serde_json::from_str::<GuardConfig>(r#"{"nope":1}"#).is_err());
    }
}
