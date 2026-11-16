use crate::{ca::hex, AuthError, Store};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use ulak_proto::{AgentId, AgentToken};

fn hash(t: &AgentToken) -> String {
    hex(&Sha256::digest(t.expose().as_bytes()))
}

/// Bearer tokens for agents calling the MCP endpoint. Only a hash is stored.
pub trait AgentTokens {
    fn issue(&self, agent: &AgentId, ttl_ms: u64, now_ms: u64) -> Result<AgentToken, AuthError>;
    fn verify(&self, token: &AgentToken, now_ms: u64) -> Result<AgentId, AuthError>;
    /// Drops every token of this agent (e.g. when it is killed).
    fn revoke_agent(&self, agent: &AgentId) -> Result<(), AuthError>;
}

impl AgentTokens for Store {
    fn issue(&self, agent: &AgentId, ttl_ms: u64, now_ms: u64) -> Result<AgentToken, AuthError> {
        let mut raw = [0u8; 32];
        rand::fill(&mut raw[..]);
        let token = AgentToken::new(hex(&raw))?;
        self.conn().execute(
            "INSERT INTO agent_tokens(hash, agent, expires_ms) VALUES (?1,?2,?3)",
            params![hash(&token), agent.as_str(), (now_ms + ttl_ms) as i64],
        )?;
        Ok(token)
    }

    fn verify(&self, token: &AgentToken, now_ms: u64) -> Result<AgentId, AuthError> {
        let agent: Option<String> = self
            .conn()
            .query_row(
                "SELECT agent FROM agent_tokens WHERE hash=?1 AND expires_ms >= ?2",
                params![hash(token), now_ms as i64],
                |r| r.get(0),
            )
            .optional()?;
        Ok(AgentId::new(&agent.ok_or(AuthError::Invalid)?)?)
    }

    fn revoke_agent(&self, agent: &AgentId) -> Result<(), AuthError> {
        self.conn()
            .execute("DELETE FROM agent_tokens WHERE agent=?1", [agent.as_str()])?;
        Ok(())
    }
}
