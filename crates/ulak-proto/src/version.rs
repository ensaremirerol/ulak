use crate::ProtoError;
use serde::{Deserialize, Serialize};

/// Wire protocol revision of this build. Bump on any non-additive wire change.
pub const PROTO_VERSION: u32 = 1;
/// The hub refuses a daemon that reports an older `proto` than this.
pub const MIN_DAEMON_PROTO: u32 = 1;

const MAX_FIELD: usize = 64;

fn field(name: &'static str, s: &str) -> Result<String, ProtoError> {
    if s.is_empty() {
        return Err(ProtoError::Empty { field: name });
    }
    if s.len() > MAX_FIELD || s.chars().any(char::is_control) {
        return Err(ProtoError::OutOfRange { field: name });
    }
    Ok(s.to_owned())
}

#[derive(Deserialize)]
struct Raw {
    version: String,
    git: String,
    proto: u32,
    os: String,
    arch: String,
}

/// What a binary is: every text field is 1..=64 bytes without control characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Raw")]
pub struct VersionInfo {
    version: String,
    git: String,
    proto: u32,
    os: String,
    arch: String,
}

impl TryFrom<Raw> for VersionInfo {
    type Error = ProtoError;
    fn try_from(r: Raw) -> Result<Self, ProtoError> {
        Self::new(&r.version, &r.git, r.proto, &r.os, &r.arch)
    }
}

impl VersionInfo {
    pub fn new(
        version: &str,
        git: &str,
        proto: u32,
        os: &str,
        arch: &str,
    ) -> Result<Self, ProtoError> {
        Ok(Self {
            version: field("version", version)?,
            git: field("git", git)?,
            proto,
            os: field("os", os)?,
            arch: field("arch", arch)?,
        })
    }
    /// This build: `PROTO_VERSION` and the compile-time OS and architecture.
    pub fn current(version: &str, git: &str) -> Result<Self, ProtoError> {
        Self::new(
            version,
            git,
            PROTO_VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn git(&self) -> &str {
        &self.git
    }
    pub fn proto(&self) -> u32 {
        self.proto
    }
    pub fn os(&self) -> &str {
        &self.os
    }
    pub fn arch(&self) -> &str {
        &self.arch
    }
}

/// Where a daemon is in its self-update. JSON: `{"state":"available","version":"0.2.0"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    UpToDate { checked_ms: u64 },
    Available { version: String },
    Checking,
    Downloading,
    Verifying,
    Installing,
    Restarting,
    Failed { message: String },
}

impl UpdateState {
    /// Control characters stripped, at most 500 chars.
    pub fn failed(message: &str) -> Self {
        Self::Failed {
            message: message
                .chars()
                .filter(|c| !c.is_control())
                .take(500)
                .collect(),
        }
    }
}
