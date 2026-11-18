use crate::ProtoError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

fn valid_slug(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn non_empty(s: &str, field: &'static str) -> Result<(), ProtoError> {
    if s.is_empty() {
        Err(ProtoError::Empty { field })
    } else {
        Ok(())
    }
}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn new(s: &str) -> Result<Self, ProtoError> {
                if valid_slug(s) {
                    Ok(Self(s.to_owned()))
                } else {
                    Err(ProtoError::InvalidId {
                        kind: stringify!($name),
                        value: s.to_owned(),
                    })
                }
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = ProtoError;
            fn try_from(s: String) -> Result<Self, ProtoError> {
                Self::new(&s)
            }
        }
        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
    };
}
id_type!(NodeId);
id_type!(AgentId);
id_type!(ThreadId);
id_type!(MessageId);
id_type!(QuestionId);
id_type!(BlockerId);
id_type!(CredentialId);
id_type!(QueueId);

impl AgentId {
    pub fn tmux_session(&self) -> String {
        format!("ulak-{}", self.0)
    }
}

/// One-time enrollment code shown in the UI. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Otp(String);
impl Otp {
    pub fn new(s: &str) -> Result<Self, ProtoError> {
        if s.len() == 8
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            Ok(Self(s.to_owned()))
        } else {
            Err(ProtoError::InvalidOtp)
        }
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Otp {
    type Error = ProtoError;
    fn try_from(s: String) -> Result<Self, ProtoError> {
        Self::new(&s)
    }
}
impl From<Otp> for String {
    fn from(v: Otp) -> String {
        v.0
    }
}
impl std::fmt::Debug for Otp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Otp(***)")
    }
}

/// Bearer token an agent uses toward the hub's MCP endpoint. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToken(String);
impl AgentToken {
    pub fn new(s: String) -> Result<Self, ProtoError> {
        non_empty(&s, "agent token")?;
        Ok(Self(s))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for AgentToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentToken(***)")
    }
}

// ponytail: only Telemetry re-validates on decode; other DTOs trust the authenticated (mTLS) peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TelemetryRaw")]
pub struct Telemetry {
    load1: f32,
    cpu_pct: f32,
    gpu_pct: Option<f32>,
    mem_used_pct: f32,
    at_ms: u64,
}
#[derive(Deserialize)]
struct TelemetryRaw {
    load1: f32,
    cpu_pct: f32,
    gpu_pct: Option<f32>,
    mem_used_pct: f32,
    at_ms: u64,
}
impl TryFrom<TelemetryRaw> for Telemetry {
    type Error = ProtoError;
    fn try_from(r: TelemetryRaw) -> Result<Self, ProtoError> {
        Self::new(r.load1, r.cpu_pct, r.gpu_pct, r.mem_used_pct, r.at_ms)
    }
}
impl Telemetry {
    pub fn new(
        load1: f32,
        cpu_pct: f32,
        gpu_pct: Option<f32>,
        mem_used_pct: f32,
        at_ms: u64,
    ) -> Result<Self, ProtoError> {
        let pct = |v: f32, field| {
            if (0.0..=100.0).contains(&v) {
                Ok(())
            } else {
                Err(ProtoError::OutOfRange { field })
            }
        };
        if !(load1.is_finite() && load1 >= 0.0) {
            return Err(ProtoError::OutOfRange { field: "load1" });
        }
        pct(cpu_pct, "cpu_pct")?;
        if let Some(g) = gpu_pct {
            pct(g, "gpu_pct")?;
        }
        pct(mem_used_pct, "mem_used_pct")?;
        Ok(Self {
            load1,
            cpu_pct,
            gpu_pct,
            mem_used_pct,
            at_ms,
        })
    }
    pub fn load1(&self) -> f32 {
        self.load1
    }
    pub fn cpu_pct(&self) -> f32 {
        self.cpu_pct
    }
    pub fn gpu_pct(&self) -> Option<f32> {
        self.gpu_pct
    }
    pub fn mem_used_pct(&self) -> f32 {
        self.mem_used_pct
    }
    pub fn at_ms(&self) -> u64 {
        self.at_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusyReason {
    Gaming { process: String },
    Load,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    Idle,
    Busy(BusyReason),
    Offline,
}
impl NodeState {
    pub fn accepts_spawn(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub hostname: String,
    pub state: NodeState,
    pub telemetry: Telemetry,
    pub agents: Vec<AgentId>,
}
impl NodeInfo {
    pub fn new(
        id: NodeId,
        hostname: String,
        state: NodeState,
        telemetry: Telemetry,
        agents: Vec<AgentId>,
    ) -> Self {
        Self {
            id,
            hostname,
            state,
            telemetry,
            agents,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    binary: String,
    args: Vec<String>,
    config_dir: Option<PathBuf>,
}
impl AgentProfile {
    pub fn new(
        binary: impl Into<String>,
        args: Vec<String>,
        config_dir: Option<PathBuf>,
    ) -> Result<Self, ProtoError> {
        let binary = binary.into();
        non_empty(&binary, "binary")?;
        Ok(Self {
            binary,
            args,
            config_dir,
        })
    }
    pub fn binary(&self) -> &str {
        &self.binary
    }
    pub fn args(&self) -> &[String] {
        &self.args
    }
    pub fn config_dir(&self) -> Option<&PathBuf> {
        self.config_dir.as_ref()
    }
    /// `claude` or `claude@/home/u/.claude-b`: binary file name plus config dir.
    pub fn key(&self) -> Result<ProfileKey, ProtoError> {
        let name = std::path::Path::new(&self.binary)
            .file_name()
            .map_or(self.binary.as_str(), |n| n.to_str().unwrap_or(&self.binary));
        match &self.config_dir {
            Some(d) => ProfileKey::new(&format!("{name}@{}", d.display())),
            None => ProfileKey::new(name),
        }
    }
}

/// Identifies an agent kind on a node (see `AgentProfile::key`). Printable, <= 200 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileKey(String);
impl ProfileKey {
    pub fn new(s: &str) -> Result<Self, ProtoError> {
        if s.is_empty() || s.len() > 200 || s.chars().any(char::is_control) {
            return Err(ProtoError::InvalidId {
                kind: "ProfileKey",
                value: s.chars().filter(|c| !c.is_control()).take(40).collect(),
            });
        }
        Ok(Self(s.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ProfileKey {
    type Error = ProtoError;
    fn try_from(s: String) -> Result<Self, ProtoError> {
        Self::new(&s)
    }
}
impl From<ProfileKey> for String {
    fn from(v: ProfileKey) -> String {
        v.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockerKind {
    Login,
    Install,
}

/// A permanent problem on a node that stops agents of one profile from starting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocker {
    id: BlockerId,
    node: NodeId,
    profile: ProfileKey,
    kind: BlockerKind,
    agent: Option<AgentId>,
    detail: String,
    at_ms: u64,
}
impl Blocker {
    /// `detail` loses control characters and is cut to 500 chars.
    pub fn new(
        id: BlockerId,
        node: NodeId,
        profile: ProfileKey,
        kind: BlockerKind,
        agent: Option<AgentId>,
        detail: String,
        at_ms: u64,
    ) -> Self {
        let detail = detail
            .chars()
            .filter(|c| !c.is_control())
            .take(500)
            .collect();
        Self {
            id,
            node,
            profile,
            kind,
            agent,
            detail,
            at_ms,
        }
    }
    pub fn id(&self) -> &BlockerId {
        &self.id
    }
    pub fn node(&self) -> &NodeId {
        &self.node
    }
    pub fn profile(&self) -> &ProfileKey {
        &self.profile
    }
    pub fn kind(&self) -> BlockerKind {
        self.kind
    }
    pub fn agent(&self) -> Option<&AgentId> {
        self.agent.as_ref()
    }
    pub fn detail(&self) -> &str {
        &self.detail
    }
    pub fn at_ms(&self) -> u64 {
        self.at_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    agent_id: AgentId,
    profile: AgentProfile,
    /// Folder spawns: where the agent runs. Repo spawns: empty, the daemon chooses.
    workdir: PathBuf,
    prompt: String,
    env: BTreeMap<String, String>,
    /// Absent (old peers, `new`) means `Source::Folder { path: workdir }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<Source>,
}
impl SpawnSpec {
    pub fn new(
        agent_id: AgentId,
        profile: AgentProfile,
        workdir: PathBuf,
        prompt: String,
        env: BTreeMap<String, String>,
    ) -> Result<Self, ProtoError> {
        non_empty(&prompt, "prompt")?;
        Ok(Self {
            agent_id,
            profile,
            workdir,
            prompt,
            env,
            source: None,
        })
    }
    /// Spawn from a GitHub repository; the daemon clones it into a workdir of its choosing.
    pub fn new_repo(
        agent_id: AgentId,
        profile: AgentProfile,
        source: Source,
        prompt: String,
        env: BTreeMap<String, String>,
    ) -> Result<Self, ProtoError> {
        let mut s = Self::new(agent_id, profile, PathBuf::new(), prompt, env)?;
        if let Source::Folder { path } = source {
            s.workdir = path;
        } else {
            s.source = Some(source);
        }
        Ok(s)
    }
    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }
    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }
    pub fn workdir(&self) -> &PathBuf {
        &self.workdir
    }
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
    /// Where the agent's files come from (a folder spec reports its workdir).
    pub fn source(&self) -> Source {
        self.source.clone().unwrap_or_else(|| Source::Folder {
            path: self.workdir.clone(),
        })
    }
    /// Same spec with another environment (the hub adds the agent token and MCP URL).
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }
    /// Frames from the wire are not re-validated on decode; the hub calls this on UI input.
    pub fn check(&self) -> Result<(), ProtoError> {
        non_empty(&self.prompt, "prompt")?;
        match &self.source {
            Some(Source::Folder { path }) if path != &self.workdir => Err(ProtoError::OutOfRange {
                field: "source.path",
            }),
            None if self.workdir.as_os_str().is_empty() => {
                Err(ProtoError::Empty { field: "workdir" })
            }
            _ => Ok(()),
        }
    }
}

/// `owner/name` of a GitHub repository (host is always github.com). Validated: owner
/// `[A-Za-z0-9-]{1,39}` without a leading `-`; name `[A-Za-z0-9._-]{1,100}` without a leading
/// `-`, never `.`/`..`, no `..` anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoRef(String);
impl RepoRef {
    pub fn new(s: &str) -> Result<Self, ProtoError> {
        let ok = s.split_once('/').is_some_and(|(o, n)| {
            let word = |x: &str, max: usize, extra: bool| {
                (1..=max).contains(&x.len())
                    && !x.starts_with('-')
                    && x.bytes().all(|b| {
                        b.is_ascii_alphanumeric()
                            || b == b'-'
                            || (extra && (b == b'.' || b == b'_'))
                    })
            };
            word(o, 39, false) && word(n, 100, true) && !n.contains("..") && n != "."
        });
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(ProtoError::InvalidId {
                kind: "RepoRef",
                value: s.chars().filter(|c| !c.is_control()).take(40).collect(),
            })
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn owner(&self) -> &str {
        self.0.split_once('/').map_or("", |x| x.0)
    }
    pub fn name(&self) -> &str {
        self.0.split_once('/').map_or("", |x| x.1)
    }
}
impl TryFrom<String> for RepoRef {
    type Error = ProtoError;
    fn try_from(s: String) -> Result<Self, ProtoError> {
        Self::new(&s)
    }
}
impl From<RepoRef> for String {
    fn from(v: RepoRef) -> String {
        v.0
    }
}

/// A git branch name that is safe to pass to `git`: 1..=200 printable chars, no leading `-` or
/// `/`, none of ` ~^:?*[\\`, no `..`, `@{`, `//`, and no trailing `/`, `.` or `.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BranchName(String);
impl BranchName {
    pub fn new(s: &str) -> Result<Self, ProtoError> {
        let bad = s.is_empty()
            || s.len() > 200
            || s.starts_with(['-', '/'])
            || s.ends_with(['/', '.'])
            || s.ends_with(".lock")
            || s.contains("..")
            || s.contains("@{")
            || s.contains("//")
            || s.chars()
                .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c));
        if bad {
            Err(ProtoError::InvalidId {
                kind: "BranchName",
                value: s.chars().filter(|c| !c.is_control()).take(40).collect(),
            })
        } else {
            Ok(Self(s.to_owned()))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for BranchName {
    type Error = ProtoError;
    fn try_from(s: String) -> Result<Self, ProtoError> {
        Self::new(&s)
    }
}
impl From<BranchName> for String {
    fn from(v: BranchName) -> String {
        v.0
    }
}

/// Where a spawned agent's files come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Folder {
        path: PathBuf,
    },
    Repo {
        repo: RepoRef,
        /// `None`: the repository's default branch.
        #[serde(default)]
        branch: Option<BranchName>,
        credential: CredentialId,
        /// May the agent push? The daemon enforces it (the token itself is not scoped).
        #[serde(default)]
        allow_push: bool,
        /// `true`: always clone anew. Default `false`: the daemon reuses a clone it already has
        /// of this repository when that clone is not used by another agent.
        #[serde(default, skip_serializing_if = "is_false")]
        fresh: bool,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One named agent profile of a node (from its `agents.json`), as the daemon reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    /// The name a spawn uses as `profile.binary` (the key of the `agents.json` entry).
    pub name: String,
    /// The executable the profile runs.
    pub binary: String,
    /// The profile's own config folder with `~` already expanded on the node, if it fixes one.
    pub config_dir: Option<String>,
    pub installed: bool,
    /// An unresolved login blocker exists for this profile on this node.
    pub login_blocked: bool,
    /// The profile comes from the hub (`HubToDaemon::Config`), not the node's `agents.json`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub from_hub: bool,
}

/// A profile defined on the hub. Never carries install or check commands: those stay in the
/// node's local `agents.json`. A hub profile whose name matches a local entry keeps that entry's
/// install/check/login/MCP fields and overrides `binary`, `args`, `config_dir` and `env`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubProfile {
    /// `[A-Za-z0-9._-]{1,64}`; what a spawn uses as `profile.binary`.
    pub name: String,
    /// Executable (`~` expanded on the node); 1-256 printable chars.
    pub binary: String,
    /// Argv placed before the spawn's own args (at most 32, each 1-512 printable chars).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Config folder (`~` expanded); for claude binaries it becomes `CLAUDE_CONFIG_DIR`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// Extra environment (at most 32; keys `[A-Za-z_][A-Za-z0-9_]*`, never `ULAK*`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// A hub profile and where it applies: `node: None` = every node; a node-scoped profile beats a
/// global one of the same name on that node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedProfile {
    pub node: Option<NodeId>,
    pub profile: HubProfile,
}

/// Node settings stored on the hub. `None` = the node's local file (`ulakd.json`, `update.json`,
/// `guard.json`) or its built-in default. Applied live by the daemon.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeSettings {
    /// Folders indexed for existing clones (`~` expanded); at most 16.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_roots: Option<Vec<String>>,
    /// Minutes between repo index refreshes, 1-1440.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_interval_mins: Option<u64>,
    /// Delete clean, fully pushed work-root clones automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clean_auto: Option<bool>,
    /// Hours after the agent exits before a clone may be cleaned, 1-8760.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clean_after_exit_hours: Option<u64>,
    /// Install updates automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    /// Node counts as busy above this CPU %, 1-100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_busy_pct: Option<f32>,
    /// Node counts as busy above this GPU %, 1-100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_busy_pct: Option<f32>,
}

/// One node's hub-stored settings (an entry of `HubToUi::HubConfig`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSettingsEntry {
    pub node: NodeId,
    pub settings: NodeSettings,
}

/// A git clone present on a node (see the node's repo index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    pub full_name: RepoRef,
    pub path: String,
    /// `None`: detached HEAD.
    pub branch: Option<String>,
    /// No modified, staged or untracked (non-ignored) files.
    pub clean: bool,
    /// Commits vs the remote tracking branch as of the last fetch; `None`: no upstream.
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    /// Time of the last fetch (clone time when never fetched); `None`: unknown.
    pub last_fetch_ms: Option<u64>,
    /// An agent currently runs in (or is starting in) this clone.
    pub in_use: bool,
    /// Lives under the daemon's work root, so `CleanRepo` may delete it.
    pub managed: bool,
}

/// One line of a clean report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanItem {
    pub path: String,
    pub full_name: Option<RepoRef>,
    pub reason: String,
}

/// A GitHub token. `Debug` never prints it; it is serialised as a plain string, but only the
/// hub's UI socket (add) and the hub-to-daemon socket (`GitAccess`) ever carry it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GitToken(String);
impl GitToken {
    pub fn new(s: &str) -> Result<Self, ProtoError> {
        if (8..=255).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            Ok(Self(s.to_owned()))
        } else {
            // Never echo the value.
            Err(ProtoError::OutOfRange { field: "token" })
        }
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for GitToken {
    type Error = ProtoError;
    fn try_from(s: String) -> Result<Self, ProtoError> {
        Self::new(&s)
    }
}
impl From<GitToken> for String {
    fn from(v: GitToken) -> String {
        v.0
    }
}
impl std::fmt::Debug for GitToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitToken(***)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Github,
}

/// A stored GitHub credential as shown to UIs. Never contains the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialInfo {
    pub id: CredentialId,
    pub name: String,
    pub kind: CredentialKind,
    pub login: String,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub full_name: RepoRef,
    pub private: bool,
    pub default_branch: String,
    /// 0 when GitHub reports none (empty repository).
    pub pushed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueState {
    /// Waiting for an eligible node.
    Queued,
    /// Placement failed with this message; stays listed until cancelled.
    Failed(String),
}

/// A spawn request waiting for a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedSpawn {
    pub id: QueueId,
    pub agent: AgentId,
    pub source: Source,
    pub target: SpawnTarget,
    pub profile: ProfileKey,
    /// First line of the prompt, at most 120 chars.
    pub prompt_preview: String,
    pub created_ms: u64,
    pub state: QueueState,
}
impl QueuedSpawn {
    pub fn preview(prompt: &str) -> String {
        prompt
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .chars()
            .take(120)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpawnTarget {
    Auto,
    Node(NodeId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Starting,
    Running,
    Exited(Option<i32>),
    Failed,
    /// ulakd is installing the agent binary.
    Installing,
    /// The agent asked the user to log in.
    NeedsLogin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageStatus {
    Pending,
    Delivered,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    id: MessageId,
    from: AgentId,
    to: AgentId,
    thread: ThreadId,
    payload: String,
    pub status: MessageStatus,
    at_ms: u64,
}
impl Message {
    pub fn new(
        id: MessageId,
        from: AgentId,
        to: AgentId,
        thread: ThreadId,
        payload: String,
        at_ms: u64,
    ) -> Result<Self, ProtoError> {
        non_empty(&payload, "payload")?;
        Ok(Self {
            id,
            from,
            to,
            thread,
            payload,
            status: MessageStatus::Pending,
            at_ms,
        })
    }
    pub fn id(&self) -> &MessageId {
        &self.id
    }
    pub fn from(&self) -> &AgentId {
        &self.from
    }
    pub fn to(&self) -> &AgentId {
        &self.to
    }
    pub fn thread(&self) -> &ThreadId {
        &self.thread
    }
    pub fn payload(&self) -> &str {
        &self.payload
    }
    pub fn at_ms(&self) -> u64 {
        self.at_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    id: QuestionId,
    from: AgentId,
    text: String,
    at_ms: u64,
}
impl Question {
    pub fn new(
        id: QuestionId,
        from: AgentId,
        text: String,
        at_ms: u64,
    ) -> Result<Self, ProtoError> {
        non_empty(&text, "question")?;
        Ok(Self {
            id,
            from,
            text,
            at_ms,
        })
    }
    pub fn id(&self) -> &QuestionId {
        &self.id
    }
    pub fn from(&self) -> &AgentId {
        &self.from
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn at_ms(&self) -> u64 {
        self.at_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    question: QuestionId,
    text: String,
}
impl Answer {
    pub fn new(question: QuestionId, text: String) -> Result<Self, ProtoError> {
        non_empty(&text, "answer")?;
        Ok(Self { question, text })
    }
    pub fn question(&self) -> &QuestionId {
        &self.question
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollRequest {
    otp: Otp,
    public_key: String,
    hostname: String,
}
impl EnrollRequest {
    pub fn new(otp: Otp, public_key: String, hostname: String) -> Result<Self, ProtoError> {
        non_empty(&public_key, "public_key")?;
        non_empty(&hostname, "hostname")?;
        Ok(Self {
            otp,
            public_key,
            hostname,
        })
    }
    pub fn otp(&self) -> &Otp {
        &self.otp
    }
    pub fn public_key(&self) -> &str {
        &self.public_key
    }
    pub fn hostname(&self) -> &str {
        &self.hostname
    }
}

/// PEM strings: the signed client certificate and the hub CA certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollResponse {
    pub cert: String,
    pub ca_cert: String,
}
impl EnrollResponse {
    pub fn new(cert: String, ca_cert: String) -> Self {
        Self { cert, ca_cert }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Node,
    Ui,
    Browser,
}

/// An enrolled device as the UI lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: NodeId,
    pub kind: DeviceKind,
    pub hostname: String,
    pub created_ms: u64,
    pub revoked: bool,
}
