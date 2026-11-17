use std::collections::BTreeMap;
use ulak_proto::*;

fn rt<T>(v: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let s = encode(&v).unwrap();
    assert_eq!(decode::<T>(&s).unwrap(), v, "{s}");
}

fn profile() -> AgentProfile {
    AgentProfile::new("claude", vec![], None).unwrap()
}
fn aid() -> AgentId {
    AgentId::new("a-1").unwrap()
}
fn cred() -> CredentialId {
    CredentialId::new("cred-1").unwrap()
}
fn repo_source() -> Source {
    Source::Repo {
        repo: RepoRef::new("octo-cat/hello.world_2").unwrap(),
        branch: Some(BranchName::new("feature/x").unwrap()),
        credential: cred(),
        allow_push: true,
    }
}

#[test]
fn repo_ref_rules() {
    for ok in [
        "a/b",
        "octocat/Hello-World",
        "o/.github",
        "o/a.b_c-d",
        "A1/x",
    ] {
        assert!(RepoRef::new(ok).is_ok(), "{ok}");
    }
    let long_owner = format!("{}/x", "a".repeat(40));
    let long_name = format!("o/{}", "a".repeat(101));
    for bad in [
        "",
        "a",
        "/b",
        "a/",
        "a/b/c",
        "-a/b",
        "a/-b",
        "a/..",
        "a/.",
        "a/b..c",
        "a b/c",
        "a/b c",
        "a_b/c",
        "a/b?x=1",
        "a/b\n",
        "../x",
        "a/ü",
        &long_owner,
        &long_name,
    ] {
        assert!(RepoRef::new(bad).is_err(), "{bad:?}");
    }
    assert!(decode::<RepoRef>("\"a/..\"").is_err());
    let r = RepoRef::new("octo/cat").unwrap();
    assert_eq!((r.owner(), r.name()), ("octo", "cat"));
}

#[test]
fn branch_name_rules() {
    for ok in ["main", "feature/x-1", "release_1.2", "a.b"] {
        assert!(BranchName::new(ok).is_ok(), "{ok}");
    }
    for bad in [
        "", "-x", "/x", "x/", "a..b", "a b", "a~b", "a^b", "a:b", "a?b", "a*b", "a[b", "a\\b",
        "a@{b", "a//b", "x.lock", "x.", "a\nb",
    ] {
        assert!(BranchName::new(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn token_is_redacted_and_validated_without_echo() {
    let t = GitToken::new("ghp_abcDEF1234567890").unwrap();
    assert_eq!(format!("{t:?}"), "GitToken(***)");
    let f = UiToHub::AddCredential {
        name: "me".into(),
        token: t.clone(),
    };
    assert!(!format!("{f:?}").contains("abcDEF"));
    let e = GitToken::new("bad token with spaces")
        .unwrap_err()
        .to_string();
    assert!(!e.contains("spaces"), "{e}");
    assert!(GitToken::new("short").is_err());
    assert_eq!(encode(&t).unwrap(), "\"ghp_abcDEF1234567890\"");
}

#[test]
fn folder_spec_is_unchanged_on_the_wire_and_old_frames_decode() {
    let s = SpawnSpec::new(
        aid(),
        profile(),
        "/srv/x".into(),
        "go".into(),
        BTreeMap::new(),
    )
    .unwrap();
    let json = encode(&s).unwrap();
    assert!(!json.contains("source"), "{json}");
    assert_eq!(
        s.source(),
        Source::Folder {
            path: "/srv/x".into()
        }
    );
    let back: SpawnSpec = decode(&json).unwrap();
    assert_eq!(back, s);
    back.check().unwrap();
}

#[test]
fn repo_spec_roundtrips_and_has_no_workdir() {
    let s = SpawnSpec::new_repo(
        aid(),
        profile(),
        repo_source(),
        "go".into(),
        BTreeMap::new(),
    )
    .unwrap();
    assert!(s.workdir().as_os_str().is_empty());
    assert_eq!(s.source(), repo_source());
    s.check().unwrap();
    rt(s.clone());
    let v: serde_json::Value = serde_json::from_str(&encode(&s).unwrap()).unwrap();
    assert_eq!(
        v["source"],
        serde_json::json!({"kind":"repo","repo":"octo-cat/hello.world_2","branch":"feature/x","credential":"cred-1","allow_push":true})
    );
    // Folder through new_repo is a plain folder spec.
    let f = SpawnSpec::new_repo(
        aid(),
        profile(),
        Source::Folder { path: "/a".into() },
        "go".into(),
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(f.workdir().to_str(), Some("/a"));
    // Branch and allow_push default.
    let src: Source = decode(r#"{"kind":"repo","repo":"o/n","credential":"c-1"}"#).unwrap();
    assert!(matches!(
        src,
        Source::Repo {
            branch: None,
            allow_push: false,
            ..
        }
    ));
    // Bad branch or repo fail on decode.
    assert!(
        decode::<Source>(r#"{"kind":"repo","repo":"o/n","branch":"-x","credential":"c"}"#).is_err()
    );
    assert!(decode::<Source>(r#"{"kind":"repo","repo":"o/..","credential":"c"}"#).is_err());
}

#[test]
fn check_rejects_inconsistent_specs() {
    let s: SpawnSpec = decode(
        r#"{"agent_id":"a","profile":{"binary":"c","args":[],"config_dir":null},"workdir":"/a","prompt":"p","env":{},"source":{"kind":"folder","path":"/b"}}"#,
    )
    .unwrap();
    assert!(s.check().is_err());
    let e: SpawnSpec = decode(
        r#"{"agent_id":"a","profile":{"binary":"c","args":[],"config_dir":null},"workdir":"","prompt":"p","env":{}}"#,
    )
    .unwrap();
    assert!(e.check().is_err());
}

#[test]
fn every_new_frame_roundtrips() {
    let info = CredentialInfo {
        id: cred(),
        name: "work".into(),
        kind: CredentialKind::Github,
        login: "octocat".into(),
        created_ms: 5,
    };
    let repo = RepoInfo {
        full_name: RepoRef::new("octo/cat").unwrap(),
        private: true,
        default_branch: "main".into(),
        pushed_at_ms: 9,
        description: None,
    };
    let q = QueuedSpawn {
        id: QueueId::new("q-1").unwrap(),
        agent: aid(),
        source: repo_source(),
        target: SpawnTarget::Node(NodeId::new("n-1").unwrap()),
        profile: ProfileKey::new("claude").unwrap(),
        prompt_preview: "go".into(),
        created_ms: 7,
        state: QueueState::Failed("boom".into()),
    };
    let spec = SpawnSpec::new_repo(
        aid(),
        profile(),
        repo_source(),
        "go".into(),
        BTreeMap::new(),
    )
    .unwrap();
    for f in [
        UiToHub::AddCredential {
            name: "w".into(),
            token: GitToken::new("ghp_12345678").unwrap(),
        },
        UiToHub::DeleteCredential { id: cred() },
        UiToHub::ListCredentials,
        UiToHub::ListRepos {
            credential: cred(),
            query: Some("x".into()),
            page: 2,
        },
        UiToHub::ListBranches {
            credential: cred(),
            repo: RepoRef::new("o/n").unwrap(),
        },
        UiToHub::CancelQueued { id: q.id.clone() },
        UiToHub::ListQueue,
        UiToHub::Spawn {
            target: SpawnTarget::Auto,
            spec: spec.clone(),
        },
    ] {
        rt(f);
    }
    for f in [
        HubToUi::Credentials {
            credentials: vec![info],
        },
        HubToUi::Repos {
            credential: cred(),
            query: None,
            repos: vec![repo],
            has_more: true,
        },
        HubToUi::Branches {
            credential: cred(),
            repo: RepoRef::new("o/n").unwrap(),
            branches: vec!["main".into()],
            default_branch: "main".into(),
        },
        HubToUi::Queue {
            items: vec![q.clone()],
        },
        HubToUi::QueueChanged { item: q.clone() },
        HubToUi::QueueRemoved { id: q.id },
        HubToUi::AgentStatuses {
            statuses: vec![(aid(), NodeId::new("n-1").unwrap(), AgentStatus::Running)],
        },
    ] {
        rt(f);
    }
    rt(HubToDaemon::GitAccess {
        agent: aid(),
        token: GitToken::new("ghp_12345678").unwrap(),
    });
    rt(HubToDaemon::Spawn(spec));
}

#[test]
fn queue_preview_is_one_short_line() {
    assert_eq!(QueuedSpawn::preview("\n  fix it\nmore"), "fix it");
    assert_eq!(QueuedSpawn::preview(&"x".repeat(500)).len(), 120);
}
