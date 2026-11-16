//! Fixture states for `--snap --scene <name>` (one per panel).
use crate::{
    app::App,
    model::{apply, ConnStatus, Event, OtpOffer, Panel},
};
use rune::TextEdit;
use ulak_proto::{
    AgentId, AgentStatus, Blocker, BlockerId, BlockerKind, BusyReason, DeviceInfo, DeviceKind,
    HubToUi, Message, MessageId, MessageStatus, NodeId, NodeInfo, NodeState, Otp, ProfileKey,
    Question, QuestionId, Telemetry, ThreadId,
};

fn node(id: &str, host: &str, st: NodeState, cpu: f32, gpu: Option<f32>, ag: &[&str]) -> NodeInfo {
    NodeInfo::new(
        NodeId::new(id).unwrap(),
        host.into(),
        st,
        Telemetry::new(1.0, cpu, gpu, 40.0, 1).unwrap(),
        ag.iter().map(|a| AgentId::new(a).unwrap()).collect(),
    )
}

pub fn base(s: &mut App, p: Panel) {
    let u = &mut s.ui;
    u.conn = ConnStatus::Connected;
    u.panel = p;
    u.now_ms = 600_000;
    u.nodes = vec![
        node(
            "n1",
            "workstation",
            NodeState::Idle,
            23.0,
            Some(8.0),
            &["claude-1", "claude-2"],
        ),
        node(
            "n2",
            "gaming-pc",
            NodeState::Busy(BusyReason::Gaming {
                process: "eldenring.exe".into(),
            }),
            71.0,
            Some(96.0),
            &["codex-1"],
        ),
        node("n3", "laptop", NodeState::Offline, 0.0, None, &[]),
    ];
    u.questions = vec![
        Question::new(
            QuestionId::new("q1").unwrap(),
            AgentId::new("claude-1").unwrap(),
            "Should I delete the stale migration files before running the build?".into(),
            540_000,
        )
        .unwrap(),
        Question::new(
            QuestionId::new("q2").unwrap(),
            AgentId::new("codex-1").unwrap(),
            "Which branch should the fix target?".into(),
            590_000,
        )
        .unwrap(),
    ];
    for (a, st) in [
        ("claude-1", AgentStatus::Running),
        ("claude-2", AgentStatus::Failed),
        ("codex-1", AgentStatus::Starting),
    ] {
        u.statuses.insert(AgentId::new(a).unwrap(), st);
    }
}

pub fn mux(s: &mut App) {
    base(s, Panel::Terminal);
    let a = AgentId::new("claude-1").unwrap();
    s.ui.selected = Some(a.clone());
    let out = b"\x1b[1;32muser@ws\x1b[0m:\x1b[1;34m~/proj\x1b[0m$ cargo test\r\n   \x1b[32mCompiling\x1b[0m ulak v0.1.0\r\n\x1b[31merror\x1b[0m: could not compile\r\n\x1b[33mwarning\x1b[0m: 2 warnings\r\nuser@ws:~/proj$ ".to_vec();
    apply(
        &mut s.ui,
        Event::Hub(HubToUi::PaneOutput {
            agent: a,
            data: out,
        }),
    );
}

pub fn questions(s: &mut App) {
    base(s, Panel::Questions);
}

pub fn spawn(s: &mut App) {
    base(s, Panel::Spawn);
    s.spawn.agent_id = TextEdit::new("worker-1");
    s.spawn.workdir = TextEdit::new("/home/me/project");
    s.spawn.config_dir = TextEdit::new("/home/me/.claude-work");
    s.spawn.prompt = TextEdit::new("Fix the failing tests in crates/ulak\nand report back.");
    s.spawn.node = Some(NodeId::new("n2").unwrap());
    s.ui.blockers = vec![blocker("b1", "n2", BlockerKind::Login, "claude-1", 500_000)];
    // The form uses a config dir, so its profile key carries it.
    s.ui.blockers[0] = Blocker::new(
        s.ui.blockers[0].id().clone(),
        NodeId::new("n2").unwrap(),
        ProfileKey::new("claude@/home/me/.claude-work").unwrap(),
        BlockerKind::Login,
        None,
        "Not logged in for this config dir.".into(),
        500_000,
    );
}

fn blocker(id: &str, node: &str, kind: BlockerKind, agent: &str, at: u64) -> Blocker {
    let detail = match kind {
        BlockerKind::Login => "Not logged in. Run /login in the agent to sign in to your Claude account.",
        BlockerKind::Install => "npm ERR! code EACCES; npm ERR! syscall mkdir; npm ERR! path /usr/lib/node_modules/antigravity",
    };
    Blocker::new(
        BlockerId::new(id).unwrap(),
        NodeId::new(node).unwrap(),
        ProfileKey::new(if kind == BlockerKind::Login {
            "claude"
        } else {
            "antigravity"
        })
        .unwrap(),
        kind,
        Some(AgentId::new(agent).unwrap()),
        detail.into(),
        at,
    )
}

/// Busy day plus a login blocker and an install blocker, on the Questions panel.
pub fn blockers(s: &mut App) {
    busy(s);
    s.ui.panel = Panel::Questions;
    s.ui.blockers = vec![
        blocker("b1", "n2", BlockerKind::Login, "codex-1", 500_000),
        blocker("b2", "n3", BlockerKind::Install, "codex-2", 560_000),
    ];
    s.ui.statuses
        .insert(AgentId::new("codex-1").unwrap(), AgentStatus::NeedsLogin);
    s.ui.statuses
        .insert(AgentId::new("codex-2").unwrap(), AgentStatus::Installing);
}

pub fn mailbox(s: &mut App) {
    base(s, Panel::Mailbox);
    let msg = |id: &str, f: &str, t: &str, th: &str, p: &str, at: u64, st| {
        let mut m = Message::new(
            MessageId::new(id).unwrap(),
            AgentId::new(f).unwrap(),
            AgentId::new(t).unwrap(),
            ThreadId::new(th).unwrap(),
            p.into(),
            at,
        )
        .unwrap();
        m.status = st;
        m
    };
    s.ui.mailbox = vec![
        msg(
            "m1",
            "claude-1",
            "claude-2",
            "review",
            "Can you review the migration plan?",
            3_723_000,
            MessageStatus::Read,
        ),
        msg(
            "m2",
            "claude-2",
            "claude-1",
            "review",
            "Looks fine.\nOne nit on step 3.",
            3_790_000,
            MessageStatus::Delivered,
        ),
        msg(
            "m3",
            "codex-1",
            "claude-1",
            "build",
            "Build is green on main",
            3_900_000,
            MessageStatus::Pending,
        ),
    ];
}

pub fn devices(s: &mut App) {
    base(s, Panel::Devices);
    s.device_kind = 1;
    s.ui.otp = Some(OtpOffer {
        kind: DeviceKind::Ui,
        otp: Otp::new("K7QX92ML").unwrap(),
        hub_fingerprint: "AB:12:CD:34:EF:56:78:90:AB:12:CD:34:EF:56:78:90".into(),
        expires_ms: s.ui.now_ms + 272_000,
    });
    let d = |id: &str, kind, host: &str, at, revoked| DeviceInfo {
        id: NodeId::new(id).unwrap(),
        kind,
        hostname: host.into(),
        created_ms: at,
        revoked,
    };
    s.ui.devices = vec![
        d("n1", DeviceKind::Node, "workstation", 3_600_000, false),
        d("ui-1", DeviceKind::Ui, "laptop", 7_200_000, false),
        d("old-node", DeviceKind::Node, "retired-box", 1_000_000, true),
    ];
}

/// Devices panel with nodes in mixed update states and a version mismatch.
pub fn updates(s: &mut App) {
    use ulak_proto::{UpdateState as U, VersionInfo};
    devices(s);
    s.ui.otp = None;
    s.ui.devices.truncate(1);
    let v =
        |ver: &str, git: &str, proto| VersionInfo::new(ver, git, proto, "linux", "x86_64").unwrap();
    s.ui.nodes
        .push(node("n4", "build-box", NodeState::Idle, 12.0, None, &[]));
    s.ui.nodes
        .push(node("n5", "nas", NodeState::Idle, 5.0, None, &[]));
    s.ui.hub_version = Some(v("0.2.0", "def456", 1));
    s.ui.own_override = Some(v("0.2.0", "def456", 1));
    let long = "download https://github.com/example/ulak/releases/latest/download/ulakd-x86_64-unknown-linux-gnu.tar.gz failed: connection reset by peer while reading the body";
    for (id, ver, st) in [
        (
            "n1",
            v("0.1.0", "abc123", 1),
            U::Available {
                version: "0.2.0".into(),
            },
        ),
        ("n2", v("0.2.0", "def456", 1), U::Installing),
        (
            "n4",
            v("0.2.0", "def456", 1),
            U::UpToDate {
                checked_ms: 540_000,
            },
        ),
        ("n5", v("0.0.9", "0aa0aa", 0), U::failed(long)),
    ] {
        let n = NodeId::new(id).unwrap();
        s.ui.node_versions.insert(n.clone(), ver);
        s.ui.updates.insert(n, st);
    }
}

/// A realistic busy day: 3 nodes (one gaming), 5 agents, 2 questions, mail, a live terminal.
pub fn busy(s: &mut App) {
    mux(s);
    let u = &mut s.ui;
    u.nodes = vec![
        node(
            "n1",
            "workstation",
            NodeState::Idle,
            41.0,
            Some(12.0),
            &["claude-1", "claude-2", "claude-3"],
        ),
        node(
            "n2",
            "gaming-pc",
            NodeState::Busy(BusyReason::Gaming {
                process: "eldenring.exe".into(),
            }),
            71.0,
            Some(96.0),
            &["codex-1"],
        ),
        node("n3", "laptop", NodeState::Idle, 9.0, None, &["codex-2"]),
    ];
    for (a, st) in [
        ("claude-1", AgentStatus::Running),
        ("claude-2", AgentStatus::Running),
        ("claude-3", AgentStatus::Exited(Some(0))),
        ("codex-1", AgentStatus::Starting),
        ("codex-2", AgentStatus::Failed),
    ] {
        u.statuses.insert(AgentId::new(a).unwrap(), st);
    }
    let mut tmp = App::default();
    mailbox(&mut tmp);
    s.ui.mailbox = tmp.ui.mailbox;
}
