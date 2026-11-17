use std::collections::BTreeMap;
use ulak_proto::*;

fn agent() -> AgentId {
    AgentId::new("a-1").unwrap()
}
fn node() -> NodeId {
    NodeId::new("n-1").unwrap()
}

fn spec() -> SpawnSpec {
    let profile = AgentProfile::new("claude", vec![], Some("/home/u/.claude-b".into())).unwrap();
    SpawnSpec::new(
        agent(),
        profile,
        "/srv/repo".into(),
        "fix the bug".into(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn roundtrip<T>(v: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let s = encode(&v).unwrap();
    assert_eq!(decode::<T>(&s).unwrap(), v, "{s}");
}

#[test]
fn every_direction_roundtrips() {
    let tel = Telemetry::new(0.2, 5.0, None, 30.0, 1).unwrap();
    let info = NodeInfo::new(
        node(),
        "box".into(),
        NodeState::Idle,
        tel.clone(),
        vec![agent()],
    );
    let q = Question::new(
        QuestionId::new("q-1").unwrap(),
        agent(),
        "Approve?".into(),
        2,
    )
    .unwrap();
    let msg = Message::new(
        MessageId::new("m-1").unwrap(),
        agent(),
        AgentId::new("b-1").unwrap(),
        ThreadId::new("t-1").unwrap(),
        "hi".into(),
        3,
    )
    .unwrap();

    roundtrip(DaemonToHub::Hello { node: info.clone() });
    roundtrip(DaemonToHub::Heartbeat(tel));
    roundtrip(DaemonToHub::AgentStatus {
        agent: agent(),
        status: AgentStatus::Exited(Some(1)),
    });
    roundtrip(DaemonToHub::PaneOutput {
        agent: agent(),
        data: vec![0, 255, 27, b'[', b'H'],
    });
    roundtrip(DaemonToHub::AgentFailed {
        agent: agent(),
        reason: "clone failed".into(),
    });
    roundtrip(HubToUi::AgentFailed {
        agent: agent(),
        node: NodeId::new("n1").unwrap(),
        reason: "clone failed".into(),
    });
    roundtrip(HubToDaemon::Spawn(spec()));
    roundtrip(HubToDaemon::Nudge {
        agent: agent(),
        text: "check q-1".into(),
    });
    roundtrip(HubToDaemon::PaneInput {
        agent: agent(),
        data: b"ls\r".to_vec(),
    });
    roundtrip(UiToHub::Spawn {
        target: SpawnTarget::Auto,
        spec: spec(),
    });
    roundtrip(UiToHub::Spawn {
        target: SpawnTarget::Node(node()),
        spec: spec(),
    });
    roundtrip(UiToHub::Answer(
        Answer::new(q.id().clone(), "yes".into()).unwrap(),
    ));
    roundtrip(HubToUi::Snapshot {
        nodes: vec![info],
        questions: vec![q.clone()],
    });
    roundtrip(HubToUi::QuestionOpened(q));
    roundtrip(HubToUi::MessageLogged(msg));
    roundtrip(HubToUi::Error {
        message: "nope".into(),
    });
}

#[test]
fn frame_is_tagged_json_with_base64_pane_bytes() {
    let s = encode(&DaemonToHub::PaneOutput {
        agent: agent(),
        data: b"hi".to_vec(),
    })
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "pane_output");
    assert_eq!(v["data"], "aGk=");
}

#[test]
fn decode_rejects_unknown_type_and_garbage() {
    assert!(matches!(
        decode::<UiToHub>(r#"{"type":"nuke"}"#),
        Err(ProtoError::Codec(_))
    ));
    assert!(matches!(
        decode::<UiToHub>("not json"),
        Err(ProtoError::Codec(_))
    ));
}

#[test]
fn decode_revalidates_ids() {
    let bad = r#"{"type":"attach","agent":"BAD ID"}"#;
    assert!(decode::<UiToHub>(bad).is_err());
}

#[test]
fn spawn_spec_and_profile_validation() {
    assert!(AgentProfile::new("", vec![], None).is_err());
    let p = AgentProfile::new("claude", vec![], None).unwrap();
    assert!(SpawnSpec::new(agent(), p, "/x".into(), "".into(), BTreeMap::new()).is_err());
    // empty prompt
}

#[test]
fn enrollment_types() {
    let otp = Otp::new("K7Q2M9XA").unwrap();
    assert!(EnrollRequest::new(otp.clone(), "".into(), "box".into()).is_err()); // empty public key
    let req = EnrollRequest::new(otp, "ssh-ed25519-pubkey-bytes".into(), "box".into()).unwrap();
    roundtrip(req);
    roundtrip(EnrollResponse::new("CERT".into(), "CA".into()));
}

#[test]
fn agent_token_hidden_from_debug() {
    let t = AgentToken::new("s3cret-token-value".into()).unwrap();
    assert!(!format!("{t:?}").contains("s3cret"));
}

#[test]
fn ui_milestone_variants_roundtrip() {
    let dev = DeviceInfo {
        id: node(),
        kind: DeviceKind::Browser,
        hostname: "phone".into(),
        created_ms: 7,
        revoked: true,
    };
    let msg = Message::new(
        MessageId::new("m-1").unwrap(),
        agent(),
        AgentId::new("b-1").unwrap(),
        ThreadId::new("t-1").unwrap(),
        "hi".into(),
        3,
    )
    .unwrap();
    roundtrip(UiToHub::Kill { agent: agent() });
    for kind in [DeviceKind::Node, DeviceKind::Ui, DeviceKind::Browser] {
        roundtrip(UiToHub::AddDevice { kind });
    }
    roundtrip(UiToHub::RevokeDevice { id: node() });
    roundtrip(UiToHub::ListDevices);
    roundtrip(UiToHub::MailboxLog {
        thread: Some(ThreadId::new("t-1").unwrap()),
        limit: 50,
    });
    roundtrip(UiToHub::MailboxLog {
        thread: None,
        limit: 5,
    });
    roundtrip(HubToUi::AgentStatus {
        agent: agent(),
        node: node(),
        status: AgentStatus::Running,
    });
    roundtrip(HubToUi::OtpOffer {
        kind: DeviceKind::Ui,
        otp: Otp::new("K7Q2M9XA").unwrap(),
        hub_fingerprint: "ab:cd".into(),
        expires_ms: 9,
    });
    roundtrip(HubToUi::Devices { devices: vec![dev] });
    roundtrip(HubToUi::MailboxLog {
        messages: vec![msg],
    });
    let s = encode(&UiToHub::AddDevice {
        kind: DeviceKind::Browser,
    })
    .unwrap();
    assert!(s.contains(r#""kind":"browser""#), "{s}");
}
