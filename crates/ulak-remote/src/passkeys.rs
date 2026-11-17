use crate::{ca::hex, AuthError, Store};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use ulak_proto::NodeId;
use webauthn_rs::prelude::Passkey;

pub(crate) fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// A registered passkey as listed to the operator (the credential itself stays inside the hub).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PasskeyInfo {
    pub id: NodeId,
    pub name: String,
    pub created_ms: u64,
    pub revoked_ms: Option<u64>,
}

pub trait Passkeys {
    fn add_passkey(&self, name: &str, key: &Passkey, now_ms: u64) -> Result<NodeId, AuthError>;
    fn list_passkeys(&self) -> Result<Vec<PasskeyInfo>, AuthError>;
    /// Non-revoked passkeys with their credentials.
    fn active_passkeys(&self) -> Result<Vec<(NodeId, Passkey)>, AuthError>;
    /// Stores the updated credential (signature counter) after a login.
    fn save_passkey(&self, id: &NodeId, key: &Passkey) -> Result<(), AuthError>;
    /// Revokes the passkey and drops its sessions. Unknown id: `AuthError::Invalid`.
    fn revoke_passkey(&self, id: &NodeId, now_ms: u64) -> Result<(), AuthError>;
}

/// Browser sessions after a passkey login. Like agent tokens, only a hash is stored.
pub trait Sessions {
    fn issue_session(
        &self,
        passkey: &NodeId,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<String, AuthError>;
    /// The passkey behind a live token; revoked passkeys and expired tokens are `Invalid`.
    fn verify_session(&self, token: &str, now_ms: u64) -> Result<NodeId, AuthError>;
    /// Deletes one session (logout). Unknown token: no error.
    fn end_session(&self, token: &str) -> Result<(), AuthError>;
}

impl Passkeys for Store {
    fn add_passkey(&self, name: &str, key: &Passkey, now_ms: u64) -> Result<NodeId, AuthError> {
        let mut raw = [0u8; 4];
        rand::fill(&mut raw[..]);
        let id = NodeId::new(&format!("pk-{}", hex(&raw)))?;
        self.conn().execute(
            "INSERT INTO passkeys(id, name, credential, created_ms) VALUES (?1,?2,?3,?4)",
            params![
                id.as_str(),
                name,
                serde_json::to_string(key)?,
                now_ms as i64
            ],
        )?;
        Ok(id)
    }

    fn list_passkeys(&self) -> Result<Vec<PasskeyInfo>, AuthError> {
        let c = self.conn();
        let rows = c
            .prepare(
                "SELECT id, name, created_ms, revoked_ms FROM passkeys ORDER BY created_ms, id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, name, created, revoked)| {
                Ok(PasskeyInfo {
                    id: NodeId::new(&id)?,
                    name,
                    created_ms: created as u64,
                    revoked_ms: revoked.map(|v| v as u64),
                })
            })
            .collect()
    }

    fn active_passkeys(&self) -> Result<Vec<(NodeId, Passkey)>, AuthError> {
        let c = self.conn();
        let rows = c
            .prepare("SELECT id, credential FROM passkeys WHERE revoked_ms IS NULL ORDER BY created_ms, id")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, blob)| Ok((NodeId::new(&id)?, serde_json::from_str(&blob)?)))
            .collect()
    }

    fn save_passkey(&self, id: &NodeId, key: &Passkey) -> Result<(), AuthError> {
        self.conn().execute(
            "UPDATE passkeys SET credential=?2 WHERE id=?1",
            params![id.as_str(), serde_json::to_string(key)?],
        )?;
        Ok(())
    }

    fn revoke_passkey(&self, id: &NodeId, now_ms: u64) -> Result<(), AuthError> {
        let c = self.conn();
        let n = c.execute(
            "UPDATE passkeys SET revoked_ms=COALESCE(revoked_ms, ?2) WHERE id=?1",
            params![id.as_str(), now_ms as i64],
        )?;
        if n == 0 {
            return Err(AuthError::Invalid);
        }
        c.execute("DELETE FROM sessions WHERE passkey=?1", [id.as_str()])?;
        Ok(())
    }
}

impl Sessions for Store {
    fn issue_session(
        &self,
        passkey: &NodeId,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<String, AuthError> {
        let mut raw = [0u8; 32];
        rand::fill(&mut raw[..]);
        let token = hex(&raw);
        let c = self.conn();
        // Expired rows are dead weight; sweep them on each login.
        c.execute(
            "DELETE FROM sessions WHERE expires_ms < ?1",
            [now_ms as i64],
        )?;
        c.execute(
            "INSERT INTO sessions(hash, passkey, expires_ms) VALUES (?1,?2,?3)",
            params![hash(&token), passkey.as_str(), (now_ms + ttl_ms) as i64],
        )?;
        Ok(token)
    }

    fn end_session(&self, token: &str) -> Result<(), AuthError> {
        self.conn()
            .execute("DELETE FROM sessions WHERE hash=?1", [hash(token)])?;
        Ok(())
    }

    fn verify_session(&self, token: &str, now_ms: u64) -> Result<NodeId, AuthError> {
        let id: Option<String> = self
            .conn()
            .query_row(
                "SELECT s.passkey FROM sessions s JOIN passkeys p ON p.id = s.passkey \
                 WHERE s.hash=?1 AND s.expires_ms >= ?2 AND p.revoked_ms IS NULL",
                params![hash(token), now_ms as i64],
                |r| r.get(0),
            )
            .optional()?;
        Ok(NodeId::new(&id.ok_or(AuthError::Invalid)?)?)
    }
}
