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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToDaemon {
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
