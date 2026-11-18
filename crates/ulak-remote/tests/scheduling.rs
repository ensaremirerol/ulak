use ulak_proto::*;
use ulak_remote::{
    pick_node, pick_node_ranked, pick_node_where, repo_rank, Registry, ScheduleError, Thresholds,
};

fn node(id: &str, state: NodeState, cpu: f32, gpu: Option<f32>, at: u64) -> NodeInfo {
    node_load(id, state, 0.1, cpu, gpu, at)
}
fn node_load(
    id: &str,
    state: NodeState,
    load: f32,
    cpu: f32,
    gpu: Option<f32>,
    at: u64,
) -> NodeInfo {
    let t = Telemetry::new(load, cpu, gpu, 10.0, at).unwrap();
    NodeInfo::new(NodeId::new(id).unwrap(), id.into(), state, t, vec![])
}
fn limits() -> Thresholds {
    Thresholds::new(80.0, 60.0)
}
fn nid(s: &str) -> NodeId {
    NodeId::new(s).unwrap()
}

#[test]
fn auto_picks_lowest_cpu_eligible() {
    let nodes = [
        node(
            "busy",
            NodeState::Busy(BusyReason::Gaming {
                process: "steam".into(),
            }),
            1.0,
            None,
            0,
        ),
        node("hot", NodeState::Idle, 90.0, None, 0),
        node("gpu-hot", NodeState::Idle, 5.0, Some(95.0), 0),
        node("b", NodeState::Idle, 30.0, None, 0),
        node("a", NodeState::Idle, 10.0, Some(20.0), 0),
        node("off", NodeState::Offline, 0.0, None, 0),
    ];
    assert_eq!(
        pick_node(&nodes, &SpawnTarget::Auto, &limits()).unwrap(),
        nid("a")
    );
}

#[test]
fn auto_with_nothing_eligible() {
    let nodes = [node("hot", NodeState::Idle, 99.0, None, 0)];
    assert_eq!(
        pick_node(&nodes, &SpawnTarget::Auto, &limits()),
        Err(ScheduleError::NoEligibleNode)
    );
    assert_eq!(
        pick_node(&[], &SpawnTarget::Auto, &limits()),
        Err(ScheduleError::NoEligibleNode)
    );
}

#[test]
fn explicit_target_never_falls_back() {
    let nodes = [
        node(
            "game",
            NodeState::Busy(BusyReason::Gaming {
                process: "steam".into(),
            }),
            1.0,
            None,
            0,
        ),
        node("free", NodeState::Idle, 1.0, None, 0),
    ];
    assert_eq!(
        pick_node(&nodes, &SpawnTarget::Node(nid("free")), &limits()).unwrap(),
        nid("free")
    );
    assert_eq!(
        pick_node(&nodes, &SpawnTarget::Node(nid("game")), &limits()),
        Err(ScheduleError::NodeNotEligible(nid("game")))
    );
    assert_eq!(
        pick_node(&nodes, &SpawnTarget::Node(nid("ghost")), &limits()),
        Err(ScheduleError::UnknownNode(nid("ghost")))
    );
}

#[test]
fn registry_tracks_heartbeats_and_expires_stale_nodes() {
    let r = Registry::default();
    r.upsert(node("a", NodeState::Idle, 10.0, None, 1_000));
    r.upsert(node("b", NodeState::Idle, 10.0, None, 1_000));
    r.heartbeat(
        &nid("a"),
        Telemetry::new(0.1, 55.0, None, 10.0, 40_000).unwrap(),
    );

    r.expire_stale(45_000, 30_000); // b last seen 44 s ago, a 5 s ago
    let snap = r.snapshot();
    let get = |id: &str| snap.iter().find(|n| n.id == nid(id)).unwrap().clone();
    assert_eq!(get("a").state, NodeState::Idle);
    assert_eq!(get("a").telemetry.cpu_pct(), 55.0);
    assert_eq!(get("b").state, NodeState::Offline);

    r.set_offline(&nid("a"));
    assert_eq!(
        r.snapshot()
            .iter()
            .filter(|n| n.state == NodeState::Offline)
            .count(),
        2
    );
}

#[test]
fn heartbeat_from_unknown_node_is_ignored() {
    let r = Registry::default();
    r.heartbeat(
        &nid("ghost"),
        Telemetry::new(0.1, 1.0, None, 1.0, 1).unwrap(),
    );
    assert!(r.snapshot().is_empty());
}

#[test]
fn high_load_average_does_not_block_an_idle_node_by_default() {
    // ulakd already judged load per core and said Idle; the hub must trust it.
    let nodes = [node_load("n", NodeState::Idle, 9.0, 95.0, Some(99.0), 0)];
    let d = Thresholds::default();
    assert_eq!(pick_node(&nodes, &SpawnTarget::Auto, &d).unwrap(), nid("n"));
    assert_eq!(
        pick_node(&nodes, &SpawnTarget::Node(nid("n")), &d).unwrap(),
        nid("n")
    );
}

#[test]
fn auto_skips_blocked_nodes() {
    let nodes = [
        node("a", NodeState::Idle, 10.0, None, 0),
        node("b", NodeState::Idle, 30.0, None, 0),
    ];
    let blocked = |n: &NodeId| n.as_str() == "a";
    assert_eq!(
        pick_node_where(&nodes, &SpawnTarget::Auto, &limits(), blocked).unwrap(),
        nid("b")
    );
    assert_eq!(
        pick_node_where(&nodes, &SpawnTarget::Auto, &limits(), |_| true),
        Err(ScheduleError::NoEligibleNode)
    );
    // An explicit target is not rerouted.
    assert_eq!(
        pick_node_where(&nodes, &SpawnTarget::Node(nid("a")), &limits(), blocked),
        Err(ScheduleError::NodeNotEligible(nid("a")))
    );
}

fn entry(name: &str, path: &str, clean: bool, in_use: bool, managed: bool) -> RepoEntry {
    RepoEntry {
        full_name: RepoRef::new(name).unwrap(),
        path: path.into(),
        branch: Some("main".into()),
        clean,
        ahead: Some(0),
        behind: Some(0),
        last_fetch_ms: None,
        in_use,
        managed,
    }
}

#[test]
fn repo_rank_prefers_an_unused_clean_managed_clone() {
    let r = RepoRef::new("o/n").unwrap();
    assert_eq!(repo_rank(&[], &r), 2);
    assert_eq!(
        repo_rank(&[entry("o/other", "/a", true, false, true)], &r),
        2
    );
    assert_eq!(repo_rank(&[entry("o/n", "/a", true, false, true)], &r), 0);
    for (clean, in_use, managed) in [
        (false, false, true),
        (true, true, true),
        (true, false, false),
    ] {
        assert_eq!(
            repo_rank(&[entry("o/n", "/a", clean, in_use, managed)], &r),
            1
        );
    }
    let two = [
        entry("o/n", "/a", true, true, true),
        entry("o/n", "/b", true, false, true),
    ];
    assert_eq!(repo_rank(&two, &r), 0);
}

#[test]
fn auto_prefers_lower_rank_then_lower_cpu_and_explicit_ignores_rank() {
    let nodes = [
        node("a", NodeState::Idle, 10.0, None, 0),
        node("b", NodeState::Idle, 50.0, None, 0),
        node("c", NodeState::Idle, 30.0, None, 0),
        node(
            "busy",
            NodeState::Busy(BusyReason::Gaming {
                process: "g".into(),
            }),
            1.0,
            None,
            0,
        ),
    ];
    let rank = |n: &NodeId| match n.as_str() {
        "b" | "c" | "busy" => 0,
        _ => 2,
    };
    let pick = |t: &SpawnTarget| pick_node_ranked(&nodes, t, &limits(), |_| false, rank);
    assert_eq!(pick(&SpawnTarget::Auto).unwrap(), nid("c"));
    assert_eq!(pick(&SpawnTarget::Node(nid("a"))).unwrap(), nid("a"));
    // Same rank everywhere: the existing order (lowest CPU).
    assert_eq!(
        pick_node_ranked(&nodes, &SpawnTarget::Auto, &limits(), |_| false, |_| 1).unwrap(),
        nid("a")
    );
    // A preferred node that is blocked is skipped.
    let blocked = |n: &NodeId| n.as_str() == "c";
    assert_eq!(
        pick_node_ranked(&nodes, &SpawnTarget::Auto, &limits(), blocked, rank).unwrap(),
        nid("b")
    );
}
