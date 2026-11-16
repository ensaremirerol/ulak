use crate::{DeviceError, Store};
use rusqlite::{params, OptionalExtension};
use ulak_proto::NodeId;

pub use ulak_proto::DeviceKind;

fn code(k: DeviceKind) -> i64 {
    k as i64
}
fn from_code(c: i64) -> DeviceKind {
    match c {
        0 => DeviceKind::Node,
        1 => DeviceKind::Ui,
        _ => DeviceKind::Browser,
    }
}

impl From<&Device> for ulak_proto::DeviceInfo {
    fn from(d: &Device) -> Self {
        Self {
            id: d.id.clone(),
            kind: d.kind,
            hostname: d.hostname.clone(),
            created_ms: d.created_ms,
            revoked: d.revoked_ms.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: NodeId,
    pub kind: DeviceKind,
    pub hostname: String,
    pub created_ms: u64,
    pub revoked_ms: Option<u64>,
    pub renewed_ms: Option<u64>,
}

pub trait Devices {
    /// Registers a device. The id is the hostname as a slug, made unique with `-2`, `-3`, ...
    fn add(
        &self,
        hostname: &str,
        kind: DeviceKind,
        public_key: &str,
        now_ms: u64,
    ) -> Result<NodeId, DeviceError>;
    fn list(&self) -> Result<Vec<Device>, DeviceError>;
    fn revoke(&self, id: &NodeId, now_ms: u64) -> Result<(), DeviceError>;
    /// True only for a known, non-revoked device. Checked on every mTLS connection.
    fn is_active(&self, id: &NodeId) -> Result<bool, DeviceError>;
    /// Records a certificate renewal and the device's (possibly new) public key.
    fn mark_renewed(&self, id: &NodeId, public_key: &str, now_ms: u64) -> Result<(), DeviceError>;
}

fn slug(hostname: &str) -> String {
    let mut out = String::new();
    for c in hostname.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s = out.trim_matches('-');
    let s = &s[..s.len().min(56)];
    if s.is_empty() {
        "device".into()
    } else {
        s.trim_end_matches('-').to_owned()
    }
}

impl Devices for Store {
    fn add(
        &self,
        hostname: &str,
        kind: DeviceKind,
        public_key: &str,
        now_ms: u64,
    ) -> Result<NodeId, DeviceError> {
        let c = self.conn();
        let base = slug(hostname);
        let mut id = base.clone();
        for n in 2.. {
            let taken: Option<i64> = c
                .query_row("SELECT 1 FROM devices WHERE id=?1", [&id], |r| r.get(0))
                .optional()?;
            if taken.is_none() {
                break;
            }
            id = format!("{base}-{n}");
        }
        c.execute(
            "INSERT INTO devices(id, kind, hostname, public_key, created_ms) VALUES (?1,?2,?3,?4,?5)",
            params![id, code(kind), hostname, public_key, now_ms as i64],
        )?;
        Ok(NodeId::new(&id)?)
    }

    fn list(&self) -> Result<Vec<Device>, DeviceError> {
        let c = self.conn();
        let rows = c
            .prepare("SELECT id, kind, hostname, created_ms, revoked_ms, renewed_ms FROM devices ORDER BY created_ms, id")?
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<i64>>(4)?, r.get::<_, Option<i64>>(5)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, kind, hostname, created, revoked, renewed)| {
                Ok(Device {
                    id: NodeId::new(&id)?,
                    kind: from_code(kind),
                    hostname,
                    created_ms: created as u64,
                    revoked_ms: revoked.map(|v| v as u64),
                    renewed_ms: renewed.map(|v| v as u64),
                })
            })
            .collect()
    }

    fn revoke(&self, id: &NodeId, now_ms: u64) -> Result<(), DeviceError> {
        match self.conn().execute(
            "UPDATE devices SET revoked_ms=COALESCE(revoked_ms, ?2) WHERE id=?1",
            params![id.as_str(), now_ms as i64],
        )? {
            0 => Err(DeviceError::NotFound(id.clone())),
            _ => Ok(()),
        }
    }

    fn mark_renewed(&self, id: &NodeId, public_key: &str, now_ms: u64) -> Result<(), DeviceError> {
        match self.conn().execute(
            "UPDATE devices SET renewed_ms=?2, public_key=?3 WHERE id=?1",
            params![id.as_str(), now_ms as i64, public_key],
        )? {
            0 => Err(DeviceError::NotFound(id.clone())),
            _ => Ok(()),
        }
    }

    fn is_active(&self, id: &NodeId) -> Result<bool, DeviceError> {
        let revoked: Option<Option<i64>> = self
            .conn()
            .query_row(
                "SELECT revoked_ms FROM devices WHERE id=?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(matches!(revoked, Some(None)))
    }
}
