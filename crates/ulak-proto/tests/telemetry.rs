use ulak_proto::{BusyReason, NodeState, ProtoError, Telemetry};

#[test]
fn valid_telemetry() {
    assert!(Telemetry::new(0.5, 12.0, Some(80.0), 40.0, 1_700_000_000_000).is_ok());
    assert!(Telemetry::new(0.0, 0.0, None, 100.0, 0).is_ok());
}

#[test]
fn rejects_out_of_range_and_nan() {
    for (cpu, gpu, mem) in [
        (-1.0, None, 0.0),
        (101.0, None, 0.0),
        (f32::NAN, None, 0.0),
        (0.0, Some(100.1), 0.0),
        (0.0, None, f32::INFINITY),
    ] {
        assert!(
            matches!(
                Telemetry::new(0.1, cpu, gpu, mem, 0),
                Err(ProtoError::OutOfRange { .. })
            ),
            "{cpu} {gpu:?} {mem}"
        );
    }
    assert!(Telemetry::new(-0.1, 0.0, None, 0.0, 0).is_err()); // negative load
}

#[test]
fn only_idle_accepts_spawns() {
    assert!(NodeState::Idle.accepts_spawn());
    assert!(!NodeState::Offline.accepts_spawn());
    assert!(!NodeState::Busy(BusyReason::Gaming {
        process: "steam".into()
    })
    .accepts_spawn());
    assert!(!NodeState::Busy(BusyReason::Load).accepts_spawn());
}
