use ulak_proto::*;

fn roundtrip<T>(v: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let s = encode(&v).unwrap();
    assert_eq!(decode::<T>(&s).unwrap(), v, "{s}");
}

fn info() -> VersionInfo {
    VersionInfo::current("0.1.0", "abc1234").unwrap()
}

fn node() -> NodeId {
    NodeId::new("n-1").unwrap()
}

fn states() -> Vec<UpdateState> {
    vec![
        UpdateState::UpToDate { checked_ms: 5 },
        UpdateState::Available {
            version: "0.2.0".into(),
        },
        UpdateState::Checking,
        UpdateState::Downloading,
        UpdateState::Verifying,
        UpdateState::Installing,
        UpdateState::Restarting,
        UpdateState::failed("no release key"),
    ]
}

#[test]
fn current_fills_proto_os_arch() {
    let i = info();
    assert_eq!(i.proto(), PROTO_VERSION);
    assert_eq!(i.os(), std::env::consts::OS);
    assert_eq!(i.arch(), std::env::consts::ARCH);
    const { assert!(MIN_DAEMON_PROTO <= PROTO_VERSION) };
}

#[test]
fn version_info_is_validated_on_construction_and_decode() {
    assert!(VersionInfo::current("", "x").is_err());
    assert!(VersionInfo::current("0.1.0", &"g".repeat(65)).is_err());
    assert!(VersionInfo::current("0.1\n.0", "x").is_err());
    let bad = r#"{"version":"1","git":"","proto":1,"os":"linux","arch":"x86_64"}"#;
    assert!(decode::<VersionInfo>(bad).is_err());
}

#[test]
fn failed_message_is_sanitized_and_bounded() {
    match UpdateState::failed(&format!("a\u{7}b{}", "x".repeat(900))) {
        UpdateState::Failed { message } => {
            assert_eq!(message.chars().count(), 500);
            assert!(message.starts_with("ab"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn update_state_json_shape() {
    let s = encode(&UpdateState::Available {
        version: "0.2.0".into(),
    })
    .unwrap();
    assert_eq!(s, r#"{"state":"available","version":"0.2.0"}"#);
}

#[test]
fn version_messages_roundtrip() {
    roundtrip(info());
    for s in states() {
        roundtrip(s.clone());
        roundtrip(DaemonToHub::UpdateStatus(s.clone()));
        roundtrip(HubToUi::UpdateStatus {
            node: node(),
            status: s,
        });
    }
    roundtrip(DaemonToHub::Version(info()));
    roundtrip(HubToDaemon::CheckUpdate);
    roundtrip(HubToDaemon::UpdateNow);
    for n in [None, Some(node())] {
        roundtrip(UiToHub::CheckUpdate { node: n.clone() });
        roundtrip(UiToHub::UpdateNode { node: n });
    }
    roundtrip(HubToUi::HubVersion(info()));
    roundtrip(HubToUi::NodeVersion {
        node: node(),
        info: info(),
    });
    assert_eq!(
        encode(&HubToDaemon::UpdateNow).unwrap(),
        r#"{"type":"update_now"}"#
    );
}
