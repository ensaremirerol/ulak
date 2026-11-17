//! ulak hub. Part 2a: store and scheduling. Part 2b: CA, enrollment, device and agent auth.
#[cfg(unix)]
pub mod admin;
mod ca;
mod devices;
mod enroll;
mod errors;
mod passkeys;
mod proxy;
mod registry;
mod scheduler;
mod server;
mod store;
mod tokens;

pub use ca::Ca;
pub use devices::{Device, DeviceKind, Devices};
pub use enroll::{Enrolled, Enroller, OtpOffer};
pub use errors::*;
pub use passkeys::{PasskeyInfo, Passkeys, Sessions};
pub use proxy::TrustedProxies;
pub use registry::Registry;
pub use scheduler::{pick_node, pick_node_where, Thresholds};
pub use server::{Hub, HubConfig, Running};
pub use store::{Blockers, Mailbox, Questions, Store};
pub use tokens::AgentTokens;

/// Version of this build: the package version plus the git sha embedded by build.rs.
pub fn version_info() -> ulak_proto::VersionInfo {
    ulak_proto::VersionInfo::current(env!("CARGO_PKG_VERSION"), env!("ULAK_GIT_SHA"))
        .expect("package version and git sha are short printable strings")
}
