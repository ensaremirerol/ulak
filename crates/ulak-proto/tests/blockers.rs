use ulak_proto::*;

fn roundtrip<T>(v: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let s = encode(&v).unwrap();
    assert_eq!(decode::<T>(&s).unwrap(), v, "{s}");
}

fn key() -> ProfileKey {
    ProfileKey::new("claude@/home/u/.claude-b").unwrap()
}

fn blocker() -> Blocker {
    Blocker::new(
        BlockerId::new("b-1").unwrap(),
        NodeId::new("n-1").unwrap(),
        key(),
        BlockerKind::Login,
        Some(AgentId::new("a-1").unwrap()),
        "please log in".into(),
        7,
    )
}

#[test]
fn profile_key_comes_from_binary_name_and_config_dir() {
    let p = AgentProfile::new("/usr/bin/claude", vec![], None).unwrap();
    assert_eq!(p.key().unwrap().as_str(), "claude");
    let p = AgentProfile::new("claude", vec![], Some("/home/u/.claude-b".into())).unwrap();
    assert_eq!(p.key().unwrap(), key());
    let other = AgentProfile::new("codex", vec![], None).unwrap();
    assert_ne!(other.key().unwrap(), p.key().unwrap());
}

#[test]
fn profile_key_rejects_control_chars_empty_and_long() {
    assert!(ProfileKey::new("").is_err());
    assert!(ProfileKey::new("a\nb").is_err());
    assert!(ProfileKey::new(&"x".repeat(201)).is_err());
    assert!(ProfileKey::new(&"x".repeat(200)).is_ok());
    assert!(serde_json::from_str::<ProfileKey>("\"a\\u0000b\"").is_err());
    let long = AgentProfile::new("claude", vec![], Some("d".repeat(300).into())).unwrap();
    assert!(long.key().is_err());
}

#[test]
fn blocker_detail_is_stripped_and_capped() {
    let mut d = String::from("a\u{1b}[31mb\nc");
    d.push_str(&"z".repeat(600));
    let b = Blocker::new(
        BlockerId::new("b-1").unwrap(),
        NodeId::new("n-1").unwrap(),
        key(),
        BlockerKind::Install,
        None,
        d,
        0,
    );
    assert!(b.detail().chars().all(|c| !c.is_control()));
    assert_eq!(b.detail().chars().count(), 500);
    assert!(b.detail().starts_with("a[31mbc"));
}

#[test]
fn blocker_messages_roundtrip() {
    roundtrip(blocker());
    roundtrip(AgentStatus::Installing);
    roundtrip(AgentStatus::NeedsLogin);
    for kind in [BlockerKind::Login, BlockerKind::Install] {
        roundtrip(DaemonToHub::AgentBlocked {
            agent: AgentId::new("a-1").unwrap(),
            profile: key(),
            kind,
            detail: "x".into(),
        });
    }
    roundtrip(HubToDaemon::BlockerResolved { profile: key() });
    roundtrip(HubToUi::Blockers {
        blockers: vec![blocker()],
    });
    roundtrip(HubToUi::BlockerOpened(blocker()));
    roundtrip(HubToUi::BlockerClosed {
        id: BlockerId::new("b-1").unwrap(),
    });
    roundtrip(UiToHub::ResolveBlocker {
        id: BlockerId::new("b-1").unwrap(),
    });
}
