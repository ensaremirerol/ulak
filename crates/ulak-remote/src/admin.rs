//! Local admin interface: a Unix socket `$ULAK_DIR/admin.sock` (0600) on the running hub.
//! One JSON request line in, one JSON reply line out. Only local processes that may open the
//! socket can use it (in Docker: `docker exec`). Admin calls are not enrollment attempts, so
//! they never touch the enrollment lockout or rate limit.
use crate::{Credentials, DeviceKind, Devices, Hub, PasskeyInfo, Passkeys};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    task::JoinHandle,
};
use ulak_proto::{CredentialId, CredentialInfo, DeviceInfo, NodeId};

pub const VERSION: u32 = 1;
/// Longest accepted request line (and reply line read by the client is not capped).
pub const MAX_LINE: usize = 4096;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
pub const SOCKET_NAME: &str = "admin.sock";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AdminRequest {
    Otp { kind: DeviceKind },
    Devices,
    Revoke { id: String },
    Passkeys,
    RevokePasskey { id: String },
    Status,
    Fingerprint,
    Credentials,
    CredentialDelete { id: String },
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "res", rename_all = "snake_case")]
pub enum AdminResponse {
    Otp {
        code: String,
        expires_in_s: u64,
        fingerprint: String,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    Passkeys {
        passkeys: Vec<PasskeyInfo>,
    },
    Credentials {
        credentials: Vec<CredentialInfo>,
    },
    Done,
    Fingerprint {
        fingerprint: String,
        ca_pem: String,
    },
    Status {
        version: String,
        fingerprint: String,
        listen: Vec<String>,
        devices: usize,
        passkeys: usize,
        nodes_online: usize,
    },
    Error {
        message: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    v: u32,
    #[serde(flatten)]
    body: T,
}

/// The line the server prints at startup and the CLI prints for a new code.
pub fn otp_line(code: &str, fingerprint: &str) -> String {
    format!("OTP {code} (hub CA sha256 {fingerprint})")
}

fn err(m: impl std::fmt::Display) -> AdminResponse {
    AdminResponse::Error {
        message: m.to_string(),
    }
}

fn now_ms() -> u64 {
    crate::server::now_ms()
}

fn handle(hub: &Hub, listen: &[String], line: &[u8]) -> AdminResponse {
    let req: Envelope<AdminRequest> = match serde_json::from_slice(line) {
        Ok(r) => r,
        Err(e) => return err(format!("bad request: {e}")),
    };
    if req.v != VERSION {
        return err(format!("unsupported protocol version {}", req.v));
    }
    let id = |s: &str| NodeId::new(s.trim()).map_err(|e| err(format!("bad id: {e}")));
    match req.body {
        AdminRequest::Otp { kind } => {
            let now = now_ms();
            let o = hub.enroller().begin(kind, now);
            AdminResponse::Otp {
                code: o.otp.expose().to_owned(),
                expires_in_s: o.expires_ms.saturating_sub(now) / 1000,
                fingerprint: o.hub_fingerprint,
            }
        }
        AdminRequest::Devices => match Devices::list(&**hub.store()) {
            Ok(d) => AdminResponse::Devices {
                devices: d.iter().map(DeviceInfo::from).collect(),
            },
            Err(e) => err(e),
        },
        AdminRequest::Revoke { id: i } => match id(&i) {
            Ok(i) => hub
                .revoke_device(&i, now_ms())
                .map_or_else(err, |()| AdminResponse::Done),
            Err(e) => e,
        },
        AdminRequest::Passkeys => match hub.store().list_passkeys() {
            Ok(passkeys) => AdminResponse::Passkeys { passkeys },
            Err(e) => err(e),
        },
        AdminRequest::RevokePasskey { id: i } => match id(&i) {
            Ok(i) => hub
                .revoke_passkey(&i, now_ms())
                .map_or_else(err, |()| AdminResponse::Done),
            Err(e) => e,
        },
        AdminRequest::Credentials => match Credentials::list_credentials(&**hub.store()) {
            Ok(credentials) => AdminResponse::Credentials { credentials },
            Err(e) => err(e),
        },
        AdminRequest::CredentialDelete { id: i } => match CredentialId::new(i.trim()) {
            Ok(i) => Credentials::delete_credential(&**hub.store(), &i)
                .map_or_else(err, |()| AdminResponse::Done),
            Err(e) => err(format!("bad id: {e}")),
        },
        AdminRequest::Fingerprint => AdminResponse::Fingerprint {
            fingerprint: hub.enroller().ca().fingerprint().to_owned(),
            ca_pem: hub.enroller().ca().cert_pem().to_owned(),
        },
        AdminRequest::Status => {
            let active = |r: bool| usize::from(!r);
            let devices = Devices::list(&**hub.store())
                .map(|d| d.iter().map(|d| active(d.revoked_ms.is_some())).sum())
                .unwrap_or(0);
            let passkeys = hub
                .store()
                .list_passkeys()
                .map(|p| p.iter().map(|p| active(p.revoked_ms.is_some())).sum())
                .unwrap_or(0);
            AdminResponse::Status {
                version: crate::version_info().version().to_owned(),
                fingerprint: hub.enroller().ca().fingerprint().to_owned(),
                listen: listen.to_vec(),
                devices,
                passkeys,
                nodes_online: hub.registry().snapshot().len(),
            }
        }
    }
}

async fn serve_conn(hub: Arc<Hub>, listen: Arc<Vec<String>>, mut s: tokio::net::UnixStream) {
    let mut line = Vec::new();
    let read = async {
        let mut r = BufReader::new((&mut s).take(MAX_LINE as u64 + 1));
        r.read_until(b'\n', &mut line).await
    };
    let reply = match tokio::time::timeout(READ_TIMEOUT, read).await {
        Err(_) => err("read timeout"),
        Ok(Err(e)) => err(e),
        Ok(Ok(_)) if line.len() > MAX_LINE => err("request too long"),
        Ok(Ok(_)) => handle(&hub, &listen, &line),
    };
    let mut out = serde_json::to_vec(&Envelope {
        v: VERSION,
        body: reply,
    })
    .expect("reply serializes");
    out.push(b'\n');
    let _ = s.write_all(&out).await;
}

/// The running admin socket. `close` stops it and removes the file; a crash leaves a stale
/// file, which the next start replaces.
pub struct AdminSocket {
    task: JoinHandle<()>,
    path: PathBuf,
}

impl AdminSocket {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn close(self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Hub {
    /// Starts the admin socket at `path`. `listen` is what `Status` reports as listen addresses.
    pub async fn serve_admin(
        self: &Arc<Self>,
        path: &Path,
        listen: Vec<String>,
    ) -> std::io::Result<AdminSocket> {
        let bad = |m: String| std::io::Error::other(m);
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if std::fs::metadata(dir)?.permissions().mode() & 0o002 != 0 {
            return Err(bad(format!("{} is world-writable", dir.display())));
        }
        match std::fs::symlink_metadata(path) {
            Ok(m) if !m.file_type().is_socket() => {
                return Err(bad(format!(
                    "{} exists and is not a socket",
                    path.display()
                )))
            }
            Ok(_) => {
                if UnixStream::connect(path).is_ok() {
                    return Err(bad(format!("{} is in use by another hub", path.display())));
                }
                std::fs::remove_file(path)?; // stale from a crash
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(path)?;
        // ponytail: bound under the umask first, then chmod; the window is covered by the data
        // dir being private. Bind in a 0700 temp dir and rename if that ever matters.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let (hub, listen) = (self.clone(), Arc::new(listen));
        let task = tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                tokio::spawn(serve_conn(hub.clone(), listen.clone(), s));
            }
        });
        Ok(AdminSocket {
            task,
            path: path.to_owned(),
        })
    }
}

/// Client side (blocking): one request, one reply.
pub fn call(path: &Path, req: AdminRequest) -> Result<AdminResponse, String> {
    let mut s = UnixStream::connect(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => format!(
            "the hub is not running (no admin socket at {}). Is ULAK_DIR right? Use --dir.",
            path.display()
        ),
        std::io::ErrorKind::PermissionDenied => format!(
            "not permitted to use {} (it is 0600; run as the hub's user, or `docker exec`)",
            path.display()
        ),
        _ => format!("cannot connect to {}: {e}", path.display()),
    })?;
    for r in [
        s.set_read_timeout(Some(Duration::from_secs(10))),
        s.set_write_timeout(Some(Duration::from_secs(10))),
    ] {
        r.map_err(|e| e.to_string())?;
    }
    let mut line = serde_json::to_vec(&Envelope {
        v: VERSION,
        body: req,
    })
    .map_err(|e| e.to_string())?;
    line.push(b'\n');
    s.write_all(&line).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    std::io::BufReader::new(s.take(1 << 20))
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    let r: Envelope<AdminResponse> =
        serde_json::from_str(&reply).map_err(|e| format!("bad reply from hub: {e}"))?;
    match r.body {
        AdminResponse::Error { message } => Err(message),
        b => Ok(b),
    }
}

fn kind_name(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Node => "node",
        DeviceKind::Ui => "ui",
        DeviceKind::Browser => "browser",
    }
}

fn ts(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    time::OffsetDateTime::from_unix_timestamp(secs)
        .map_or_else(|_| ms.to_string(), |t| t.date().to_string())
}

/// Human-readable text for a reply. `join_host` is the first SAN of ULAK_SANS, if known.
pub fn render(r: &AdminResponse, join_host: Option<&str>) -> String {
    match r {
        AdminResponse::Otp {
            code,
            expires_in_s,
            fingerprint,
        } => {
            let mut s = format!(
                "{}\nexpires in {expires_in_s} s",
                otp_line(code, fingerprint)
            );
            if let Some(h) = join_host {
                s += &format!("\nulak join https://{h}");
            }
            s
        }
        AdminResponse::Devices { devices } => devices
            .iter()
            .map(|d| {
                format!(
                    "{}\t{}\t{}\t{}{}",
                    d.id.as_str(),
                    kind_name(d.kind),
                    d.hostname,
                    ts(d.created_ms),
                    if d.revoked { "\trevoked" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        AdminResponse::Passkeys { passkeys } => passkeys
            .iter()
            .map(|p| {
                format!(
                    "{}\t{}\t{}{}",
                    p.id.as_str(),
                    p.name,
                    ts(p.created_ms),
                    if p.revoked_ms.is_some() { "\trevoked" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        AdminResponse::Credentials { credentials } => credentials
            .iter()
            .map(|c| format!("{}\t{}\t{}", c.id.as_str(), c.name, c.login))
            .collect::<Vec<_>>()
            .join("\n"),
        AdminResponse::Done => "done".into(),
        AdminResponse::Fingerprint { fingerprint, .. } => fingerprint.clone(),
        AdminResponse::Status {
            version,
            fingerprint,
            listen,
            devices,
            passkeys,
            nodes_online,
        } => format!(
            "ulak-remote {version}\nCA sha256 {fingerprint}\nlistening: {}\nactive devices: {devices}, passkeys: {passkeys}, nodes online: {nodes_online}",
            listen.join(", ")
        ),
        AdminResponse::Error { message } => message.clone(),
    }
}

/// `ulak-remote admin <cmd> ...` (args after "admin"). Returns the text to print.
pub fn cli(
    args: &[String],
    env_dir: Option<String>,
    sans: Option<String>,
) -> Result<String, String> {
    let mut dir = env_dir.unwrap_or_else(|| "ulak-data".into());
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--dir" {
            dir = it.next().ok_or("--dir needs a path")?.clone();
        } else {
            rest.push(a.as_str());
        }
    }
    let usage = "usage: ulak-remote admin [--dir D] otp device|browser (ui, node: same as device) | devices | revoke <id> | passkeys | revoke-passkey <id> | credentials | credential-delete <id> | status | fingerprint | ca [--pem]";
    let req = match rest.as_slice() {
        ["otp", k] => AdminRequest::Otp {
            kind: match *k {
                "device" | "node" => DeviceKind::Node,
                "ui" => DeviceKind::Ui,
                "browser" => DeviceKind::Browser,
                _ => return Err(usage.into()),
            },
        },
        ["devices"] => AdminRequest::Devices,
        ["revoke", id] => AdminRequest::Revoke { id: (*id).into() },
        ["passkeys"] => AdminRequest::Passkeys,
        ["revoke-passkey", id] => AdminRequest::RevokePasskey { id: (*id).into() },
        ["credentials"] => AdminRequest::Credentials,
        ["credential-delete", id] => AdminRequest::CredentialDelete { id: (*id).into() },
        ["status"] => AdminRequest::Status,
        ["fingerprint"] => AdminRequest::Fingerprint,
        ["ca"] | ["ca", "--pem"] => {
            if rest.len() == 1 {
                return Ok(Path::new(&dir).join("ca.pem").display().to_string());
            }
            return match call(
                &Path::new(&dir).join(SOCKET_NAME),
                AdminRequest::Fingerprint,
            )? {
                AdminResponse::Fingerprint { ca_pem, .. } => Ok(ca_pem.trim_end().to_owned()),
                _ => Err("unexpected reply".into()),
            };
        }
        _ => return Err(usage.into()),
    };
    let host = sans
        .as_deref()
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let r = call(&Path::new(&dir).join(SOCKET_NAME), req)?;
    let mut out = render(&r, host);
    if matches!(rest.as_slice(), ["otp", "device"]) {
        out += "\nkind: device (works for `ulakd join` and `ulak join`)";
    }
    Ok(out)
}
