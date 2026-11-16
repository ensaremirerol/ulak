use crate::{MailboxError, QuestionError};
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
CREATE TABLE IF NOT EXISTS devices(
  id TEXT PRIMARY KEY, kind INTEGER NOT NULL, hostname TEXT NOT NULL, public_key TEXT NOT NULL,
  created_ms INTEGER NOT NULL, revoked_ms INTEGER, renewed_ms INTEGER);
CREATE TABLE IF NOT EXISTS agent_tokens(
  hash TEXT PRIMARY KEY, agent TEXT NOT NULL, expires_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS passkeys(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, credential TEXT NOT NULL,
  created_ms INTEGER NOT NULL, revoked_ms INTEGER);
CREATE TABLE IF NOT EXISTS sessions(
  hash TEXT PRIMARY KEY, passkey TEXT NOT NULL, expires_ms INTEGER NOT NULL);";

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
