use crate::{
    BlockerError, CredentialError, HubProfileError, MailboxError, QuestionError, QueueError,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::{path::Path, sync::Mutex};
use ulak_proto::*;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
  from_agent TEXT NOT NULL, to_agent TEXT NOT NULL, thread TEXT NOT NULL,
  payload TEXT NOT NULL, status INTEGER NOT NULL DEFAULT 0, at_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS messages_inbox ON messages(to_agent, status, seq);
CREATE TABLE IF NOT EXISTS questions(
  id TEXT PRIMARY KEY, from_agent TEXT NOT NULL, text TEXT NOT NULL, at_ms INTEGER NOT NULL,
  answer TEXT, answered_ms INTEGER, collected_ms INTEGER,
  nudges INTEGER NOT NULL DEFAULT 0, last_nudge_ms INTEGER);
CREATE TABLE IF NOT EXISTS blockers(
  id TEXT PRIMARY KEY, node TEXT NOT NULL, profile TEXT NOT NULL, kind INTEGER NOT NULL,
  agent TEXT, detail TEXT NOT NULL, at_ms INTEGER NOT NULL, resolved_ms INTEGER);
CREATE TABLE IF NOT EXISTS devices(
  id TEXT PRIMARY KEY, kind INTEGER NOT NULL, hostname TEXT NOT NULL, public_key TEXT NOT NULL,
  created_ms INTEGER NOT NULL, revoked_ms INTEGER, renewed_ms INTEGER);
CREATE TABLE IF NOT EXISTS agent_tokens(
  hash TEXT PRIMARY KEY, agent TEXT NOT NULL, expires_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS passkeys(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, credential TEXT NOT NULL,
  created_ms INTEGER NOT NULL, revoked_ms INTEGER);
CREATE TABLE IF NOT EXISTS sessions(
  hash TEXT PRIMARY KEY, passkey TEXT NOT NULL, expires_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS credentials(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, login TEXT NOT NULL,
  nonce BLOB NOT NULL, ciphertext BLOB NOT NULL, created_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS queue(
  id TEXT PRIMARY KEY, agent TEXT NOT NULL, spec_json TEXT NOT NULL, target_json TEXT NOT NULL,
  state TEXT NOT NULL, created_ms INTEGER NOT NULL, placed_ms INTEGER, error TEXT);
CREATE TABLE IF NOT EXISTS hub_profiles(
  node TEXT NOT NULL, name TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY(node, name));
CREATE TABLE IF NOT EXISTS node_settings(node TEXT PRIMARY KEY, json TEXT NOT NULL);";

pub struct Store(Mutex<Connection>);

impl Store {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        Self::init(Connection::open(path)?)
    }
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        Self::init(Connection::open_in_memory()?)
    }
    fn init(c: Connection) -> Result<Self, rusqlite::Error> {
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.execute_batch(SCHEMA)?;
        // Databases from before renewal lack the column.
        let has: bool = c.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('devices') WHERE name='renewed_ms'",
            [],
            |r| r.get(0),
        )?;
        if !has {
            c.execute("ALTER TABLE devices ADD COLUMN renewed_ms INTEGER", [])?;
        }
        Ok(Self(Mutex::new(c)))
    }
    pub(crate) fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0
            .lock()
            .expect("store lock is never held across a panic")
    }
}

pub trait Mailbox {
    /// `thread: None` starts a new thread.
    fn send(
        &self,
        from: &AgentId,
        to: &AgentId,
        thread: Option<ThreadId>,
        body: String,
        now_ms: u64,
    ) -> Result<Message, MailboxError>;
    /// Oldest-first Pending messages for `agent`; they become Delivered.
    fn inbox(&self, agent: &AgentId, limit: u32) -> Result<Vec<Message>, MailboxError>;
    fn mark_read(&self, id: &MessageId) -> Result<(), MailboxError>;
    /// Everything (or one thread), oldest first, for the inspector.
    fn log(&self, thread: Option<&ThreadId>, limit: u32) -> Result<Vec<Message>, MailboxError>;
}

pub trait Questions {
    fn open(&self, from: &AgentId, text: String, now_ms: u64) -> Result<Question, QuestionError>;
    fn answer(&self, a: Answer, now_ms: u64) -> Result<(), QuestionError>;
    fn pending(&self) -> Result<Vec<Question>, QuestionError>;
    /// The answer if present; the first call marks it collected, which ends nudging.
    fn collect(&self, id: &QuestionId, now_ms: u64) -> Result<Option<Answer>, QuestionError>;
    /// Answered, uncollected questions whose last nudge is older than `every_ms`, under `max` nudges.
    fn due_nudges(
        &self,
        now_ms: u64,
        every_ms: u64,
        max: u32,
    ) -> Result<Vec<Question>, QuestionError>;
    fn record_nudge(&self, id: &QuestionId, now_ms: u64) -> Result<(), QuestionError>;
    /// The agent that asked, so only it can collect the answer.
    fn asker(&self, id: &QuestionId) -> Result<AgentId, QuestionError>;
}

/// Permanent: never expires, never nudged, survives restarts until the user resolves it.
pub trait Blockers {
    /// Dedupes on unresolved node+profile+kind: the existing row gets the new detail.
    /// Returns the blocker and whether it is new.
    fn open(
        &self,
        node: &NodeId,
        profile: &ProfileKey,
        kind: BlockerKind,
        agent: Option<&AgentId>,
        detail: String,
        now_ms: u64,
    ) -> Result<(Blocker, bool), BlockerError>;
    /// Returns the resolved blocker; `NotFound` if unknown or already resolved.
    fn resolve(&self, id: &BlockerId, now_ms: u64) -> Result<Blocker, BlockerError>;
    fn list_open(&self) -> Result<Vec<Blocker>, BlockerError>;
    fn is_blocked(
        &self,
        node: &NodeId,
        profile: &ProfileKey,
    ) -> Result<Option<Blocker>, BlockerError>;
}

const MSG_COLS: &str = "id, from_agent, to_agent, thread, payload, status, at_ms";

fn msg_row(r: &Row) -> rusqlite::Result<Result<Message, ProtoError>> {
    let (id, from, to, thread, payload, status, at): (
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
    ) = (
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    );
    Ok((|| {
        let mut m = Message::new(
            MessageId::new(&id)?,
            AgentId::new(&from)?,
            AgentId::new(&to)?,
            ThreadId::new(&thread)?,
            payload,
            at as u64,
        )?;
        m.status = match status {
            0 => MessageStatus::Pending,
            1 => MessageStatus::Delivered,
            _ => MessageStatus::Read,
        };
        Ok(m)
    })())
}

fn messages(
    c: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<Message>, MailboxError> {
    let rows = c
        .prepare(sql)?
        .query_map(args, msg_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect::<Result<_, _>>()?)
}

impl Mailbox for Store {
    fn send(
        &self,
        from: &AgentId,
        to: &AgentId,
        thread: Option<ThreadId>,
        body: String,
        now_ms: u64,
    ) -> Result<Message, MailboxError> {
        let id = MessageId::new(&uuid::Uuid::now_v7().to_string())?;
        let thread = match thread {
            Some(t) => t,
            None => ThreadId::new(id.as_str())?,
        };
        let msg = Message::new(id, from.clone(), to.clone(), thread, body, now_ms)?;
        self.conn().execute(
            "INSERT INTO messages(id, from_agent, to_agent, thread, payload, at_ms) VALUES (?1,?2,?3,?4,?5,?6)",
            params![msg.id().as_str(), from.as_str(), to.as_str(), msg.thread().as_str(), msg.payload(), now_ms as i64],
        )?;
        Ok(msg)
    }

    fn inbox(&self, agent: &AgentId, limit: u32) -> Result<Vec<Message>, MailboxError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let mut got = messages(
            &tx,
            &format!("SELECT {MSG_COLS} FROM messages WHERE to_agent=?1 AND status=0 ORDER BY seq LIMIT ?2"),
            params![agent.as_str(), limit],
        )?;
        for m in &mut got {
            tx.execute(
                "UPDATE messages SET status=1 WHERE id=?1",
                [m.id().as_str()],
            )?;
            m.status = MessageStatus::Delivered;
        }
        tx.commit()?;
        Ok(got)
    }

    fn mark_read(&self, id: &MessageId) -> Result<(), MailboxError> {
        match self
            .conn()
            .execute("UPDATE messages SET status=2 WHERE id=?1", [id.as_str()])?
        {
            0 => Err(MailboxError::NotFound(id.clone())),
            _ => Ok(()),
        }
    }

    fn log(&self, thread: Option<&ThreadId>, limit: u32) -> Result<Vec<Message>, MailboxError> {
        messages(
            &self.conn(),
            &format!("SELECT {MSG_COLS} FROM messages WHERE (?1 IS NULL OR thread=?1) ORDER BY seq LIMIT ?2"),
            params![thread.map(|t| t.as_str()), limit],
        )
    }
}

fn question_row(r: &Row) -> rusqlite::Result<Result<Question, ProtoError>> {
    let (id, from, text, at): (String, String, String, i64) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
    Ok((|| {
        Question::new(QuestionId::new(&id)?, AgentId::new(&from)?, text, at as u64)
    })())
}

fn questions(
    c: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<Question>, QuestionError> {
    let rows = c
        .prepare(sql)?
        .query_map(args, question_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect::<Result<_, _>>()?)
}

impl Questions for Store {
    fn open(&self, from: &AgentId, text: String, now_ms: u64) -> Result<Question, QuestionError> {
        let q = Question::new(
            QuestionId::new(&uuid::Uuid::now_v7().to_string())?,
            from.clone(),
            text,
            now_ms,
        )?;
        self.conn().execute(
            "INSERT INTO questions(id, from_agent, text, at_ms) VALUES (?1,?2,?3,?4)",
            params![q.id().as_str(), from.as_str(), q.text(), now_ms as i64],
        )?;
        Ok(q)
    }

    fn answer(&self, a: Answer, now_ms: u64) -> Result<(), QuestionError> {
        let c = self.conn();
        let id = a.question();
        if c.execute(
            "UPDATE questions SET answer=?2, answered_ms=?3 WHERE id=?1 AND answer IS NULL",
            params![id.as_str(), a.text(), now_ms as i64],
        )? == 1
        {
            return Ok(());
        }
        let exists = c
            .query_row("SELECT 1 FROM questions WHERE id=?1", [id.as_str()], |_| {
                Ok(())
            })
            .optional()?;
        Err(match exists {
            Some(()) => QuestionError::AlreadyAnswered(id.clone()),
            None => QuestionError::NotFound(id.clone()),
        })
    }

    fn pending(&self) -> Result<Vec<Question>, QuestionError> {
        questions(
            &self.conn(),
            "SELECT id, from_agent, text, at_ms FROM questions WHERE answer IS NULL ORDER BY at_ms",
            [],
        )
    }

    fn collect(&self, id: &QuestionId, now_ms: u64) -> Result<Option<Answer>, QuestionError> {
        let c = self.conn();
        let row: Option<Option<String>> = c
            .query_row(
                "SELECT answer FROM questions WHERE id=?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        match row {
            None => Err(QuestionError::NotFound(id.clone())),
            Some(None) => Ok(None),
            Some(Some(text)) => {
                c.execute(
                    "UPDATE questions SET collected_ms=?2 WHERE id=?1 AND collected_ms IS NULL",
                    params![id.as_str(), now_ms as i64],
                )?;
                Ok(Some(Answer::new(id.clone(), text)?))
            }
        }
    }

    fn due_nudges(
        &self,
        now_ms: u64,
        every_ms: u64,
        max: u32,
    ) -> Result<Vec<Question>, QuestionError> {
        questions(
            &self.conn(),
            "SELECT id, from_agent, text, at_ms FROM questions
             WHERE answered_ms IS NOT NULL AND collected_ms IS NULL AND nudges < ?1
               AND (last_nudge_ms IS NULL OR last_nudge_ms + ?2 <= ?3) ORDER BY at_ms",
            params![max, every_ms as i64, now_ms as i64],
        )
    }

    fn asker(&self, id: &QuestionId) -> Result<AgentId, QuestionError> {
        let from: Option<String> = self
            .conn()
            .query_row(
                "SELECT from_agent FROM questions WHERE id=?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(AgentId::new(
            &from.ok_or_else(|| QuestionError::NotFound(id.clone()))?,
        )?)
    }

    fn record_nudge(&self, id: &QuestionId, now_ms: u64) -> Result<(), QuestionError> {
        match self.conn().execute(
            "UPDATE questions SET nudges=nudges+1, last_nudge_ms=?2 WHERE id=?1",
            params![id.as_str(), now_ms as i64],
        )? {
            0 => Err(QuestionError::NotFound(id.clone())),
            _ => Ok(()),
        }
    }
}

const BLK_COLS: &str = "id, node, profile, kind, agent, detail, at_ms";

fn blocker_row(r: &Row) -> rusqlite::Result<Result<Blocker, ProtoError>> {
    let (id, node, profile, kind, agent, detail, at): (
        String,
        String,
        String,
        i64,
        Option<String>,
        String,
        i64,
    ) = (
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    );
    Ok((|| {
        Ok(Blocker::new(
            BlockerId::new(&id)?,
            NodeId::new(&node)?,
            ProfileKey::new(&profile)?,
            if kind == 0 {
                BlockerKind::Login
            } else {
                BlockerKind::Install
            },
            agent.map(|a| AgentId::new(&a)).transpose()?,
            detail,
            at as u64,
        ))
    })())
}

fn blockers(
    c: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<Blocker>, BlockerError> {
    let rows = c
        .prepare(sql)?
        .query_map(args, blocker_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect::<Result<_, _>>()?)
}

impl Blockers for Store {
    fn open(
        &self,
        node: &NodeId,
        profile: &ProfileKey,
        kind: BlockerKind,
        agent: Option<&AgentId>,
        detail: String,
        now_ms: u64,
    ) -> Result<(Blocker, bool), BlockerError> {
        let c = self.conn();
        let k = kind as i64;
        let find = format!(
            "SELECT {BLK_COLS} FROM blockers WHERE node=?1 AND profile=?2 AND kind=?3 AND resolved_ms IS NULL"
        );
        let key = params![node.as_str(), profile.as_str(), k];
        if let Some(old) = blockers(&c, &find, key)?.pop() {
            let fresh = Blocker::new(
                old.id().clone(),
                node.clone(),
                profile.clone(),
                kind,
                old.agent().cloned(),
                detail,
                old.at_ms(),
            );
            c.execute(
                "UPDATE blockers SET detail=?2 WHERE id=?1",
                params![fresh.id().as_str(), fresh.detail()],
            )?;
            return Ok((fresh, false));
        }
        let b = Blocker::new(
            BlockerId::new(&uuid::Uuid::now_v7().to_string())?,
            node.clone(),
            profile.clone(),
            kind,
            agent.cloned(),
            detail,
            now_ms,
        );
        c.execute(
            "INSERT INTO blockers(id, node, profile, kind, agent, detail, at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                b.id().as_str(),
                node.as_str(),
                profile.as_str(),
                k,
                agent.map(|a| a.as_str()),
                b.detail(),
                now_ms as i64
            ],
        )?;
        Ok((b, true))
    }

    fn resolve(&self, id: &BlockerId, now_ms: u64) -> Result<Blocker, BlockerError> {
        let c = self.conn();
        let b = blockers(
            &c,
            &format!("SELECT {BLK_COLS} FROM blockers WHERE id=?1 AND resolved_ms IS NULL"),
            [id.as_str()],
        )?
        .pop()
        .ok_or_else(|| BlockerError::NotFound(id.clone()))?;
        c.execute(
            "UPDATE blockers SET resolved_ms=?2 WHERE id=?1",
            params![id.as_str(), now_ms as i64],
        )?;
        Ok(b)
    }

    fn list_open(&self) -> Result<Vec<Blocker>, BlockerError> {
        blockers(
            &self.conn(),
            &format!(
                "SELECT {BLK_COLS} FROM blockers WHERE resolved_ms IS NULL ORDER BY at_ms, id"
            ),
            [],
        )
    }

    fn is_blocked(
        &self,
        node: &NodeId,
        profile: &ProfileKey,
    ) -> Result<Option<Blocker>, BlockerError> {
        Ok(blockers(
            &self.conn(),
            &format!("SELECT {BLK_COLS} FROM blockers WHERE node=?1 AND profile=?2 AND resolved_ms IS NULL ORDER BY at_ms LIMIT 1"),
            params![node.as_str(), profile.as_str()],
        )?
        .pop())
    }
}

/// Most queued + failed items kept at once.
pub const QUEUE_CAP: usize = 100;

/// Encrypted GitHub tokens; `Secrets` does the crypto, the store only holds the bytes.
pub trait Credentials {
    fn insert_credential(
        &self,
        info: &CredentialInfo,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<(), CredentialError>;
    fn list_credentials(&self) -> Result<Vec<CredentialInfo>, CredentialError>;
    /// `(nonce, ciphertext)`.
    fn credential_secret(&self, id: &CredentialId) -> Result<(Vec<u8>, Vec<u8>), CredentialError>;
    fn delete_credential(&self, id: &CredentialId) -> Result<(), CredentialError>;
}

fn cred_row(r: &Row) -> rusqlite::Result<Result<CredentialInfo, ProtoError>> {
    let (id, name, login, at): (String, String, String, i64) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
    Ok(CredentialId::new(&id).map(|id| CredentialInfo {
        id,
        name,
        kind: CredentialKind::Github,
        login,
        created_ms: at as u64,
    }))
}

impl Credentials for Store {
    fn insert_credential(
        &self,
        i: &CredentialInfo,
        nonce: &[u8],
        ct: &[u8],
    ) -> Result<(), CredentialError> {
        self.conn().execute(
            "INSERT INTO credentials(id,name,kind,login,nonce,ciphertext,created_ms) VALUES (?1,?2,'github',?3,?4,?5,?6)",
            params![i.id.as_str(), i.name, i.login, nonce, ct, i.created_ms as i64],
        )?;
        Ok(())
    }
    fn list_credentials(&self) -> Result<Vec<CredentialInfo>, CredentialError> {
        let c = self.conn();
        let mut st =
            c.prepare("SELECT id,name,login,created_ms FROM credentials ORDER BY created_ms, id")?;
        let rows = st.query_map([], cred_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect::<Result<_, _>>()?)
    }
    fn credential_secret(&self, id: &CredentialId) -> Result<(Vec<u8>, Vec<u8>), CredentialError> {
        self.conn()
            .query_row(
                "SELECT nonce, ciphertext FROM credentials WHERE id=?1",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CredentialError::NotFound(id.clone()))
    }
    fn delete_credential(&self, id: &CredentialId) -> Result<(), CredentialError> {
        match self
            .conn()
            .execute("DELETE FROM credentials WHERE id=?1", [id.as_str()])?
        {
            0 => Err(CredentialError::NotFound(id.clone())),
            _ => Ok(()),
        }
    }
}

/// A listed queue item with the spec to place it with.
#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub item: QueuedSpawn,
    pub spec: SpawnSpec,
}

/// Spawns waiting for a node. States: `queued`, `failed`, `placed` (placed rows are kept but
/// never listed).
pub trait Queue {
    /// Rejects when `QUEUE_CAP` queued+failed items exist, or when `agent` is already
    /// queued/failed or listed in `known_agents` (agents on registry nodes).
    fn enqueue(
        &self,
        spec: SpawnSpec,
        target: SpawnTarget,
        known_agents: &[AgentId],
        now_ms: u64,
    ) -> Result<QueuedSpawn, QueueError>;
    /// Queued and failed items, oldest first.
    fn queue_list(&self) -> Result<Vec<QueueEntry>, QueueError>;
    /// Removes a queued or failed item.
    fn cancel_queued(&self, id: &QueueId) -> Result<(), QueueError>;
    fn mark_placed(&self, id: &QueueId, now_ms: u64) -> Result<(), QueueError>;
    fn mark_failed(&self, id: &QueueId, error: &str) -> Result<QueuedSpawn, QueueError>;
}

fn queue_entries(
    c: &Connection,
    sql: &str,
    p: impl rusqlite::Params,
) -> Result<Vec<QueueEntry>, QueueError> {
    let mut st = c.prepare(sql)?;
    let rows = st
        .query_map(p, |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, _agent, spec, target, at, state, err)| {
            let spec: SpawnSpec = serde_json::from_str(&spec)?;
            Ok(QueueEntry {
                item: QueuedSpawn {
                    id: QueueId::new(&id)?,
                    agent: spec.agent_id().clone(),
                    source: spec.source(),
                    target: serde_json::from_str(&target)?,
                    profile: spec.profile().key()?,
                    prompt_preview: QueuedSpawn::preview(spec.prompt()),
                    created_ms: at as u64,
                    state: if state == "failed" {
                        QueueState::Failed(err.unwrap_or_default())
                    } else {
                        QueueState::Queued
                    },
                },
                spec,
            })
        })
        .collect()
}

const QUEUE_COLS: &str = "id, agent, spec_json, target_json, created_ms, state, error";

impl Queue for Store {
    fn enqueue(
        &self,
        spec: SpawnSpec,
        target: SpawnTarget,
        known: &[AgentId],
        now_ms: u64,
    ) -> Result<QueuedSpawn, QueueError> {
        let c = self.conn();
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM queue WHERE state IN ('queued','failed')",
            [],
            |r| r.get(0),
        )?;
        if n as usize >= QUEUE_CAP {
            return Err(QueueError::Full(QUEUE_CAP));
        }
        let dup: i64 = c.query_row(
            "SELECT COUNT(*) FROM queue WHERE agent=?1 AND state IN ('queued','failed')",
            [spec.agent_id().as_str()],
            |r| r.get(0),
        )?;
        if dup > 0 || known.contains(spec.agent_id()) {
            return Err(QueueError::DuplicateAgent(spec.agent_id().clone()));
        }
        let id = QueueId::new(&uuid::Uuid::now_v7().to_string())?;
        c.execute(
            "INSERT INTO queue(id,agent,spec_json,target_json,state,created_ms) VALUES (?1,?2,?3,?4,'queued',?5)",
            params![
                id.as_str(),
                spec.agent_id().as_str(),
                serde_json::to_string(&spec)?,
                serde_json::to_string(&target)?,
                now_ms as i64
            ],
        )?;
        Ok(QueuedSpawn {
            id,
            agent: spec.agent_id().clone(),
            source: spec.source(),
            target,
            profile: spec.profile().key()?,
            prompt_preview: QueuedSpawn::preview(spec.prompt()),
            created_ms: now_ms,
            state: QueueState::Queued,
        })
    }
    fn queue_list(&self) -> Result<Vec<QueueEntry>, QueueError> {
        queue_entries(
            &self.conn(),
            &format!("SELECT {QUEUE_COLS} FROM queue WHERE state IN ('queued','failed') ORDER BY created_ms, id"),
            [],
        )
    }
    fn cancel_queued(&self, id: &QueueId) -> Result<(), QueueError> {
        match self.conn().execute(
            "DELETE FROM queue WHERE id=?1 AND state IN ('queued','failed')",
            [id.as_str()],
        )? {
            0 => Err(QueueError::NotFound(id.clone())),
            _ => Ok(()),
        }
    }
    fn mark_placed(&self, id: &QueueId, now_ms: u64) -> Result<(), QueueError> {
        match self.conn().execute(
            "UPDATE queue SET state='placed', placed_ms=?2 WHERE id=?1 AND state='queued'",
            params![id.as_str(), now_ms as i64],
        )? {
            0 => Err(QueueError::NotFound(id.clone())),
            _ => Ok(()),
        }
    }
    fn mark_failed(&self, id: &QueueId, error: &str) -> Result<QueuedSpawn, QueueError> {
        let c = self.conn();
        let error: String = error.chars().take(500).collect();
        c.execute(
            "UPDATE queue SET state='failed', error=?2 WHERE id=?1 AND state='queued'",
            params![id.as_str(), error],
        )?;
        queue_entries(
            &c,
            &format!("SELECT {QUEUE_COLS} FROM queue WHERE id=?1 AND state='failed'"),
            [id.as_str()],
        )?
        .pop()
        .map(|e| e.item)
        .ok_or_else(|| QueueError::NotFound(id.clone()))
    }
}

/// Hub-managed profiles (`node` '' = global) and per-node settings, stored as JSON.
pub trait HubProfiles {
    /// Global first, then by node, then by name.
    fn list_hub_profiles(&self) -> Result<Vec<ScopedProfile>, HubProfileError>;
    /// Inserts or replaces; a new entry is refused once `max` profiles exist.
    fn upsert_hub_profile(
        &self,
        node: Option<&NodeId>,
        p: &HubProfile,
        max: usize,
    ) -> Result<(), HubProfileError>;
    fn delete_hub_profile(&self, node: Option<&NodeId>, name: &str) -> Result<(), HubProfileError>;
    fn list_node_settings(&self) -> Result<Vec<NodeSettingsEntry>, HubProfileError>;
    /// All-`None` settings delete the row.
    fn save_node_settings(&self, node: &NodeId, s: &NodeSettings) -> Result<(), HubProfileError>;
}

fn node_key(n: Option<&NodeId>) -> &str {
    n.map_or("", NodeId::as_str)
}

impl HubProfiles for Store {
    fn list_hub_profiles(&self) -> Result<Vec<ScopedProfile>, HubProfileError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT node, json FROM hub_profiles ORDER BY node, name")?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(node, json)| {
                Ok(ScopedProfile {
                    node: (!node.is_empty()).then(|| NodeId::new(&node)).transpose()?,
                    profile: serde_json::from_str(&json)?,
                })
            })
            .collect()
    }
    fn upsert_hub_profile(
        &self,
        node: Option<&NodeId>,
        p: &HubProfile,
        max: usize,
    ) -> Result<(), HubProfileError> {
        let c = self.conn();
        let (total, same): (i64, i64) = c.query_row(
            "SELECT COUNT(*), COALESCE(SUM(node=?1 AND name=?2), 0) FROM hub_profiles",
            params![node_key(node), p.name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if same == 0 && total as usize >= max {
            return Err(HubProfileError::Full(max));
        }
        c.execute(
            "INSERT OR REPLACE INTO hub_profiles(node,name,json) VALUES (?1,?2,?3)",
            params![node_key(node), p.name, serde_json::to_string(p)?],
        )?;
        Ok(())
    }
    fn delete_hub_profile(&self, node: Option<&NodeId>, name: &str) -> Result<(), HubProfileError> {
        match self.conn().execute(
            "DELETE FROM hub_profiles WHERE node=?1 AND name=?2",
            params![node_key(node), name],
        )? {
            0 => Err(HubProfileError::NotFound(name.to_owned())),
            _ => Ok(()),
        }
    }
    fn list_node_settings(&self) -> Result<Vec<NodeSettingsEntry>, HubProfileError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT node, json FROM node_settings ORDER BY node")?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(node, json)| {
                Ok(NodeSettingsEntry {
                    node: NodeId::new(&node)?,
                    settings: serde_json::from_str(&json)?,
                })
            })
            .collect()
    }
    fn save_node_settings(&self, node: &NodeId, s: &NodeSettings) -> Result<(), HubProfileError> {
        let c = self.conn();
        if *s == NodeSettings::default() {
            c.execute("DELETE FROM node_settings WHERE node=?1", [node.as_str()])?;
        } else {
            c.execute(
                "INSERT OR REPLACE INTO node_settings(node,json) VALUES (?1,?2)",
                params![node.as_str(), serde_json::to_string(s)?],
            )?;
        }
        Ok(())
    }
}
