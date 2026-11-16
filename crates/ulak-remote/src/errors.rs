use ulak_proto::{AgentId, BlockerId, MessageId, NodeId, ProtoError, QuestionId};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MailboxError {
    #[error("message {0:?} not found")]
    NotFound(MessageId),
    #[error(transparent)]
    Invalid(#[from] ProtoError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QuestionError {
    #[error("question {0:?} not found")]
    NotFound(QuestionId),
    #[error("question {0:?} was already answered")]
    AlreadyAnswered(QuestionId),
    #[error(transparent)]
    Invalid(#[from] ProtoError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockerError {
    #[error("blocker {0:?} not found or already resolved")]
    NotFound(BlockerId),
    #[error(transparent)]
    Invalid(#[from] ProtoError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScheduleError {
    #[error("no node is idle and under the load thresholds")]
    NoEligibleNode,
    #[error("unknown node {0:?}")]
    UnknownNode(NodeId),
    #[error("node {0:?} is busy, offline or over a threshold")]
    NodeNotEligible(NodeId),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeviceError {
    #[error("device {0:?} not found")]
    NotFound(NodeId),
    #[error(transparent)]
    Invalid(#[from] ProtoError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EnrollError {
    /// Wrong, unknown, used, expired or wrong-kind code. One error so nothing leaks.
    #[error("invalid one-time code")]
    BadOtp,
    #[error("too many wrong codes; create a new one in the UI")]
    Locked,
    #[error("public key is not a valid PEM public key")]
    InvalidKey,
    #[error("certificate signing failed: {0}")]
    Ca(#[from] rcgen::Error),
    #[error(transparent)]
    Device(#[from] DeviceError),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    #[error("invalid or expired token")]
    Invalid,
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error("stored credential is unreadable: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    #[error(transparent)]
    Schedule(#[from] ScheduleError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Question(#[from] QuestionError),
    #[error(transparent)]
    Mailbox(#[from] MailboxError),
    #[error(transparent)]
    Blocker(#[from] BlockerError),
    #[error("{0}")]
    Blocked(String),
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error("node {0:?} is not connected")]
    NodeOffline(NodeId),
    #[error("no connected node runs agent {0:?}")]
    AgentUnplaced(AgentId),
    #[error("node {node} speaks protocol {got}, the hub needs at least {min}")]
    DaemonTooOld { node: String, got: u32, min: u32 },
    #[error("hello names a different node than the client certificate")]
    SpoofedHello,
    #[error("bad arguments: {0}")]
    BadArgs(String),
    #[error("tls setup: {0}")]
    Tls(#[from] tokio_rustls::rustls::Error),
    #[error("certificate: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("tls material: {0}")]
    Pem(String),
}
