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
    /// Why an agent ended up `Failed` (sanitized, at most 300 chars, never a credential).
    AgentFailed {
        agent: AgentId,
        reason: String,
    },
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
    /// Sent right before the `Spawn` of a repo source, to the chosen node only: the token for
    /// cloning (and, if `allow_push`, pushing). Never log it, never write it to disk or the
    /// agent's environment beyond what git needs.
    GitAccess {
        agent: AgentId,
        token: GitToken,
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
#[allow(clippy::large_enum_variant)] // one short-lived frame per message; boxing would change every caller
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
    /// Validated against GitHub, then stored encrypted. Reply: `Credentials` or `Error`.
    AddCredential {
        name: String,
        token: GitToken,
    },
    /// Reply: `Credentials`.
    DeleteCredential {
        id: CredentialId,
    },
    ListCredentials,
    /// `page` is 1-based, 30 repos per page. `query` filters by substring of the full name.
    ListRepos {
        credential: CredentialId,
        query: Option<String>,
        page: u32,
    },
    ListBranches {
        credential: CredentialId,
        repo: RepoRef,
    },
    /// Removes a queued or failed item. Reply to everyone: `QueueRemoved`.
    CancelQueued {
        id: QueueId,
    },
    ListQueue,
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
    /// The hub's release cache (what nodes update from), after `HubVersion` on connect and on
    /// every change. `latest` is the newest verified version, `checked_ms` the last fetch attempt
    /// (0 = never), `error` the last failure text (never a token).
    ReleaseCache {
        latest: Option<String>,
        checked_ms: u64,
        error: Option<String>,
    },
    NodeVersion {
        node: NodeId,
        info: VersionInfo,
    },
    UpdateStatus {
        node: NodeId,
        status: UpdateState,
    },
    /// After `Blockers` on connect, only when at least one agent status is known.
    AgentStatuses {
        statuses: Vec<(AgentId, NodeId, AgentStatus)>,
    },
    /// Daemon-only origin; `node` is the daemon's connection, never the payload. Also replayed
    /// after `AgentStatuses` for agents whose status is `Failed`.
    AgentFailed {
        agent: AgentId,
        node: NodeId,
        reason: String,
    },
    Credentials {
        credentials: Vec<CredentialInfo>,
    },
    Repos {
        credential: CredentialId,
        query: Option<String>,
        repos: Vec<RepoInfo>,
        has_more: bool,
    },
    Branches {
        credential: CredentialId,
        repo: RepoRef,
        branches: Vec<String>,
        default_branch: String,
    },
    /// Full queue; on connect only when non-empty, and as the reply to `ListQueue`.
    Queue {
        items: Vec<QueuedSpawn>,
    },
    /// A new item, or an existing one whose state changed (e.g. to `Failed`).
    QueueChanged {
        item: QueuedSpawn,
    },
    /// Placed on a node, or cancelled.
    QueueRemoved {
        id: QueueId,
    },
    Error {
        message: String,
    },
}

/// Failure reason for the UI: control characters stripped, at most 300 chars. Callers must not
/// pass anything that contains a credential.
pub fn sanitize_reason(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(300).collect()
}

pub fn encode<T: Serialize>(v: &T) -> Result<String, ProtoError> {
    Ok(serde_json::to_string(v)?)
}

pub fn decode<T: DeserializeOwned>(s: &str) -> Result<T, ProtoError> {
    Ok(serde_json::from_str(s)?)
}
