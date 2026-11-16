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
    workdir: PathBuf,
    prompt: String,
    env: BTreeMap<String, String>,
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
        })
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
