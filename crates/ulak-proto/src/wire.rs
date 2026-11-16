use crate::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

mod b64 {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonToHub {
    Hello {
        node: NodeInfo,
    },
    Heartbeat(Telemetry),
    AgentStatus {
        agent: AgentId,
        status: AgentStatus,
    },
    PaneOutput {
        agent: AgentId,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// The agent cannot work until the user fixes something (login) or ulakd could not install it.
    AgentBlocked {
        agent: AgentId,
        profile: ProfileKey,
        kind: BlockerKind,
        detail: String,
    },
    /// First thing a current daemon sends after connecting.
    Version(VersionInfo),
    UpdateStatus(UpdateState),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToDaemon {
    /// Look for a newer release in the daemon's own configured repo; report via `UpdateStatus`.
    CheckUpdate,
    /// Download, verify, install and restart; the daemon decides what to fetch.
    UpdateNow,
    /// The user fixed the blocker for this profile; spawning is allowed again.
    BlockerResolved {
        profile: ProfileKey,
    },
    Spawn(SpawnSpec),
    Kill {
        agent: AgentId,
    },
    /// Hub-written text typed into the agent's tmux pane (e.g. "check answer q-1").
    Nudge {
        agent: AgentId,
        text: String,
    },
    Attach {
        agent: AgentId,
    },
    Detach {
        agent: AgentId,
    },
    PaneInput {
        agent: AgentId,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiToHub {
    Spawn {
        target: SpawnTarget,
        spec: SpawnSpec,
    },
    Answer(Answer),
    Attach {
        agent: AgentId,
    },
    Detach {
        agent: AgentId,
    },
    PaneInput {
        agent: AgentId,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    Kill {
        agent: AgentId,
    },
    AddDevice {
        kind: DeviceKind,
    },
    RevokeDevice {
        id: NodeId,
    },
    ListDevices,
    ResolveBlocker {
        id: BlockerId,
    },
    /// `node: None` = every connected node.
    CheckUpdate {
        node: Option<NodeId>,
    },
    UpdateNode {
        node: Option<NodeId>,
    },
    /// Oldest first; `thread: None` means every thread.
    MailboxLog {
        thread: Option<ThreadId>,
        limit: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToUi {
    Snapshot {
        nodes: Vec<NodeInfo>,
        questions: Vec<Question>,
    },
    /// Sent right after Snapshot: every open blocker.
    Blockers {
        blockers: Vec<Blocker>,
    },
    BlockerOpened(Blocker),
    BlockerClosed {
        id: BlockerId,
    },
    NodeUpdate(NodeInfo),
    QuestionOpened(Question),
    QuestionClosed {
        id: QuestionId,
    },
    MessageLogged(Message),
    PaneOutput {
        agent: AgentId,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    AgentStatus {
        agent: AgentId,
        node: NodeId,
        status: AgentStatus,
    },
    OtpOffer {
        kind: DeviceKind,
        otp: Otp,
        hub_fingerprint: String,
        expires_ms: u64,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    MailboxLog {
        messages: Vec<Message>,
    },
    /// Sent right after Blockers on connect.
    HubVersion(VersionInfo),
    NodeVersion {
        node: NodeId,
        info: VersionInfo,
    },
    UpdateStatus {
        node: NodeId,
        status: UpdateState,
    },
    Error {
        message: String,
    },
}

pub fn encode<T: Serialize>(v: &T) -> Result<String, ProtoError> {
    Ok(serde_json::to_string(v)?)
}

pub fn decode<T: DeserializeOwned>(s: &str) -> Result<T, ProtoError> {
    Ok(serde_json::from_str(s)?)
}
