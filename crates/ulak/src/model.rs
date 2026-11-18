//! UI state, the reducer (`apply`) and the intent -> wire-frame mapping. No window, no network.
use crate::term::Screen;
use std::collections::BTreeMap;
use ulak_proto::{
    AgentId, AgentProfile, AgentStatus, Answer, Blocker, BlockerId, BlockerKind, DeviceInfo,
    DeviceKind, HubToUi, Message, NodeId, NodeInfo, NodeState, Otp, ProtoError, Question,
    QuestionId, SpawnSpec, SpawnTarget, ThreadId, UiToHub, UpdateState, VersionInfo,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnStatus {
    #[default]
    Connecting,
    Connected,
    Disconnected(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    #[default]
    Fleet,
    Terminal,
    Questions,
    Mailbox,
    Devices,
    Spawn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OtpOffer {
    pub kind: DeviceKind,
    pub otp: Otp,
    pub hub_fingerprint: String,
    pub expires_ms: u64,
}

/// A connection-level event from the net layer, or the clock.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // transient UI events; boxing buys nothing
pub enum Event {
    Hub(HubToUi),
    Connecting,
    Connected,
    Disconnected(String),
    /// Wall clock (unix ms), for question ages.
    Tick(u64),
}

/// Raw form fields of the Spawn panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnForm {
    /// Empty = let the hub pick (`SpawnTarget::Auto`).
    pub node: String,
    pub agent_id: String,
    pub binary: String,
    /// Whitespace-separated.
    pub args: String,
    pub workdir: String,
    pub prompt: String,
    /// Optional `CLAUDE_CONFIG_DIR`-style profile directory; empty = none.
    pub config_dir: String,
}

impl Default for SpawnForm {
    fn default() -> Self {
        Self {
            node: String::new(),
            agent_id: String::new(),
            binary: "claude".into(),
            args: String::new(),
            workdir: String::new(),
            prompt: String::new(),
            config_dir: String::new(),
        }
    }
}

impl SpawnForm {
    pub fn to_frame(&self) -> Result<UiToHub, ProtoError> {
        let target = match self.node.trim() {
            "" => SpawnTarget::Auto,
            n => SpawnTarget::Node(NodeId::new(n)?),
        };
        let profile = AgentProfile::new(
            self.binary.trim(),
            self.args.split_whitespace().map(str::to_owned).collect(),
            Some(self.config_dir.trim())
                .filter(|d| !d.is_empty())
                .map(Into::into),
        )?;
        let spec = SpawnSpec::new(
            AgentId::new(self.agent_id.trim())?,
            profile,
            self.workdir.trim().into(),
            self.prompt.clone(),
            BTreeMap::new(),
        )?;
        Ok(UiToHub::Spawn { target, spec })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    SelectAgent(AgentId),
    Detach,
    SendKeys(Vec<u8>),
    AnswerQuestion {
        question: QuestionId,
        text: String,
    },
    Spawn(SpawnForm),
    Kill(AgentId),
    AddDevice(DeviceKind),
    RevokeDevice(NodeId),
    RefreshDevices,
    RefreshMailbox(Option<ThreadId>),
    SwitchPanel(Panel),
    ResolveBlocker {
        id: BlockerId,
    },
    /// `None` = every connected node.
    CheckUpdate {
        node: Option<NodeId>,
    },
    UpdateNode {
        node: Option<NodeId>,
    },
}

pub const MAILBOX_LIMIT: u32 = 200;
/// An error toast disappears this long after the first tick that saw it.
pub const ERROR_TTL_MS: u64 = 8_000;

#[derive(Debug, Default)]
pub struct State {
    pub nodes: Vec<NodeInfo>,
    pub questions: Vec<Question>,
    /// Open blockers, oldest first.
    pub blockers: Vec<Blocker>,
    pub statuses: BTreeMap<AgentId, AgentStatus>,
    pub mailbox: Vec<Message>,
    pub devices: Vec<DeviceInfo>,
    pub otp: Option<OtpOffer>,
    pub screens: BTreeMap<AgentId, Screen>,
    pub selected: Option<AgentId>,
    pub panel: Panel,
    pub last_error: Option<String>,
    pub conn: ConnStatus,
    pub now_ms: u64,
    /// (message, tick time it was first seen) for the toast auto-expiry.
    pub error_seen: Option<(String, u64)>,
    pub hub_version: Option<VersionInfo>,
    pub node_versions: BTreeMap<NodeId, VersionInfo>,
    pub updates: BTreeMap<NodeId, UpdateState>,
    /// Test/scene override of this binary's version; `own_version()` when `None`.
    pub own_override: Option<VersionInfo>,
}

/// This binary's version (`ULAK_GIT_SHA` is set by build.rs).
pub fn own_version() -> VersionInfo {
    version_or_unknown(VersionInfo::current(
        env!("CARGO_PKG_VERSION"),
        env!("ULAK_GIT_SHA"),
    ))
}

/// The UI must never panic over its own version line: a failure shows as "unknown".
fn version_or_unknown(r: Result<VersionInfo, ulak_proto::ProtoError>) -> VersionInfo {
    r.unwrap_or_else(|_| VersionInfo::unknown())
}

/// How a version differs from the hub's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    DiffersFromHub,
    ProtocolTooOld,
}

impl Flag {
    pub fn label(self) -> &'static str {
        match self {
            Flag::DiffersFromHub => "differs from hub",
            Flag::ProtocolTooOld => "protocol too old",
        }
    }
}

/// Compare `v` with the hub: older protocol wins over any other difference.
pub fn flag(hub: &VersionInfo, v: &VersionInfo) -> Option<Flag> {
    if v.proto() < hub.proto() {
        Some(Flag::ProtocolTooOld)
    } else if v.proto() != hub.proto()
        || v.version() != hub.version()
        || !(v.git().starts_with(hub.git()) || hub.git().starts_with(v.git()))
    {
        Some(Flag::DiffersFromHub)
    } else {
        None
    }
}

/// The version an update would install, when one is available.
pub fn available(u: &UpdateState) -> Option<&str> {
    match u {
        UpdateState::Available { version } => Some(version),
        _ => None,
    }
}

impl State {
    pub fn own(&self) -> VersionInfo {
        self.own_override.clone().unwrap_or_else(own_version)
    }

    /// Flag of the UI itself against the hub (None while the hub version is unknown).
    pub fn own_flag(&self) -> Option<Flag> {
        self.hub_version.as_ref().and_then(|h| flag(h, &self.own()))
    }

    pub fn node_flag(&self, n: &NodeId) -> Option<Flag> {
        let h = self.hub_version.as_ref()?;
        flag(h, self.node_versions.get(n)?)
    }

    pub fn updates_available(&self) -> bool {
        self.updates.values().any(|u| available(u).is_some())
    }

    fn forget_offline(&mut self) {
        for n in self.nodes.iter().filter(|n| n.state == NodeState::Offline) {
            self.updates.remove(&n.id);
            self.node_versions.remove(&n.id);
        }
    }

    fn age_error(&mut self) {
        match (&self.last_error, &self.error_seen) {
            (None, _) => self.error_seen = None,
            (Some(e), Some((seen, at))) if e == seen => {
                if self.now_ms.saturating_sub(*at) >= ERROR_TTL_MS {
                    self.last_error = None;
                    self.error_seen = None;
                }
            }
            (Some(e), _) => self.error_seen = Some((e.clone(), self.now_ms)),
        }
    }

    pub fn question_age_ms(&self, q: &Question) -> u64 {
        self.now_ms.saturating_sub(q.at_ms())
    }

    pub fn node_of(&self, agent: &AgentId) -> Option<&NodeInfo> {
        self.nodes.iter().find(|n| n.agents.contains(agent))
    }

    pub fn selected_screen(&self) -> Option<&Screen> {
        self.selected.as_ref().and_then(|a| self.screens.get(a))
    }

    /// Frames to send right after (re)connecting: the hub forgets attachments with the socket.
    pub fn resync_frames(&self) -> Vec<UiToHub> {
        self.selected
            .iter()
            .map(|a| UiToHub::Attach { agent: a.clone() })
            .collect()
    }

    /// Frames for `intent` against the current state. Pure; see `dispatch` for local effects.
    pub fn intent_to_frames(&self, intent: Intent) -> Vec<UiToHub> {
        self.try_frames(intent).unwrap_or_default()
    }

    fn try_frames(&self, intent: Intent) -> Result<Vec<UiToHub>, ProtoError> {
        Ok(match intent {
            Intent::SelectAgent(a) if self.selected.as_ref() == Some(&a) => vec![],
            Intent::SelectAgent(a) => self
                .selected
                .iter()
                .map(|p| UiToHub::Detach { agent: p.clone() })
                .chain([UiToHub::Attach { agent: a }])
                .collect(),
            Intent::Detach => self
                .selected
                .iter()
                .map(|a| UiToHub::Detach { agent: a.clone() })
                .collect(),
            Intent::SendKeys(data) => self
                .selected
                .iter()
                .filter(|_| !data.is_empty())
                .map(|a| UiToHub::PaneInput {
                    agent: a.clone(),
                    data: data.clone(),
                })
                .collect(),
            Intent::AnswerQuestion { question, text } => {
                vec![UiToHub::Answer(Answer::new(question, text)?)]
            }
            Intent::Spawn(form) => vec![form.to_frame()?],
            Intent::Kill(agent) => vec![UiToHub::Kill { agent }],
            Intent::AddDevice(kind) => vec![UiToHub::AddDevice { kind }],
            Intent::RevokeDevice(id) => vec![UiToHub::RevokeDevice { id }],
            Intent::RefreshDevices => vec![UiToHub::ListDevices],
            Intent::RefreshMailbox(thread) => {
                vec![UiToHub::MailboxLog {
                    thread,
                    limit: MAILBOX_LIMIT,
                }]
            }
            Intent::SwitchPanel(_) => vec![],
            Intent::ResolveBlocker { id } => vec![UiToHub::ResolveBlocker { id }],
            Intent::CheckUpdate { node } => vec![UiToHub::CheckUpdate { node }],
            Intent::UpdateNode { node } => vec![UiToHub::UpdateNode { node }],
        })
    }

    /// Frames for `intent`, then its local effects (selection, panel, form errors).
    pub fn dispatch(&mut self, intent: Intent) -> Vec<UiToHub> {
        let frames = match self.try_frames(intent.clone()) {
            Ok(f) => f,
            Err(e) => {
                self.last_error = Some(e.to_string());
                return vec![];
            }
        };
        match intent {
            Intent::SelectAgent(a) => {
                self.selected = Some(a);
                self.panel = Panel::Terminal;
            }
            Intent::Detach => self.selected = None,
            Intent::Kill(a) if self.selected.as_ref() == Some(&a) => self.selected = None,
            Intent::SwitchPanel(p) => self.panel = p,
            Intent::AddDevice(_) => self.otp = None,
            _ => {}
        }
        frames
    }
}

fn upsert<T>(list: &mut Vec<T>, item: T, same: impl Fn(&T, &T) -> bool) {
    match list.iter_mut().find(|x| same(x, &item)) {
        Some(slot) => *slot = item,
        None => list.push(item),
    }
}

/// Insert or replace by id, keeping the list ordered by time.
fn add_blocker(s: &mut State, b: Blocker) {
    upsert(&mut s.blockers, b, |a, b| a.id() == b.id());
    s.blockers.sort_by_key(Blocker::at_ms);
}

impl State {
    /// Open blockers of `profile` (a `ProfileKey` string) on `node`, or on any node for `None`.
    pub fn blockers_for(&self, node: Option<&NodeId>, profile: &str) -> Vec<&Blocker> {
        self.blockers
            .iter()
            .filter(|b| b.profile().as_str() == profile && node.is_none_or(|n| b.node() == n))
            .collect()
    }
}

/// "Login required" / "Install failed".
pub fn kind_title(k: BlockerKind) -> &'static str {
    match k {
        BlockerKind::Login => "Login required",
        BlockerKind::Install => "Install failed",
    }
}

pub fn apply(state: &mut State, event: Event) {
    match event {
        Event::Connecting => state.conn = ConnStatus::Connecting,
        Event::Connected => {
            state.conn = ConnStatus::Connected;
            state.last_error = None;
        }
        Event::Disconnected(why) => state.conn = ConnStatus::Disconnected(why),
        Event::Tick(ms) => {
            state.now_ms = ms;
            state.age_error();
        }
        Event::Hub(f) => apply_hub(state, f),
    }
}

fn apply_hub(s: &mut State, f: HubToUi) {
    match f {
        HubToUi::Snapshot { nodes, questions } => {
            s.nodes = nodes;
            s.questions = questions;
            s.forget_offline();
        }
        HubToUi::NodeUpdate(n) => {
            upsert(&mut s.nodes, n, |a, b| a.id == b.id);
            s.forget_offline();
        }
        HubToUi::QuestionOpened(q) => upsert(&mut s.questions, q, |a, b| a.id() == b.id()),
        HubToUi::QuestionClosed { id } => s.questions.retain(|q| q.id() != &id),
        HubToUi::MessageLogged(m) => upsert(&mut s.mailbox, m, |a, b| a.id() == b.id()),
        HubToUi::MailboxLog { messages } => s.mailbox = messages,
        HubToUi::PaneOutput { agent, data } => s.screens.entry(agent).or_default().feed(&data),
        HubToUi::AgentStatus {
            agent,
            node,
            status,
        } => {
            if let Some(n) = s.nodes.iter_mut().find(|n| n.id == node) {
                if !n.agents.contains(&agent) {
                    n.agents.push(agent.clone());
                }
            }
            s.statuses.insert(agent, status);
        }
        HubToUi::OtpOffer {
            kind,
            otp,
            hub_fingerprint,
            expires_ms,
        } => {
            s.otp = Some(OtpOffer {
                kind,
                otp,
                hub_fingerprint,
                expires_ms,
            });
        }
        HubToUi::Devices { devices } => s.devices = devices,
        HubToUi::Blockers { blockers } => {
            s.blockers.clear();
            for b in blockers {
                add_blocker(s, b);
            }
        }
        HubToUi::BlockerOpened(b) => add_blocker(s, b),
        HubToUi::BlockerClosed { id } => s.blockers.retain(|b| b.id() != &id),
        HubToUi::HubVersion(v) => s.hub_version = Some(v),
        HubToUi::NodeVersion { node, info } => {
            s.node_versions.insert(node, info);
        }
        HubToUi::UpdateStatus { node, status } => {
            s.updates.insert(node, status);
        }
        HubToUi::Error { message } => s.last_error = Some(message),
        // Frames added later by the hub must not break this build.
        #[allow(unreachable_patterns)]
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ulak_proto::Telemetry;

    fn aid(s: &str) -> AgentId {
        AgentId::new(s).unwrap()
    }
    fn nid(s: &str) -> NodeId {
        NodeId::new(s).unwrap()
    }
    fn node(id: &str, host: &str, agents: &[&str]) -> NodeInfo {
        NodeInfo::new(
            nid(id),
            host.into(),
            NodeState::Idle,
            Telemetry::new(0.5, 10.0, None, 20.0, 1).unwrap(),
            agents.iter().map(|a| aid(a)).collect(),
        )
    }
    fn question(id: &str, at: u64) -> Question {
        Question::new(QuestionId::new(id).unwrap(), aid("a1"), "why?".into(), at).unwrap()
    }
    fn message(id: &str) -> Message {
        Message::new(
            ulak_proto::MessageId::new(id).unwrap(),
            aid("a1"),
            aid("a2"),
            ThreadId::new("t1").unwrap(),
            "hello".into(),
            5,
        )
        .unwrap()
    }
    fn hub(s: &mut State, f: HubToUi) {
        apply(s, Event::Hub(f));
    }

    #[test]
    fn own_version_never_panics_and_falls_back() {
        let v = own_version();
        assert!(!v.os().is_empty() && !v.arch().is_empty());
        let bad = VersionInfo::new("", "x", 1, "web", "wasm32");
        assert_eq!(version_or_unknown(bad).os(), "unknown");
    }

    #[test]
    fn snapshot_replaces_nodes_and_questions() {
        let mut s = State::default();
        hub(
            &mut s,
            HubToUi::Snapshot {
                nodes: vec![node("n1", "a", &[])],
                questions: vec![question("q1", 1)],
            },
        );
        hub(
            &mut s,
            HubToUi::Snapshot {
                nodes: vec![node("n2", "b", &[])],
                questions: vec![],
            },
        );
        assert_eq!(s.nodes.len(), 1);
        assert_eq!(s.nodes[0].id, nid("n2"));
        assert!(s.questions.is_empty());
    }

    #[test]
    fn node_update_replaces_by_id_or_appends() {
        let mut s = State::default();
        hub(&mut s, HubToUi::NodeUpdate(node("n1", "old", &[])));
        hub(&mut s, HubToUi::NodeUpdate(node("n2", "x", &[])));
        hub(&mut s, HubToUi::NodeUpdate(node("n1", "new", &["a1"])));
        assert_eq!(s.nodes.len(), 2);
        assert_eq!(s.nodes[0].hostname, "new");
        assert_eq!(s.node_of(&aid("a1")).map(|n| n.id.clone()), Some(nid("n1")));
    }

    #[test]
    fn question_open_close_and_age() {
        let mut s = State::default();
        hub(&mut s, HubToUi::QuestionOpened(question("q1", 1_000)));
        hub(&mut s, HubToUi::QuestionOpened(question("q1", 1_000)));
        hub(&mut s, HubToUi::QuestionOpened(question("q2", 2_000)));
        assert_eq!(s.questions.len(), 2);
        apply(&mut s, Event::Tick(4_000));
        assert_eq!(s.question_age_ms(&s.questions[0]), 3_000);
        hub(
            &mut s,
            HubToUi::QuestionClosed {
                id: QuestionId::new("q1").unwrap(),
            },
        );
        assert_eq!(s.questions.len(), 1);
        assert_eq!(s.questions[0].id().as_str(), "q2");
    }

    #[test]
    fn age_never_underflows_when_the_clock_is_behind() {
        let s = State::default();
        assert_eq!(s.question_age_ms(&question("q1", 99)), 0);
    }

    #[test]
    fn answering_leaves_the_question_until_the_hub_closes_it() {
        let mut s = State::default();
        hub(&mut s, HubToUi::QuestionOpened(question("q1", 1)));
        let frames = s.dispatch(Intent::AnswerQuestion {
            question: QuestionId::new("q1").unwrap(),
            text: "yes".into(),
        });
        assert_eq!(
            frames,
            vec![UiToHub::Answer(
                Answer::new(QuestionId::new("q1").unwrap(), "yes".into()).unwrap()
            )]
        );
        assert_eq!(s.questions.len(), 1);
        hub(
            &mut s,
            HubToUi::QuestionClosed {
                id: QuestionId::new("q1").unwrap(),
            },
        );
        assert!(s.questions.is_empty());
    }

    #[test]
    fn empty_answer_sends_nothing_and_reports() {
        let mut s = State::default();
        let f = s.dispatch(Intent::AnswerQuestion {
            question: QuestionId::new("q1").unwrap(),
            text: String::new(),
        });
        assert!(f.is_empty());
        assert!(s.last_error.is_some());
    }

    #[test]
    fn mailbox_appends_dedupes_and_log_replaces() {
        let mut s = State::default();
        hub(&mut s, HubToUi::MessageLogged(message("m1")));
        hub(&mut s, HubToUi::MessageLogged(message("m1")));
        hub(&mut s, HubToUi::MessageLogged(message("m2")));
        assert_eq!(s.mailbox.len(), 2);
        hub(
            &mut s,
            HubToUi::MailboxLog {
                messages: vec![message("m9")],
            },
        );
        assert_eq!(s.mailbox.len(), 1);
        assert_eq!(s.mailbox[0].id().as_str(), "m9");
    }

    #[test]
    fn agent_status_is_kept_and_registers_the_agent_on_its_node() {
        let mut s = State::default();
        hub(&mut s, HubToUi::NodeUpdate(node("n1", "a", &[])));
        hub(
            &mut s,
            HubToUi::AgentStatus {
                agent: aid("a1"),
                node: nid("n1"),
                status: AgentStatus::Running,
            },
        );
        assert_eq!(s.statuses[&aid("a1")], AgentStatus::Running);
        assert_eq!(s.nodes[0].agents, vec![aid("a1")]);
        hub(
            &mut s,
            HubToUi::AgentStatus {
                agent: aid("a1"),
                node: nid("n1"),
                status: AgentStatus::Exited(Some(0)),
            },
        );
        assert_eq!(s.statuses[&aid("a1")], AgentStatus::Exited(Some(0)));
        assert_eq!(s.nodes[0].agents.len(), 1);
    }

    #[test]
    fn pane_output_goes_to_the_right_agents_screen() {
        let mut s = State::default();
        hub(
            &mut s,
            HubToUi::PaneOutput {
                agent: aid("a1"),
                data: b"\x1b[H\x1b[2Jone".to_vec(),
            },
        );
        hub(
            &mut s,
            HubToUi::PaneOutput {
                agent: aid("a2"),
                data: b"two".to_vec(),
            },
        );
        assert_eq!(s.screens[&aid("a1")].line(0), "one");
        assert_eq!(s.screens[&aid("a2")].line(0), "two");
        hub(
            &mut s,
            HubToUi::PaneOutput {
                agent: aid("a1"),
                data: b"\x1b[H\x1b[2Jredraw".to_vec(),
            },
        );
        assert_eq!(s.screens[&aid("a1")].line(0), "redraw");
        s.selected = Some(aid("a2"));
        assert_eq!(s.selected_screen().unwrap().line(0), "two");
    }

    #[test]
    fn otp_offer_devices_and_error() {
        let mut s = State::default();
        hub(
            &mut s,
            HubToUi::OtpOffer {
                kind: DeviceKind::Ui,
                otp: Otp::new("ABCD2345").unwrap(),
                hub_fingerprint: "AA:BB".into(),
                expires_ms: 9,
            },
        );
        let o = s.otp.clone().unwrap();
        assert_eq!(
            (o.otp.expose(), o.hub_fingerprint.as_str(), o.expires_ms),
            ("ABCD2345", "AA:BB", 9)
        );
        let d = DeviceInfo {
            id: nid("d1"),
            kind: DeviceKind::Ui,
            hostname: "h".into(),
            created_ms: 1,
            revoked: false,
        };
        hub(
            &mut s,
            HubToUi::Devices {
                devices: vec![d.clone()],
            },
        );
        hub(
            &mut s,
            HubToUi::Devices {
                devices: vec![d.clone(), d],
            },
        );
        assert_eq!(s.devices.len(), 2);
        hub(
            &mut s,
            HubToUi::Error {
                message: "nope".into(),
            },
        );
        assert_eq!(s.last_error.as_deref(), Some("nope"));
    }

    #[test]
    fn connection_events_set_status_and_connected_clears_the_error() {
        let mut s = State::default();
        assert_eq!(s.conn, ConnStatus::Connecting);
        s.last_error = Some("old".into());
        apply(&mut s, Event::Connected);
        assert_eq!(
            (s.conn.clone(), s.last_error.clone()),
            (ConnStatus::Connected, None)
        );
        apply(&mut s, Event::Disconnected("eof".into()));
        assert_eq!(s.conn, ConnStatus::Disconnected("eof".into()));
        apply(&mut s, Event::Connecting);
        assert_eq!(s.conn, ConnStatus::Connecting);
    }

    #[test]
    fn selecting_an_agent_detaches_the_previous_one_first() {
        let mut s = State::default();
        assert_eq!(
            s.dispatch(Intent::SelectAgent(aid("a1"))),
            vec![UiToHub::Attach { agent: aid("a1") }]
        );
        assert_eq!(
            (s.selected.clone(), s.panel),
            (Some(aid("a1")), Panel::Terminal)
        );
        assert_eq!(
            s.dispatch(Intent::SelectAgent(aid("a2"))),
            vec![
                UiToHub::Detach { agent: aid("a1") },
                UiToHub::Attach { agent: aid("a2") }
            ]
        );
        assert!(s.dispatch(Intent::SelectAgent(aid("a2"))).is_empty());
        assert_eq!(
            s.dispatch(Intent::Detach),
            vec![UiToHub::Detach { agent: aid("a2") }]
        );
        assert_eq!(s.selected, None);
        assert!(s.dispatch(Intent::Detach).is_empty());
    }

    #[test]
    fn send_keys_targets_the_selected_agent_only() {
        let mut s = State::default();
        assert!(s.dispatch(Intent::SendKeys(b"x".to_vec())).is_empty());
        s.dispatch(Intent::SelectAgent(aid("a1")));
        assert_eq!(
            s.dispatch(Intent::SendKeys(b"ls\r".to_vec())),
            vec![UiToHub::PaneInput {
                agent: aid("a1"),
                data: b"ls\r".to_vec()
            }]
        );
        assert!(s.dispatch(Intent::SendKeys(vec![])).is_empty());
    }

    #[test]
    fn spawn_form_builds_a_frame_or_reports() {
        let mut s = State::default();
        let form = SpawnForm {
            agent_id: "worker-1".into(),
            workdir: "/tmp".into(),
            prompt: "do it".into(),
            args: "--a  --b".into(),
            ..SpawnForm::default()
        };
        let frames = s.dispatch(Intent::Spawn(form.clone()));
        let [UiToHub::Spawn { target, spec }] = &frames[..] else {
            panic!("{frames:?}")
        };
        assert_eq!(*target, SpawnTarget::Auto);
        assert_eq!(
            (
                spec.agent_id().as_str(),
                spec.profile().binary(),
                spec.profile().args()
            ),
            (
                "worker-1",
                "claude",
                &["--a".to_owned(), "--b".to_owned()][..]
            )
        );
        let node_form = SpawnForm {
            node: "n1".into(),
            ..form.clone()
        };
        let UiToHub::Spawn { target, .. } = node_form.to_frame().unwrap() else {
            panic!()
        };
        assert_eq!(target, SpawnTarget::Node(nid("n1")));
        let bad = SpawnForm {
            agent_id: "Bad Id".into(),
            ..form
        };
        assert!(s.dispatch(Intent::Spawn(bad)).is_empty());
        assert!(s.last_error.is_some());
    }

    #[test]
    fn config_dir_goes_into_the_profile() {
        let f = SpawnForm {
            agent_id: "a1".into(),
            workdir: "/w".into(),
            prompt: "p".into(),
            config_dir: " /home/me/.claude-p ".into(),
            ..SpawnForm::default()
        };
        let UiToHub::Spawn { spec, .. } = f.to_frame().unwrap() else {
            panic!()
        };
        assert_eq!(
            spec.profile().config_dir().map(|p| p.as_path()),
            Some(std::path::Path::new("/home/me/.claude-p"))
        );
    }

    #[test]
    fn error_expires_after_ttl_of_ticks() {
        let mut s = State::default();
        apply(&mut s, Event::Tick(1_000));
        s.last_error = Some("boom".into());
        apply(&mut s, Event::Tick(2_000));
        apply(&mut s, Event::Tick(2_000 + ERROR_TTL_MS - 1));
        assert!(s.last_error.is_some());
        apply(&mut s, Event::Tick(2_000 + ERROR_TTL_MS));
        assert_eq!(s.last_error, None);
        s.last_error = Some("again".into());
        apply(&mut s, Event::Tick(20_000));
        apply(&mut s, Event::Tick(20_001));
        assert!(s.last_error.is_some(), "a new error restarts the clock");
    }

    #[test]
    fn simple_intents_map_to_one_frame() {
        let s = State::default();
        assert_eq!(
            s.intent_to_frames(Intent::Kill(aid("a1"))),
            vec![UiToHub::Kill { agent: aid("a1") }]
        );
        assert_eq!(
            s.intent_to_frames(Intent::AddDevice(DeviceKind::Browser)),
            vec![UiToHub::AddDevice {
                kind: DeviceKind::Browser
            }]
        );
        assert_eq!(
            s.intent_to_frames(Intent::RevokeDevice(nid("d1"))),
            vec![UiToHub::RevokeDevice { id: nid("d1") }]
        );
        assert_eq!(
            s.intent_to_frames(Intent::RefreshDevices),
            vec![UiToHub::ListDevices]
        );
        assert_eq!(
            s.intent_to_frames(Intent::RefreshMailbox(None)),
            vec![UiToHub::MailboxLog {
                thread: None,
                limit: MAILBOX_LIMIT
            }]
        );
        assert!(s
            .intent_to_frames(Intent::SwitchPanel(Panel::Devices))
            .is_empty());
    }

    #[test]
    fn local_effects_of_switch_kill_and_add_device() {
        let mut s = State::default();
        s.dispatch(Intent::SwitchPanel(Panel::Mailbox));
        assert_eq!(s.panel, Panel::Mailbox);
        s.dispatch(Intent::SelectAgent(aid("a1")));
        s.dispatch(Intent::Kill(aid("a2")));
        assert_eq!(s.selected, Some(aid("a1")));
        s.dispatch(Intent::Kill(aid("a1")));
        assert_eq!(s.selected, None);
        hub(
            &mut s,
            HubToUi::OtpOffer {
                kind: DeviceKind::Ui,
                otp: Otp::new("ABCD2345").unwrap(),
                hub_fingerprint: String::new(),
                expires_ms: 1,
            },
        );
        s.dispatch(Intent::AddDevice(DeviceKind::Ui));
        assert!(s.otp.is_none());
    }

    #[test]
    fn resync_reattaches_the_selected_agent() {
        let mut s = State::default();
        assert!(s.resync_frames().is_empty());
        s.dispatch(Intent::SelectAgent(aid("a1")));
        assert_eq!(
            s.resync_frames(),
            vec![UiToHub::Attach { agent: aid("a1") }]
        );
    }

    fn blocker(id: &str, n: &str, kind: BlockerKind, at: u64) -> Blocker {
        Blocker::new(
            BlockerId::new(id).unwrap(),
            nid(n),
            ulak_proto::ProfileKey::new("claude").unwrap(),
            kind,
            Some(aid("a1")),
            "d".into(),
            at,
        )
    }

    #[test]
    fn blockers_dedupe_sort_close_and_resolve_frame() {
        let mut s = State::default();
        let b1 = blocker("b1", "n1", BlockerKind::Login, 20);
        let b2 = blocker("b2", "n2", BlockerKind::Install, 10);
        apply(
            &mut s,
            Event::Hub(HubToUi::Blockers {
                blockers: vec![b1.clone(), b2],
            }),
        );
        assert_eq!(
            s.blockers
                .iter()
                .map(|b| b.id().as_str())
                .collect::<Vec<_>>(),
            ["b2", "b1"]
        );
        apply(&mut s, Event::Hub(HubToUi::BlockerOpened(b1)));
        assert_eq!(s.blockers.len(), 2);
        assert_eq!(s.blockers_for(Some(&nid("n1")), "claude").len(), 1);
        assert_eq!(s.blockers_for(None, "claude").len(), 2);
        assert_eq!(s.blockers_for(None, "other").len(), 0);
        let id = BlockerId::new("b2").unwrap();
        apply(
            &mut s,
            Event::Hub(HubToUi::BlockerClosed { id: id.clone() }),
        );
        assert_eq!(s.blockers.len(), 1);
        assert_eq!(
            s.intent_to_frames(Intent::ResolveBlocker { id: id.clone() }),
            vec![UiToHub::ResolveBlocker { id }]
        );
    }

    fn vi(ver: &str, git: &str, proto: u32) -> VersionInfo {
        VersionInfo::new(ver, git, proto, "linux", "x86_64").unwrap()
    }

    #[test]
    fn flag_rules() {
        let h = vi("0.2.0", "aaa", 2);
        assert_eq!(flag(&h, &vi("0.2.0", "aaa", 2)), None);
        assert_eq!(flag(&h, &vi("0.1.0", "aaa", 2)), Some(Flag::DiffersFromHub));
        assert_eq!(flag(&h, &vi("0.2.0", "bbb", 2)), Some(Flag::DiffersFromHub));
        assert_eq!(flag(&h, &vi("0.2.0", "aaa", 3)), Some(Flag::DiffersFromHub));
        assert_eq!(flag(&h, &vi("0.2.0", "aaa", 1)), Some(Flag::ProtocolTooOld));
        assert_eq!(Flag::ProtocolTooOld.label(), "protocol too old");
        assert!(available(&UpdateState::Available {
            version: "1".into()
        })
        .is_some());
        assert!(available(&UpdateState::Checking).is_none());
    }

    #[test]
    fn version_frames_fill_state_and_offline_clears_them() {
        let mut s = State::default();
        hub(&mut s, HubToUi::NodeUpdate(node("n1", "a", &[])));
        hub(&mut s, HubToUi::HubVersion(vi("0.2.0", "h", 1)));
        hub(
            &mut s,
            HubToUi::NodeVersion {
                node: nid("n1"),
                info: vi("0.1.0", "x", 1),
            },
        );
        hub(
            &mut s,
            HubToUi::UpdateStatus {
                node: nid("n1"),
                status: UpdateState::Checking,
            },
        );
        assert_eq!(s.hub_version.as_ref().unwrap().git(), "h");
        assert_eq!(s.node_flag(&nid("n1")), Some(Flag::DiffersFromHub));
        assert_eq!(s.updates[&nid("n1")], UpdateState::Checking);
        let mut off = node("n1", "a", &[]);
        off.state = NodeState::Offline;
        hub(&mut s, HubToUi::NodeUpdate(off));
        assert!(s.updates.is_empty() && s.node_versions.is_empty());
    }

    #[test]
    fn update_intents_map_to_frames() {
        let s = State::default();
        let n = Some(nid("n1"));
        assert_eq!(
            s.intent_to_frames(Intent::CheckUpdate { node: n.clone() }),
            vec![UiToHub::CheckUpdate { node: n.clone() }]
        );
        assert_eq!(
            s.intent_to_frames(Intent::UpdateNode { node: None }),
            vec![UiToHub::UpdateNode { node: None }]
        );
    }
}
