use ulak_proto::ProtoError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TelemetryError {
    #[error("cannot parse {what}: {detail}")]
    Parse { what: &'static str, detail: String },
    #[error("io error reading {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Proto(#[from] ProtoError),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    #[error("text contains control characters")]
    ControlChars,
    #[error("invalid environment variable name {0:?}")]
    BadEnvName(String),
    #[error("tmux {action} failed: {stderr}")]
    Tmux {
        action: &'static str,
        stderr: String,
    },
    #[error("agent {0} is not running")]
    NotRunning(String),
    #[error("agent {0} already exists")]
    AlreadyExists(String),
    #[error("repository setup failed: {0}")]
    Git(String),
    #[error("agent is not installed: {0}")]
    Install(String),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("not enrolled: {0} is missing (run `ulakd join <hub-url>`)")]
    NotEnrolled(String),
    #[error("invalid guard config: {0}")]
    BadGuard(String),
    #[error("certificate has no usable common name")]
    BadCert,
    #[error("invalid hub url {0:?}")]
    BadUrl(String),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HubError {
    #[error("websocket: {0}")]
    Ws(Box<tokio_tungstenite::tungstenite::Error>),
    #[error("tls: {0}")]
    Tls(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Telemetry(#[from] TelemetryError),
}

impl From<tokio_tungstenite::tungstenite::Error> for HubError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Ws(Box::new(e))
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JoinError {
    #[error("hub presented no certificate matching fingerprint {0}")]
    FingerprintMismatch(String),
    #[error("hub returned a CA whose fingerprint differs from the confirmed one")]
    CaMismatch,
    #[error("hub rejected enrollment (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },
    #[error("an identity already exists in {dir}: hub {hub}, device {device}; pass --force or set {env}=1 to replace it")]
    AlreadyEnrolled {
        dir: String,
        hub: String,
        device: String,
        env: &'static str,
    },
    #[error("malformed HTTP response: {0}")]
    BadResponse(&'static str),
    #[error("tls: {0}")]
    Tls(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    KeyGen(#[from] rcgen::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    #[error("no release signing key configured; updates are disabled (run scripts/gen-release-key.sh, commit crates/ulakd/release-key.pub and rebuild)")]
    NoKey,
    #[error("bad update config: {0}")]
    Config(String),
    #[error("update server: {0}")]
    Http(String),
    /// Hub update source: the text is already the user-facing message.
    #[error("{0}")]
    Hub(String),
    #[error("bad release data: {0}")]
    Release(String),
    #[error("the release has no asset named {0}")]
    NoAsset(String),
    #[error("download larger than the {0} byte limit")]
    TooLarge(u64),
    #[error("download truncated: got {got} of {want} bytes")]
    Truncated { got: u64, want: u64 },
    #[error("signature check failed for {0}")]
    BadSignature(String),
    #[error("checksum mismatch for {0}")]
    BadChecksum(String),
    #[error("unsafe or unexpected archive: {0}")]
    Archive(String),
    #[error(
        "cannot write to {0}: the daemon user needs write access to the directory of its binary"
    )]
    NotWritable(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
