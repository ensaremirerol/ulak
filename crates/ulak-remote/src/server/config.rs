//! Hub profiles, node settings (stored, pushed to daemons) and per-node telemetry history.
use super::hub::Hub;
use crate::*;
use std::collections::{BTreeMap, VecDeque};
use ulak_proto::*;

pub(crate) const MAX_HUB_PROFILES: usize = 64;
const STATS_CAP: usize = 720;
const STATS_WINDOW_MS: u64 = 3_600_000;

fn bad(m: impl Into<String>) -> ServerError {
    ServerError::BadArgs(m.into())
}

/// `min..=max` chars, none of them control characters.
fn text_ok(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.chars().count()) && !s.chars().any(char::is_control)
}

pub(crate) fn check_name(name: &str) -> Result<(), ServerError> {
    let ok = (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(bad(
            "profile name must be 1-64 characters of A-Z a-z 0-9 . _ -",
        ))
    }
}

pub(crate) fn check_profile(p: &HubProfile) -> Result<(), ServerError> {
    check_name(&p.name)?;
    if !text_ok(&p.binary, 1, 256) {
        return Err(bad("binary must be 1-256 printable characters"));
    }
    if p.args.len() > 32 || !p.args.iter().all(|a| text_ok(a, 1, 512)) {
        return Err(bad("at most 32 args, each 1-512 printable characters"));
    }
    if p.config_dir.as_deref().is_some_and(|d| !text_ok(d, 0, 512)) {
        return Err(bad("config_dir must be at most 512 printable characters"));
    }
    if p.env.len() > 32 {
        return Err(bad("at most 32 env entries"));
    }
    for (k, v) in &p.env {
        let mut b = k.bytes();
        let key_ok = k.len() <= 64
            && b.next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            && b.all(|c| c.is_ascii_alphanumeric() || c == b'_');
        if !key_ok {
            return Err(bad(format!(
                "env key {k:?} must match [A-Za-z_][A-Za-z0-9_]{{0,63}}"
            )));
        }
        if k.to_ascii_uppercase().starts_with("ULAK") {
            return Err(bad(format!(
                "env key {k:?}: ULAK* variables are reserved for ulak"
            )));
        }
        if !text_ok(v, 0, 4096) {
            return Err(bad(format!(
                "env {k}: value must be at most 4096 printable characters"
            )));
        }
    }
    Ok(())
}

pub(crate) fn check_settings(s: &NodeSettings) -> Result<(), ServerError> {
    if s.repo_roots
        .as_ref()
        .is_some_and(|r| r.len() > 16 || !r.iter().all(|x| text_ok(x, 1, 512)))
    {
        return Err(bad(
            "at most 16 repo roots, each 1-512 printable characters",
        ));
    }
    if s.index_interval_mins
        .is_some_and(|m| !(1..=1440).contains(&m))
    {
        return Err(bad("index interval must be 1-1440 minutes"));
    }
    if s.clean_after_exit_hours
        .is_some_and(|h| !(1..=8760).contains(&h))
    {
        return Err(bad("clean-after-exit must be 1-8760 hours"));
    }
    let pct_ok = |p: Option<f32>| p.is_none_or(|p| p.is_finite() && (1.0..=100.0).contains(&p));
    if !pct_ok(s.cpu_busy_pct) || !pct_ok(s.gpu_busy_pct) {
        return Err(bad("busy thresholds must be 1-100 %"));
    }
    Ok(())
}

/// The profiles `node` gets: globals, overridden by its own of the same name; sorted by name.
fn merged(profiles: &[ScopedProfile], node: &NodeId) -> Vec<HubProfile> {
    let global = profiles.iter().filter(|p| p.node.is_none());
    let own = profiles.iter().filter(|p| p.node.as_ref() == Some(node));
    let m: BTreeMap<_, _> = global
        .chain(own)
        .map(|p| (p.profile.name.clone(), p.profile.clone()))
        .collect();
    m.into_values().collect()
}

type ConfigState = (Vec<ScopedProfile>, Vec<NodeSettingsEntry>);

impl Hub {
    fn config_state(&self) -> Result<ConfigState, ServerError> {
        Ok((
            HubProfiles::list_hub_profiles(&*self.store)?,
            HubProfiles::list_node_settings(&*self.store)?,
        ))
    }

    pub(crate) fn hub_config(&self) -> Result<HubToUi, ServerError> {
        let (profiles, settings) = self.config_state()?;
        Ok(HubToUi::HubConfig { profiles, settings })
    }

    /// Sends `Config` to `only` (or every node) among connected daemons that sent `Version`.
    /// Callers hold `config_lock`, so the frames leave in the order the store changed.
    fn send_configs(&self, (profiles, settings): &ConfigState, only: Option<&NodeId>) {
        let nodes: Vec<NodeId> = self
            .versions
            .lock()
            .expect("versions lock is never held across a panic")
            .iter()
            .filter(|(n, m)| m.version.is_some() && only.is_none_or(|o| o == *n))
            .map(|(n, _)| n.clone())
            .collect();
        for n in nodes {
            let settings = settings
                .iter()
                .find(|e| e.node == n)
                .map(|e| e.settings.clone())
                .unwrap_or_default();
            let profiles = merged(profiles, &n);
            // An offline node gets its config after its next `Version`.
            let _ = self.to_daemon(&n, HubToDaemon::Config { profiles, settings });
        }
    }

    /// A daemon just sent `Version`.
    pub(crate) fn push_config(&self, node: &NodeId) -> Result<(), ServerError> {
        let _g = self
            .config_lock
            .lock()
            .expect("config lock is never held across a panic");
        self.send_configs(&self.config_state()?, Some(node));
        Ok(())
    }

    /// Applies a store change, then broadcasts `HubConfig` and pushes `Config` to `only` (or
    /// every connected node).
    pub(crate) fn config_change(
        &self,
        only: Option<&NodeId>,
        f: impl FnOnce(&Store) -> Result<(), HubProfileError>,
    ) -> Result<(), ServerError> {
        let _g = self
            .config_lock
            .lock()
            .expect("config lock is never held across a panic");
        f(&self.store)?;
        let state = self.config_state()?;
        self.send_configs(&state, only);
        let (profiles, settings) = state;
        self.ui_send(HubToUi::HubConfig { profiles, settings });
        Ok(())
    }

    /// A node-scoped entry needs an enrolled, unrevoked device.
    pub(crate) fn known_node(&self, n: &NodeId) -> Result<(), ServerError> {
        if Devices::is_active(&*self.store, n)? {
            Ok(())
        } else {
            Err(bad(format!("unknown node {}", n.as_str())))
        }
    }

    fn stats_map(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<NodeId, StatsRing>> {
        self.stats
            .lock()
            .expect("stats lock is never held across a panic")
    }

    pub(crate) fn record_telemetry(&self, node: &NodeId, t: Telemetry, now: u64) {
        self.stats_map()
            .entry(node.clone())
            .or_default()
            .push(now, t);
    }

    /// When the node's current connection started; `None` on disconnect.
    pub(crate) fn set_connected(&self, node: &NodeId, at: Option<u64>) {
        self.stats_map()
            .entry(node.clone())
            .or_default()
            .connected_ms = at;
    }

    pub(crate) fn node_stats(&self, node: NodeId, now: u64) -> HubToUi {
        let mut m = self.stats_map();
        let (connected_ms, samples) = m
            .get_mut(&node)
            .map_or((None, vec![]), |r| (r.connected_ms, r.samples(now)));
        HubToUi::NodeStats {
            node,
            connected_ms,
            samples,
        }
    }
}

/// Telemetry history of one node: at most `STATS_CAP` samples received in the last hour.
#[derive(Default)]
pub(crate) struct StatsRing {
    pub connected_ms: Option<u64>,
    /// (received at, sample), oldest first.
    samples: VecDeque<(u64, Telemetry)>,
}

impl StatsRing {
    pub fn push(&mut self, now: u64, t: Telemetry) {
        // A Hello after a heartbeat repeats the same sample.
        if self
            .samples
            .back()
            .is_some_and(|(_, l)| l.at_ms() == t.at_ms())
        {
            return;
        }
        self.samples.push_back((now, t));
        self.trim(now);
    }

    fn trim(&mut self, now: u64) {
        while self.samples.len() > STATS_CAP
            || self
                .samples
                .front()
                .is_some_and(|(at, _)| now.saturating_sub(*at) > STATS_WINDOW_MS)
        {
            self.samples.pop_front();
        }
    }

    pub fn samples(&mut self, now: u64) -> Vec<Telemetry> {
        self.trim(now);
        self.samples.iter().map(|(_, t)| t.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(at: u64) -> Telemetry {
        Telemetry::new(0.1, 5.0, None, 30.0, at).unwrap()
    }

    #[test]
    fn ring_is_capped_by_count_and_time() {
        let mut r = StatsRing::default();
        for i in 0..800 {
            r.push(i * 1000, t(i));
        }
        let s = r.samples(799_000);
        assert_eq!(s.len(), STATS_CAP);
        assert_eq!(s[0].at_ms(), 80);
        assert_eq!(s.last().unwrap().at_ms(), 799);
        // An hour later only samples received in the last hour remain.
        let s = r.samples(799_000 + STATS_WINDOW_MS - 500_000);
        assert_eq!(s.first().unwrap().at_ms(), 299);
        assert!(r.samples(799_000 + STATS_WINDOW_MS + 1).is_empty());
        // A repeated sample (Hello after Heartbeat) is not stored twice.
        r.push(5_000_000, t(1));
        r.push(5_000_001, t(1));
        assert_eq!(r.samples(5_000_001).len(), 1);
    }

    #[test]
    fn node_scoped_profiles_beat_globals_and_come_sorted() {
        let p = |name: &str, bin: &str| HubProfile {
            name: name.into(),
            binary: bin.into(),
            args: vec![],
            config_dir: None,
            env: BTreeMap::new(),
        };
        let a = NodeId::new("a").unwrap();
        let b = NodeId::new("b").unwrap();
        let all = vec![
            ScopedProfile {
                node: Some(a.clone()),
                profile: p("x", "own"),
            },
            ScopedProfile {
                node: None,
                profile: p("z", "g"),
            },
            ScopedProfile {
                node: None,
                profile: p("x", "global"),
            },
            ScopedProfile {
                node: Some(b.clone()),
                profile: p("y", "b"),
            },
        ];
        assert_eq!(merged(&all, &a), vec![p("x", "own"), p("z", "g")]);
        assert_eq!(
            merged(&all, &b),
            vec![p("x", "global"), p("y", "b"), p("z", "g")]
        );
    }

    #[test]
    fn validation_limits() {
        let mut p = HubProfile {
            name: "claude-work".into(),
            binary: "claude".into(),
            args: vec![],
            config_dir: Some("~/.claude-work".into()),
            env: BTreeMap::from([("FOO_1".into(), "bar".into())]),
        };
        assert!(check_profile(&p).is_ok());
        for bad in ["", "a b", "x/y", &"a".repeat(65)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        p.env = BTreeMap::from([("ulak_x".into(), "v".into())]);
        assert!(check_profile(&p).is_err());
        p.env = BTreeMap::from([("1A".into(), "v".into())]);
        assert!(check_profile(&p).is_err());
        p.env = BTreeMap::from([("A".into(), "a\nb".into())]);
        assert!(check_profile(&p).is_err());
        p.env.clear();
        p.args = vec!["-v".into(); 33];
        assert!(check_profile(&p).is_err());
        p.args = vec![];
        p.binary = "cl\u{7}aude".into();
        assert!(check_profile(&p).is_err());
        let ok = NodeSettings {
            repo_roots: Some(vec!["~/src".into()]),
            index_interval_mins: Some(1440),
            cpu_busy_pct: Some(100.0),
            ..NodeSettings::default()
        };
        assert!(check_settings(&ok).is_ok());
        for s in [
            NodeSettings {
                index_interval_mins: Some(0),
                ..ok.clone()
            },
            NodeSettings {
                clean_after_exit_hours: Some(8761),
                ..ok.clone()
            },
            NodeSettings {
                gpu_busy_pct: Some(f32::NAN),
                ..ok.clone()
            },
            NodeSettings {
                repo_roots: Some(vec![String::new()]),
                ..ok.clone()
            },
            NodeSettings {
                repo_roots: Some(vec!["x".into(); 17]),
                ..ok.clone()
            },
        ] {
            assert!(check_settings(&s).is_err(), "{s:?}");
        }
    }
}
