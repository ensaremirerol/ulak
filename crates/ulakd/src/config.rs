//! The enrollment directory: `cert.pem`, `key.pem` (0600), `ca.pem`, `hub_url`, optional `guard.json`.
use crate::{errors::ConfigError, guard::GuardConfig};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use ulak_proto::{EnrollResponse, NodeId};

/// `$ULAKD_DIR`, else `$XDG_CONFIG_HOME/ulakd`, else `~/.config/ulakd`.
pub fn default_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ULAKD_DIR") {
        return d.into();
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("ulakd");
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".config/ulakd")
}

/// `https://host:port[/]` -> `wss://host:port/ws/daemon` (`http` -> `ws`).
pub fn daemon_ws_url(hub: &str) -> Result<String, ConfigError> {
    let (scheme, rest) = hub
        .split_once("://")
        .ok_or_else(|| ConfigError::BadUrl(hub.to_owned()))?;
    let ws = match scheme {
        "https" => "wss",
        "http" => "ws",
        _ => return Err(ConfigError::BadUrl(hub.to_owned())),
    };
    let authority = rest.trim_end_matches('/');
    if authority.is_empty() || authority.contains('/') {
        return Err(ConfigError::BadUrl(hub.to_owned()));
    }
    Ok(format!("{ws}://{authority}/ws/daemon"))
}

/// Writes the enrollment result. The private key is created with mode 0600.
pub fn save(
    dir: &Path,
    hub_url: &str,
    key_pem: &str,
    resp: &EnrollResponse,
) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("cert.pem"), &resp.cert)?;
    std::fs::write(dir.join("ca.pem"), &resp.ca_cert)?;
    std::fs::write(dir.join("hub_url"), hub_url)?;
    let key = dir.join("key.pem");
    std::fs::remove_file(&key).or_else(|e| {
        (e.kind() == std::io::ErrorKind::NotFound)
            .then_some(())
            .ok_or(e)
    })?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    // 0600 on Unix. Windows has no mode bits: the key inherits the ACL of the per-user config dir.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(key)?.write_all(key_pem.as_bytes())?;
    Ok(())
}

/// Everything a running daemon needs from the enrollment directory.
#[derive(Debug, Clone)]
pub struct Identity {
    pub hub_url: String,
    pub node_id: NodeId,
    pub ca_pem: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub guard: GuardConfig,
}

fn read(dir: &Path, name: &str) -> Result<String, ConfigError> {
    std::fs::read_to_string(dir.join(name)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            ConfigError::NotEnrolled(dir.join(name).display().to_string())
        }
        _ => e.into(),
    })
}

/// The node id is the client certificate's common name.
pub fn node_id_from_cert(cert_pem: &str) -> Result<NodeId, ConfigError> {
    let (_, pem) =
        x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).map_err(|_| ConfigError::BadCert)?;
    let cert = pem.parse_x509().map_err(|_| ConfigError::BadCert)?;
    let cn = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|c| c.as_str().ok())
        .ok_or(ConfigError::BadCert)?;
    Ok(NodeId::new(cn)?)
}

impl Identity {
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let cert_pem = read(dir, "cert.pem")?;
        let guard = match std::fs::read_to_string(dir.join("guard.json")) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => GuardConfig::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            hub_url: read(dir, "hub_url")?.trim().to_owned(),
            node_id: node_id_from_cert(&cert_pem)?,
            ca_pem: read(dir, "ca.pem")?,
            key_pem: read(dir, "key.pem")?,
            cert_pem,
            guard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ws_url() {
        assert_eq!(
            daemon_ws_url("https://hub.lan:8443/").unwrap(),
            "wss://hub.lan:8443/ws/daemon"
        );
        assert_eq!(
            daemon_ws_url("http://127.0.0.1:1").unwrap(),
            "ws://127.0.0.1:1/ws/daemon"
        );
        assert!(daemon_ws_url("ftp://x").is_err());
        assert!(daemon_ws_url("hub.lan").is_err());
        assert!(daemon_ws_url("https://hub/extra").is_err());
    }

    #[test]
    fn save_then_load_roundtrip_with_private_key() {
        let dir = std::env::temp_dir().join(format!("ulakd-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(
            Identity::load(&dir),
            Err(ConfigError::NotEnrolled(_))
        ));
        let key = rcgen::KeyPair::generate().unwrap();
        let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        p.distinguished_name
            .push(rcgen::DnType::CommonName, "node-7");
        let cert = p.self_signed(&key).unwrap().pem();
        let resp = EnrollResponse::new(cert.clone(), "CA".into());
        save(&dir, "https://hub:1", &key.serialize_pem(), &resp).unwrap();
        save(&dir, "https://hub:1", &key.serialize_pem(), &resp).unwrap(); // re-enroll overwrites
        let mode = std::fs::metadata(dir.join("key.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let id = Identity::load(&dir).unwrap();
        assert_eq!(id.node_id.as_str(), "node-7");
        assert_eq!(id.hub_url, "https://hub:1");
        std::fs::write(dir.join("guard.json"), r#"{"gpu_threshold":10}"#).unwrap();
        assert!(Identity::load(&dir).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
