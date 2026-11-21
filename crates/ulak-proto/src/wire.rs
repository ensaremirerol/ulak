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
    /// Raw bytes of the agent's tmux client (stream mode, `AttachStream`): a byte stream, not redraws.
    PaneStream {
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
    /// The node's named profiles; sent after Hello and whenever they change.
    Profiles {
        profiles: Vec<ProfileInfo>,
    },
    /// The node's repo index (at most 200 entries); sent after Hello and whenever it changes.
    RepoIndex {
        repos: Vec<RepoEntry>,
    },
    /// Result of `CleanRepos` / `CleanRepo` or of the automatic cleanup. With `dry_run`,
    /// `removed` lists what WOULD be removed.
    CleanReport {
        dry_run: bool,
        removed: Vec<CleanItem>,
        kept: Vec<CleanItem>,
    },
    /// An `InstallProfile` started (`done: false`) or finished (`done: true`; `ok` and `detail`:
    /// the error, or the output tail on success; at most 300 chars, never a credential).
    /// A successful install also re-sends `Profiles`.
    InstallStatus {
        name: String,
        done: bool,
        ok: bool,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToDaemon {
    /// Hub-managed profiles (already merged for this node: node-scoped beats global) and
    /// settings. Sent after the daemon's `Version` and whenever either changes; each one
    /// replaces the previous. Not persisted by the daemon: a restart waits for the next one.
    Config {
        profiles: Vec<HubProfile>,
        settings: NodeSettings,
    },
    /// Delete every eligible clone under the daemon's work root; answered with `CleanReport`.
    CleanRepos {
        dry_run: bool,
    },
    /// Delete one clone; `path` must equal the path of an indexed work-root entry.
    CleanRepo {
        path: String,
    },
    /// Run the profile's install command now (even if installed: a reinstall/upgrade). Answered
    /// with `InstallStatus`; a profile without an install command answers `ok: false` at once.
    InstallProfile {
        name: String,
    },
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
    /// Resize the agent's pane (tmux window) to the viewer's terminal. Nodes before 0.1.10 ignore it.
    Resize {
        agent: AgentId,
        cols: u16,
        rows: u16,
    },
    /// Stream the agent's terminal (a tmux client in a PTY) as `PaneStream`; a repeat restarts the
    /// client, so every viewer gets a full repaint. Nodes before 0.1.11 ignore it.
    AttachStream {
        agent: AgentId,
        cols: u16,
        rows: u16,
    },
    /// No stream viewer is left for the agent.
    DetachStream {
        agent: AgentId,
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
    /// The viewer's terminal size for the agent's pane; the last resize wins across viewers.
    Resize {
        agent: AgentId,
        cols: u16,
        rows: u16,
    },
    /// Watch the agent as a raw terminal stream (`PaneStream`) instead of redraws (`Attach`).
    /// `Detach` ends either kind.
    AttachStream {
        agent: AgentId,
        cols: u16,
        rows: u16,
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
    /// Create or replace a hub profile (`node: None` = global). Reply: `HubConfig` (broadcast).
    SaveProfile {
        node: Option<NodeId>,
        profile: HubProfile,
    },
    /// Delete a hub profile. Reply: `HubConfig` (broadcast).
    DeleteProfile {
        node: Option<NodeId>,
        name: String,
    },
    /// Replace a node's hub-stored settings (all `None` = delete them). Reply: `HubConfig`.
    SaveNodeSettings {
        node: NodeId,
        settings: NodeSettings,
    },
    /// Run a profile's install command on a node, or (`node: None`) on every connected node that
    /// reports the profile as installable. Progress: `InstallStatus` (broadcast).
    InstallProfile {
        node: Option<NodeId>,
        name: String,
    },
    /// Telemetry history of a node. Reply: `NodeStats` (to the requester only).
    NodeStats {
        node: NodeId,
    },
    /// Clean a node's work-root clones (`dry_run`: only report). Reply: `CleanReport`.
    CleanNode {
        node: NodeId,
        dry_run: bool,
    },
    /// Clean one indexed work-root clone of a node. Reply: `CleanReport`.
    CleanRepo {
        node: NodeId,
        path: String,
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
    /// Raw bytes of the agent's tmux client (stream mode, `AttachStream`), relayed from the node: a byte stream, not redraws.
    PaneStream {
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
    /// A node's named profiles (relay of `DaemonToHub::Profiles`); replayed per node on connect.
    NodeProfiles {
        node: NodeId,
        profiles: Vec<ProfileInfo>,
    },
    /// A node's repo index (relay of `DaemonToHub::RepoIndex`); replayed per node on connect.
    NodeRepos {
        node: NodeId,
        repos: Vec<RepoEntry>,
    },
    /// All hub profiles and per-node settings; sent on connect and broadcast on every change.
    HubConfig {
        profiles: Vec<ScopedProfile>,
        settings: Vec<NodeSettingsEntry>,
    },
    /// Reply to `UiToHub::NodeStats`: the hub's in-memory history (oldest first, at most one
    /// hour), and when the node's current connection started (`None`: offline).
    NodeStats {
        node: NodeId,
        connected_ms: Option<u64>,
        samples: Vec<Telemetry>,
    },
    /// Relay of `DaemonToHub::InstallStatus`.
    InstallStatus {
        node: NodeId,
        name: String,
        done: bool,
        ok: bool,
        detail: String,
    },
    /// Relay of `DaemonToHub::CleanReport`.
    CleanReport {
        node: NodeId,
        dry_run: bool,
        removed: Vec<CleanItem>,
        kept: Vec<CleanItem>,
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
