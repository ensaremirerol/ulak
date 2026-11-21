//! Wire fixtures for the web app (web/test/fixtures/wire.json): one value of every UiToHub and
//! HubToUi variant, serialised with `ulak_proto::encode`. The TypeScript decoder tests parse them.
//! Regenerate: `ULAK_WRITE_FIXTURES=1 cargo test -p ulak-proto --test ts_fixtures`.
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use ulak_proto::*;

const UI_VARIANTS: usize = 34;
const HUB_VARIANTS: usize = 38;

fn agent() -> AgentId {
    AgentId::new("reviewer-2").unwrap()
}
fn node() -> NodeId {
    NodeId::new("vps-fra").unwrap()
}
fn spec() -> SpawnSpec {
    let profile = AgentProfile::new(
        "claude",
        vec!["--model".into(), "opus".into()],
        Some("/home/u/.claude-b".into()),
    )
    .unwrap();
    let env = BTreeMap::from([("RUST_LOG".to_owned(), "info".to_owned())]);
    SpawnSpec::new(
        agent(),
        profile,
        "/srv/repo".into(),
        "fix the bug".into(),
        env,
    )
    .unwrap()
}
fn tel() -> Telemetry {
    Telemetry::new(0.25, 12.5, Some(40.0), 33.5, 1_700_000_000_000).unwrap()
}
fn info(state: NodeState) -> NodeInfo {
    NodeInfo::new(node(), "fra.example".into(), state, tel(), vec![agent()])
}
fn question() -> Question {
    Question::new(
        QuestionId::new("q-1").unwrap(),
        agent(),
        "Approve the migration?".into(),
        1_700_000_001_000,
    )
    .unwrap()
}
fn message() -> Message {
    Message::new(
        MessageId::new("m-1").unwrap(),
        agent(),
        AgentId::new("planner").unwrap(),
        ThreadId::new("t-1").unwrap(),
        "ready for review".into(),
        1_700_000_002_000,
    )
    .unwrap()
}
fn blocker() -> Blocker {
    Blocker::new(
        BlockerId::new("b-1").unwrap(),
        node(),
        ProfileKey::new("claude@/home/u/.claude-b").unwrap(),
        BlockerKind::Login,
        Some(agent()),
        "please log in".into(),
        1_700_000_003_000,
    )
}
fn repo_spec() -> SpawnSpec {
    let profile = AgentProfile::new("claude", vec![], None).unwrap();
    SpawnSpec::new_repo(
        agent(),
        profile,
        Source::Repo {
            repo: RepoRef::new("octocat/hello-world").unwrap(),
            branch: Some(BranchName::new("main").unwrap()),
            credential: CredentialId::new("cred-1").unwrap(),
            allow_push: false,
            fresh: false,
        },
        "fix the bug".into(),
        BTreeMap::new(),
    )
    .unwrap()
}
fn queued(state: QueueState) -> QueuedSpawn {
    QueuedSpawn {
        id: QueueId::new("q-1").unwrap(),
        agent: agent(),
        source: repo_spec().source(),
        target: SpawnTarget::Auto,
        profile: ProfileKey::new("claude").unwrap(),
        prompt_preview: "fix the bug".into(),
        created_ms: 1_700_000_004_000,
        state,
    }
}
fn cred_id() -> CredentialId {
    CredentialId::new("cred-1").unwrap()
}
fn version() -> VersionInfo {
    VersionInfo::new("0.1.2", "abc1234", 1, "linux", "x86_64").unwrap()
}

fn ui_name(f: &UiToHub) -> &'static str {
    match f {
        UiToHub::Spawn { .. } => "spawn",
        UiToHub::Answer(_) => "answer",
        UiToHub::Attach { .. } => "attach",
        UiToHub::Detach { .. } => "detach",
        UiToHub::PaneInput { .. } => "pane_input",
        UiToHub::Resize { .. } => "resize",
        UiToHub::AttachStream { .. } => "attach_stream",
        UiToHub::ListDir { .. } => "list_dir",
        UiToHub::Kill { .. } => "kill",
        UiToHub::AddDevice { .. } => "add_device",
        UiToHub::RevokeDevice { .. } => "revoke_device",
        UiToHub::ListDevices => "list_devices",
        UiToHub::ResolveBlocker { .. } => "resolve_blocker",
        UiToHub::CheckUpdate { .. } => "check_update",
        UiToHub::UpdateNode { .. } => "update_node",
        UiToHub::MailboxLog { .. } => "mailbox_log",
        UiToHub::AddCredential { .. } => "add_credential",
        UiToHub::DeleteCredential { .. } => "delete_credential",
        UiToHub::ListCredentials => "list_credentials",
        UiToHub::ListRepos { .. } => "list_repos",
        UiToHub::ListBranches { .. } => "list_branches",
        UiToHub::CancelQueued { .. } => "cancel_queued",
        UiToHub::ListQueue => "list_queue",
        UiToHub::CleanNode { .. } => "clean_node",
        UiToHub::CleanRepo { .. } => "clean_repo",
        UiToHub::SaveProfile { .. } => "save_profile",
        UiToHub::DeleteProfile { .. } => "delete_profile",
        UiToHub::SaveNodeSettings { .. } => "save_node_settings",
        UiToHub::NodeStats { .. } => "node_stats",
        UiToHub::InstallProfile { .. } => "install_profile",
        UiToHub::SpawnForIssue { .. } => "spawn_for_issue",
        UiToHub::ListIssues => "list_issues",
        UiToHub::SaveIssueSettings { .. } => "save_issue_settings",
        UiToHub::ResolveForward { .. } => "resolve_forward",
    }
}

fn hub_name(f: &HubToUi) -> &'static str {
    match f {
        HubToUi::Snapshot { .. } => "snapshot",
        HubToUi::Blockers { .. } => "blockers",
        HubToUi::BlockerOpened(_) => "blocker_opened",
        HubToUi::BlockerClosed { .. } => "blocker_closed",
        HubToUi::NodeUpdate(_) => "node_update",
        HubToUi::QuestionOpened(_) => "question_opened",
        HubToUi::QuestionClosed { .. } => "question_closed",
        HubToUi::MessageLogged(_) => "message_logged",
        HubToUi::PaneOutput { .. } => "pane_output",
        HubToUi::PaneStream { .. } => "pane_stream",
        HubToUi::DirListing { .. } => "dir_listing",
        HubToUi::AgentStatus { .. } => "agent_status",
        HubToUi::OtpOffer { .. } => "otp_offer",
        HubToUi::Devices { .. } => "devices",
        HubToUi::MailboxLog { .. } => "mailbox_log",
        HubToUi::HubVersion(_) => "hub_version",
        HubToUi::ReleaseCache { .. } => "release_cache",
        HubToUi::NodeVersion { .. } => "node_version",
        HubToUi::UpdateStatus { .. } => "update_status",
        HubToUi::AgentStatuses { .. } => "agent_statuses",
        HubToUi::AgentFailed { .. } => "agent_failed",
        HubToUi::Credentials { .. } => "credentials",
        HubToUi::Repos { .. } => "repos",
        HubToUi::Branches { .. } => "branches",
        HubToUi::Queue { .. } => "queue",
        HubToUi::QueueChanged { .. } => "queue_changed",
        HubToUi::QueueRemoved { .. } => "queue_removed",
        HubToUi::NodeProfiles { .. } => "node_profiles",
        HubToUi::NodeRepos { .. } => "node_repos",
        HubToUi::CleanReport { .. } => "clean_report",
        HubToUi::HubConfig { .. } => "hub_config",
        HubToUi::NodeStats { .. } => "node_stats",
        HubToUi::InstallStatus { .. } => "install_status",
        HubToUi::Issues { .. } => "issues",
        HubToUi::IssueSettings { .. } => "issue_settings",
        HubToUi::IssueLinks { .. } => "issue_links",
        HubToUi::Forwards { .. } => "forwards",
        HubToUi::Error { .. } => "error",
    }
}

fn issue_ref() -> IssueRef {
    IssueRef::parse("octocat/hello-world#64").unwrap()
}
fn issue_settings() -> IssueSettings {
    let mut s = IssueSettings {
        credential: Some(CredentialId::new("cred-1").unwrap()),
        tagged: vec![RepoRef::new("oxigraph/oxigraph").unwrap()],
        ..IssueSettings::default()
    };
    s.templates.truncate(1);
    s.templates[0].comment = true;
    s.routing.rules.push(RouteRule {
        repo: Some(RepoRef::new("octocat/hello-world").unwrap()),
        label: Some("docs".into()),
        words: vec!["readme".into()],
        to: AgentId::new("docs-sync").unwrap(),
        fallback: RouteFallback::NewAgent,
    });
    s
}
fn hub_profile() -> HubProfile {
    HubProfile {
        name: "claude-work".into(),
        binary: "claude".into(),
        args: vec!["--model".into(), "opus".into()],
        config_dir: Some("~/.claude-work".into()),
        env: BTreeMap::from([("HTTPS_PROXY".to_owned(), "http://proxy:3128".to_owned())]),
        install: Some("curl -fsSL https://example.org/install.sh | bash".into()),
    }
}

fn node_settings() -> NodeSettings {
    NodeSettings {
        repo_roots: Some(vec!["~/code".into()]),
        index_interval_mins: Some(30),
        clean_auto: Some(true),
        clean_after_exit_hours: None,
        auto_update: Some(false),
        cpu_busy_pct: Some(85.0),
        gpu_busy_pct: None,
    }
}

fn ui_frames() -> Vec<(&'static str, UiToHub)> {
    vec![
        (
            "spawn_auto",
            UiToHub::Spawn {
                target: SpawnTarget::Auto,
                spec: spec(),
            },
        ),
        (
            "spawn_node",
            UiToHub::Spawn {
                target: SpawnTarget::Node(node()),
                spec: spec(),
            },
        ),
        (
            "answer",
            UiToHub::Answer(Answer::new(QuestionId::new("q-1").unwrap(), "yes".into()).unwrap()),
        ),
        ("attach", UiToHub::Attach { agent: agent() }),
        ("detach", UiToHub::Detach { agent: agent() }),
        (
            "pane_input",
            UiToHub::PaneInput {
                agent: agent(),
                data: vec![0, 255, 27, b'[', b'A', b'\r'],
            },
        ),
        (
            "resize",
            UiToHub::Resize {
                agent: agent(),
                cols: 120,
                rows: 40,
            },
        ),
        (
            "attach_stream",
            UiToHub::AttachStream {
                agent: agent(),
                cols: 120,
                rows: 40,
            },
        ),
        (
            "list_dir",
            UiToHub::ListDir {
                node: node(),
                path: "~/workspace".into(),
            },
        ),
        ("kill", UiToHub::Kill { agent: agent() }),
        (
            "add_device_node",
            UiToHub::AddDevice {
                kind: DeviceKind::Node,
            },
        ),
        (
            "add_device_ui",
            UiToHub::AddDevice {
                kind: DeviceKind::Ui,
            },
        ),
        (
            "add_device_browser",
            UiToHub::AddDevice {
                kind: DeviceKind::Browser,
            },
        ),
        (
            "revoke_device",
            UiToHub::RevokeDevice {
                id: NodeId::new("pk-1a2b3c4d").unwrap(),
            },
        ),
        ("list_devices", UiToHub::ListDevices),
        (
            "resolve_blocker",
            UiToHub::ResolveBlocker {
                id: BlockerId::new("b-1").unwrap(),
            },
        ),
        (
            "spawn_repo",
            UiToHub::Spawn {
                target: SpawnTarget::Auto,
                spec: repo_spec(),
            },
        ),
        (
            "add_credential",
            UiToHub::AddCredential {
                name: "work".into(),
                token: GitToken::new("ghp_exampleExampleExample0123").unwrap(),
            },
        ),
        (
            "delete_credential",
            UiToHub::DeleteCredential { id: cred_id() },
        ),
        ("list_credentials", UiToHub::ListCredentials),
        (
            "list_repos",
            UiToHub::ListRepos {
                credential: cred_id(),
                query: None,
                page: 1,
            },
        ),
        (
            "list_repos_query",
            UiToHub::ListRepos {
                credential: cred_id(),
                query: Some("hello".into()),
                page: 2,
            },
        ),
        (
            "list_branches",
            UiToHub::ListBranches {
                credential: cred_id(),
                repo: RepoRef::new("octocat/hello-world").unwrap(),
            },
        ),
        (
            "cancel_queued",
            UiToHub::CancelQueued {
                id: QueueId::new("q-1").unwrap(),
            },
        ),
        ("list_queue", UiToHub::ListQueue),
        (
            "save_profile_global",
            UiToHub::SaveProfile {
                node: None,
                profile: hub_profile(),
            },
        ),
        (
            "save_profile_node",
            UiToHub::SaveProfile {
                node: Some(node()),
                profile: HubProfile {
                    name: "claude-personal".into(),
                    binary: "claude".into(),
                    args: vec![],
                    config_dir: Some("~/.claude-personal".into()),
                    env: BTreeMap::new(),
                    install: None,
                },
            },
        ),
        (
            "delete_profile",
            UiToHub::DeleteProfile {
                node: Some(node()),
                name: "claude-personal".into(),
            },
        ),
        (
            "save_node_settings",
            UiToHub::SaveNodeSettings {
                node: node(),
                settings: node_settings(),
            },
        ),
        ("node_stats", UiToHub::NodeStats { node: node() }),
        (
            "install_profile_node",
            UiToHub::InstallProfile {
                node: Some(node()),
                name: "claude-work".into(),
            },
        ),
        (
            "install_profile_all",
            UiToHub::InstallProfile {
                node: None,
                name: "claude-work".into(),
            },
        ),
        (
            "clean_node_dry",
            UiToHub::CleanNode {
                node: node(),
                dry_run: true,
            },
        ),
        (
            "clean_repo",
            UiToHub::CleanRepo {
                node: node(),
                path: "/home/u/.config/ulakd/work/reviewer-2/hello-world".into(),
            },
        ),
        (
            "spawn_for_issue",
            UiToHub::SpawnForIssue {
                target: SpawnTarget::Auto,
                spec: repo_spec(),
                issue: IssueStart {
                    issue: issue_ref(),
                    branch: Some("issue-64".into()),
                    comment: true,
                    route: true,
                },
            },
        ),
        ("list_issues", UiToHub::ListIssues),
        (
            "save_issue_settings",
            UiToHub::SaveIssueSettings {
                settings: issue_settings(),
            },
        ),
        (
            "resolve_forward",
            UiToHub::ResolveForward {
                id: "f-1".into(),
                to: Some(agent()),
            },
        ),
        ("check_update_all", UiToHub::CheckUpdate { node: None }),
        (
            "check_update_node",
            UiToHub::CheckUpdate { node: Some(node()) },
        ),
        ("update_node_all", UiToHub::UpdateNode { node: None }),
        (
            "update_node_one",
            UiToHub::UpdateNode { node: Some(node()) },
        ),
        (
            "mailbox_log_all",
            UiToHub::MailboxLog {
                thread: None,
                limit: 200,
            },
        ),
        (
            "mailbox_log_thread",
            UiToHub::MailboxLog {
                thread: Some(ThreadId::new("t-1").unwrap()),
                limit: 50,
            },
        ),
    ]
}

fn hub_frames() -> Vec<(&'static str, HubToUi)> {
    let status = |s| HubToUi::AgentStatus {
        agent: agent(),
        node: node(),
        status: s,
    };
    let upd = |s| HubToUi::UpdateStatus {
        node: node(),
        status: s,
    };
    vec![
        (
            "snapshot",
            HubToUi::Snapshot {
                nodes: vec![info(NodeState::Idle)],
                questions: vec![question()],
            },
        ),
        (
            "blockers",
            HubToUi::Blockers {
                blockers: vec![blocker()],
            },
        ),
        ("blocker_opened", HubToUi::BlockerOpened(blocker())),
        (
            "blocker_closed",
            HubToUi::BlockerClosed {
                id: BlockerId::new("b-1").unwrap(),
            },
        ),
        (
            "node_update_idle",
            HubToUi::NodeUpdate(info(NodeState::Idle)),
        ),
        (
            "node_update_busy_gaming",
            HubToUi::NodeUpdate(info(NodeState::Busy(BusyReason::Gaming {
                process: "steam".into(),
            }))),
        ),
        (
            "node_update_busy_load",
            HubToUi::NodeUpdate(info(NodeState::Busy(BusyReason::Load))),
        ),
        (
            "node_update_offline",
            HubToUi::NodeUpdate(info(NodeState::Offline)),
        ),
        ("question_opened", HubToUi::QuestionOpened(question())),
        (
            "question_closed",
            HubToUi::QuestionClosed {
                id: QuestionId::new("q-1").unwrap(),
            },
        ),
        ("message_logged", HubToUi::MessageLogged(message())),
        (
            "pane_output",
            HubToUi::PaneOutput {
                agent: agent(),
                data: vec![0, 255, 27, b'[', b'H', 0xe2, 0x9c, 0x93],
            },
        ),
        (
            "pane_stream",
            HubToUi::PaneStream {
                agent: agent(),
                data: vec![27, b'[', b'?', b'1', b'0', b'4', b'9', b'h', 0xe2, 0x9c],
            },
        ),
        (
            "dir_listing",
            HubToUi::DirListing {
                node: node(),
                listing: DirListing {
                    requested: "~/workspace".into(),
                    path: "/home/me/workspace".into(),
                    dirs: vec!["ulak".into(), "notes".into()],
                    error: None,
                },
            },
        ),
        (
            "dir_listing_error",
            HubToUi::DirListing {
                node: node(),
                listing: DirListing {
                    requested: "/etc".into(),
                    path: String::new(),
                    dirs: vec![],
                    error: Some(
                        "only folders under your home or a repo root can be browsed".into(),
                    ),
                },
            },
        ),
        ("agent_status_starting", status(AgentStatus::Starting)),
        ("agent_status_running", status(AgentStatus::Running)),
        (
            "agent_status_exited_code",
            status(AgentStatus::Exited(Some(1))),
        ),
        (
            "agent_status_exited_unknown",
            status(AgentStatus::Exited(None)),
        ),
        ("agent_status_failed", status(AgentStatus::Failed)),
        ("agent_status_installing", status(AgentStatus::Installing)),
        ("agent_status_needs_login", status(AgentStatus::NeedsLogin)),
        (
            "otp_offer",
            HubToUi::OtpOffer {
                kind: DeviceKind::Browser,
                otp: Otp::new("ABCD2345").unwrap(),
                hub_fingerprint: "ab".repeat(32),
                expires_ms: 1_700_000_300_000,
            },
        ),
        (
            "devices",
            HubToUi::Devices {
                devices: vec![
                    DeviceInfo {
                        id: node(),
                        kind: DeviceKind::Node,
                        hostname: "fra.example".into(),
                        created_ms: 1_700_000_000_000,
                        revoked: false,
                    },
                    DeviceInfo {
                        id: NodeId::new("pk-1a2b3c4d").unwrap(),
                        kind: DeviceKind::Browser,
                        hostname: "my phone".into(),
                        created_ms: 1_700_000_100_000,
                        revoked: true,
                    },
                ],
            },
        ),
        (
            "mailbox_log",
            HubToUi::MailboxLog {
                messages: vec![message()],
            },
        ),
        ("hub_version", HubToUi::HubVersion(version())),
        (
            "release_cache",
            HubToUi::ReleaseCache {
                latest: Some("0.1.4".into()),
                checked_ms: 1_700_000_007_000,
                error: Some("release fetch failed: HTTP 404".into()),
            },
        ),
        (
            "node_version",
            HubToUi::NodeVersion {
                node: node(),
                info: version(),
            },
        ),
        (
            "update_status_up_to_date",
            upd(UpdateState::UpToDate {
                checked_ms: 1_700_000_000_000,
            }),
        ),
        (
            "update_status_available",
            upd(UpdateState::Available {
                version: "0.2.0".into(),
            }),
        ),
        ("update_status_checking", upd(UpdateState::Checking)),
        ("update_status_downloading", upd(UpdateState::Downloading)),
        ("update_status_verifying", upd(UpdateState::Verifying)),
        ("update_status_installing", upd(UpdateState::Installing)),
        ("update_status_restarting", upd(UpdateState::Restarting)),
        (
            "update_status_failed",
            upd(UpdateState::failed("no release signing key configured")),
        ),
        (
            "agent_statuses",
            HubToUi::AgentStatuses {
                statuses: vec![
                    (agent(), node(), AgentStatus::Running),
                    (
                        AgentId::new("planner").unwrap(),
                        node(),
                        AgentStatus::Exited(Some(0)),
                    ),
                ],
            },
        ),
        (
            "agent_failed",
            HubToUi::AgentFailed {
                agent: agent(),
                node: node(),
                reason: "clone failed: repository not found".into(),
            },
        ),
        (
            "credentials",
            HubToUi::Credentials {
                credentials: vec![CredentialInfo {
                    id: cred_id(),
                    name: "work".into(),
                    kind: CredentialKind::Github,
                    login: "octocat".into(),
                    created_ms: 1_700_000_005_000,
                }],
            },
        ),
        (
            "repos",
            HubToUi::Repos {
                credential: cred_id(),
                query: Some("hello".into()),
                repos: vec![
                    RepoInfo {
                        full_name: RepoRef::new("octocat/hello-world").unwrap(),
                        private: false,
                        default_branch: "main".into(),
                        pushed_at_ms: 1_700_000_006_000,
                        description: Some("My first repository".into()),
                    },
                    RepoInfo {
                        full_name: RepoRef::new("octocat/secret").unwrap(),
                        private: true,
                        default_branch: "trunk".into(),
                        pushed_at_ms: 0,
                        description: None,
                    },
                ],
                has_more: true,
            },
        ),
        (
            "branches",
            HubToUi::Branches {
                credential: cred_id(),
                repo: RepoRef::new("octocat/hello-world").unwrap(),
                branches: vec!["main".into(), "feature/x".into()],
                default_branch: "main".into(),
            },
        ),
        (
            "queue",
            HubToUi::Queue {
                items: vec![queued(QueueState::Queued)],
            },
        ),
        (
            "queue_changed_failed",
            HubToUi::QueueChanged {
                item: queued(QueueState::Failed("credential cred-1 is gone".into())),
            },
        ),
        (
            "queue_removed",
            HubToUi::QueueRemoved {
                id: QueueId::new("q-1").unwrap(),
            },
        ),
        (
            "node_profiles",
            HubToUi::NodeProfiles {
                node: node(),
                profiles: vec![
                    ProfileInfo {
                        name: "claude-work".into(),
                        binary: "claude".into(),
                        config_dir: Some("/home/u/.claude-work".into()),
                        installed: true,
                        login_blocked: false,
                        from_hub: true,
                        installable: true,
                    },
                    ProfileInfo {
                        name: "antigravity".into(),
                        binary: "antigravity".into(),
                        config_dir: None,
                        installed: false,
                        login_blocked: true,
                        from_hub: false,
                        installable: false,
                    },
                ],
            },
        ),
        (
            "node_repos",
            HubToUi::NodeRepos {
                node: node(),
                repos: vec![RepoEntry {
                    full_name: RepoRef::new("octocat/hello-world").unwrap(),
                    path: "/home/u/.config/ulakd/work/reviewer-2/hello-world".into(),
                    branch: Some("main".into()),
                    clean: true,
                    ahead: Some(0),
                    behind: Some(2),
                    last_fetch_ms: Some(1_700_000_007_000),
                    in_use: false,
                    managed: true,
                }],
            },
        ),
        (
            "clean_report",
            HubToUi::CleanReport {
                node: node(),
                dry_run: true,
                removed: vec![CleanItem {
                    path: "/home/u/.config/ulakd/work/reviewer-2/hello-world".into(),
                    full_name: Some(RepoRef::new("octocat/hello-world").unwrap()),
                    reason: "up to date with the remote (last fetch)".into(),
                }],
                kept: vec![CleanItem {
                    path: "/home/u/code/other".into(),
                    full_name: None,
                    reason: "not under the work root".into(),
                }],
            },
        ),
        (
            "hub_config",
            HubToUi::HubConfig {
                profiles: vec![
                    ScopedProfile {
                        node: None,
                        profile: hub_profile(),
                    },
                    ScopedProfile {
                        node: Some(node()),
                        profile: HubProfile {
                            name: "claude-personal".into(),
                            binary: "claude".into(),
                            args: vec![],
                            config_dir: Some("~/.claude-personal".into()),
                            env: BTreeMap::new(),
                            install: None,
                        },
                    },
                ],
                settings: vec![NodeSettingsEntry {
                    node: node(),
                    settings: node_settings(),
                }],
            },
        ),
        (
            "node_stats",
            HubToUi::NodeStats {
                node: node(),
                connected_ms: Some(1_700_000_000_000),
                samples: vec![
                    Telemetry::new(0.5, 12.5, Some(3.0), 40.0, 1_700_000_005_000).unwrap(),
                    Telemetry::new(0.7, 20.0, None, 41.5, 1_700_000_010_000).unwrap(),
                ],
            },
        ),
        (
            "install_status_started",
            HubToUi::InstallStatus {
                node: node(),
                name: "claude-work".into(),
                done: false,
                ok: false,
                detail: String::new(),
            },
        ),
        (
            "install_status_failed",
            HubToUi::InstallStatus {
                node: node(),
                name: "claude-work".into(),
                done: true,
                ok: false,
                detail: "installing claude failed (exit status: 1): npm ERR! EACCES".into(),
            },
        ),
        (
            "issues",
            HubToUi::Issues {
                issues: vec![Issue {
                    issue: issue_ref(),
                    title: "Install fails without root".into(),
                    body: "npm needs root\n```\nEACCES\n```".into(),
                    author: "octocat".into(),
                    trusted: true,
                    labels: vec!["bug".into()],
                    comments: 3,
                    updated_ms: 1_700_000_004_000,
                    url: "https://github.com/octocat/hello-world/issues/64".into(),
                    tagged: false,
                }],
                errors: vec!["other/gone: not found (or the token cannot see it)".into()],
            },
        ),
        (
            "issue_settings",
            HubToUi::IssueSettings {
                settings: issue_settings(),
            },
        ),
        (
            "issue_links",
            HubToUi::IssueLinks {
                links: vec![IssueLink {
                    agent: agent(),
                    issue: issue_ref(),
                    branch: Some("issue-64".into()),
                    comment: true,
                    route: false,
                    at_ms: 1_700_000_005_000,
                    commented_start: true,
                    pr: Some(65),
                }],
            },
        ),
        (
            "forwards",
            HubToUi::Forwards {
                forwards: vec![Forward {
                    id: "f-1".into(),
                    issue: issue_ref(),
                    from: agent(),
                    to: None,
                    reason: "nobody owns this".into(),
                    status: ForwardStatus::NeedsOwner,
                    note: "No routing rule matched.".into(),
                    title: "Install fails without root".into(),
                    at_ms: 1_700_000_006_000,
                }],
            },
        ),
        (
            "error",
            HubToUi::Error {
                message: "node vps-fra is offline".into(),
            },
        ),
    ]
}

fn section<T: serde::Serialize>(frames: &[(&str, T)]) -> Value {
    let mut m = Map::new();
    for (k, f) in frames {
        let v: Value = serde_json::from_str(&encode(f).unwrap()).unwrap();
        assert!(
            m.insert((*k).to_owned(), v).is_none(),
            "duplicate fixture {k}"
        );
    }
    Value::Object(m)
}

#[test]
fn web_fixtures_cover_every_variant_and_are_current() {
    let ui = ui_frames();
    let hub = hub_frames();
    let uniq = |names: Vec<&str>| {
        names
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    assert_eq!(
        uniq(ui.iter().map(|(_, f)| ui_name(f)).collect()),
        UI_VARIANTS,
        "a UiToHub variant has no fixture"
    );
    assert_eq!(
        uniq(hub.iter().map(|(_, f)| hub_name(f)).collect()),
        HUB_VARIANTS,
        "a HubToUi variant has no fixture"
    );
    let doc = serde_json::json!({ "ui_to_hub": section(&ui), "hub_to_ui": section(&hub) });
    let text = serde_json::to_string_pretty(&doc).unwrap() + "\n";
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/test/fixtures/wire.json");
    if std::env::var("ULAK_WRITE_FIXTURES").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    } else {
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(have, text, "web/test/fixtures/wire.json is stale: run ULAK_WRITE_FIXTURES=1 cargo test -p ulak-proto --test ts_fixtures");
    }
}
