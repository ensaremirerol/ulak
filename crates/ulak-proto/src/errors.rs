#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProtoError {
    #[error("invalid {kind}: {value:?} (allowed: [a-z0-9-], 1..=64 chars)")]
    InvalidId { kind: &'static str, value: String },
    #[error("invalid one-time code (expected 8 chars of A-Z0-9)")]
    InvalidOtp,
    #[error("{field} is out of range")]
    OutOfRange { field: &'static str },
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error(transparent)]
    Codec(#[from] serde_json::Error),
}
