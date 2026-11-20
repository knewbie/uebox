//! Persistent visible chat history with no model or editor dependencies.
use std::{
    fmt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Row, TransactionBehavior};

mod actor_label;
mod storage;

pub use actor_label::{ActorLabelReceipt, ActorLabelStatus};

const MAX_USER_TEXT_BYTES: usize = 1024 * 1024;
const MAX_ASSISTANT_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 80;
const MAX_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Unreal,
    Unity,
    Godot,
}

impl Engine {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreal => "unreal",
            Self::Unity => "unity",
            Self::Godot => "godot",
        }
    }

    fn from_str(value: &str) -> Result<Self, HistoryError> {
        match value {
            "unreal" => Ok(Self::Unreal),
            "unity" => Ok(Self::Unity),
            "godot" => Ok(Self::Godot),
            _ => Err(HistoryError::Corrupt),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCode {
    ModelUnavailable,
    ModelResponse,
    LimitReached,
    TargetUnavailable,
    Internal,
}

impl FailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelUnavailable => "model_unavailable",
            Self::ModelResponse => "model_response",
            Self::LimitReached => "limit_reached",
            Self::TargetUnavailable => "target_unavailable",
            Self::Internal => "internal",
        }
    }

    fn from_str(value: &str) -> Result<Self, HistoryError> {
        match value {
            "model_unavailable" => Ok(Self::ModelUnavailable),
            "model_response" => Ok(Self::ModelResponse),
            "limit_reached" => Ok(Self::LimitReached),
            "target_unavailable" => Ok(Self::TargetUnavailable),
            "internal" => Ok(Self::Internal),
            _ => Err(HistoryError::Corrupt),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryError {
    InvalidInput,
    Missing,
    AlreadyExists,
    Unsupported,
    Corrupt,
    RecoveryRequired,
    Busy,
    ChatAlreadyExists,
    ChatNotFound,
    EngineMismatch,
    TurnNotFound,
    ActiveTurn,
    TurnAlreadyFinished,
    ReceiptConflict,
    Io,
    Database,
}

impl HistoryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Missing => "missing",
            Self::AlreadyExists => "already_exists",
            Self::Unsupported => "unsupported",
            Self::Corrupt => "corrupt",
            Self::RecoveryRequired => "recovery_required",
            Self::Busy => "busy",
            Self::ChatAlreadyExists => "chat_already_exists",
            Self::ChatNotFound => "chat_not_found",
            Self::EngineMismatch => "engine_mismatch",
            Self::TurnNotFound => "turn_not_found",
            Self::ActiveTurn => "active_turn",
            Self::TurnAlreadyFinished => "turn_already_finished",
            Self::ReceiptConflict => "receipt_conflict",
            Self::Io => "io",
            Self::Database => "database",
        }
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for HistoryError {}

impl From<rusqlite::Error> for HistoryError {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            rusqlite::Error::SqliteFailure(error, _) => match error.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Self::Busy,
                ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => Self::Corrupt,
                ErrorCode::CannotOpen
                | ErrorCode::SystemIoFailure
                | ErrorCode::PermissionDenied
                | ErrorCode::ReadOnly
                | ErrorCode::DiskFull => Self::Io,
                _ => Self::Database,
            },
            rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..) => Self::Corrupt,
            _ => Self::Database,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub id: String,
    pub engine: Engine,
    pub title: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
    pub summary: ChatSummary,
    pub turns: Vec<Turn>,
}

pub type TurnId = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub id: TurnId,
    pub user_text: String,
    pub state: TurnState,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub actor_label_receipts: Vec<ActorLabelReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnState {
    Active,
    Completed { assistant_text: String },
    Failed { code: FailureCode },
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTurn<'a> {
    Completed { assistant_text: &'a str },
    Failed { code: FailureCode },
    Cancelled,
    Interrupted,
}

#[derive(Debug)]
pub struct ChatHistory {
    connection: Connection,
}

impl ChatHistory {
    /// Create a database under a host-owned, existing directory without overwriting it.
    pub fn create_new(database: impl AsRef<Path>) -> Result<Self, HistoryError> {
        Ok(Self {
            connection: storage::create_new(database.as_ref())?,
        })
    }

    /// Open an existing v1 database. Active turns retain their recorded state.
    pub fn open_existing(database: impl AsRef<Path>) -> Result<Self, HistoryError> {
        Ok(Self {
            connection: storage::open_existing(database.as_ref())?,
        })
    }

    /// Allow SQLite to replay a private single-database rollback journal.
    /// The host must hold exclusive startup ownership of the directory and
    /// know that the database and journal belong to this history store.
    /// Call recover_interrupted separately to retire unfinished turns.
    pub fn open_existing_recovering(database: impl AsRef<Path>) -> Result<Self, HistoryError> {
        Ok(Self {
            connection: storage::open_existing_recovering(database.as_ref())?,
        })
    }

    pub fn create_chat(&mut self, id: &str, engine: Engine) -> Result<ChatSummary, HistoryError> {
        validate_id(id)?;
        let now = now_ms();
        if !insert_chat(&self.connection, id, engine, now)? {
            return Err(HistoryError::ChatAlreadyExists);
        }
        Ok(ChatSummary {
            id: id.to_owned(),
            engine,
            title: String::new(),
            created_at_ms: now,
            updated_at_ms: now,
        })
    }

    /// Newest activity first, then ascending opaque ID when timestamps tie.
    pub fn list_chats(&self, limit: u32, offset: u64) -> Result<Vec<ChatSummary>, HistoryError> {
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(HistoryError::InvalidInput);
        }
        let offset = i64::try_from(offset).map_err(|_| HistoryError::InvalidInput)?;
        let mut statement = self.connection.prepare(
            "SELECT id, engine, title, created_at_ms, updated_at_ms FROM chats
             ORDER BY updated_at_ms DESC, id LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit, offset], summary_row)?;
        rows.map(|row| decode_summary(row?)).collect()
    }

    pub fn get_chat(&self, id: &str) -> Result<Option<Chat>, HistoryError> {
        validate_id(id)?;
        let transaction = self.connection.unchecked_transaction()?;
        let Some(summary) = transaction
            .query_row(
                "SELECT id, engine, title, created_at_ms, updated_at_ms FROM chats WHERE id = ?1",
                [id],
                summary_row,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let summary = decode_summary(summary)?;
        let mut turns = {
            let mut statement = transaction.prepare(
                "SELECT id, user_text, state, assistant_text, failure_code, started_at_ms, finished_at_ms
                 FROM turns WHERE chat_id = ?1 ORDER BY id",
            )?;
            let rows = statement.query_map([id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?;
            rows.map(|row| decode_turn(row?))
                .collect::<Result<Vec<_>, HistoryError>>()?
        };
        actor_label::load_receipts(&transaction, id, summary.engine, &mut turns)?;
        transaction.commit()?;
        Ok(Some(Chat { summary, turns }))
    }

    /// Create the chat if absent and persist the turn in the same transaction.
    pub fn begin_chat_turn(
        &mut self,
        id: &str,
        engine: Engine,
        user_text: &str,
    ) -> Result<TurnId, HistoryError> {
        validate_id(id)?;
        validate_user_text(user_text)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        if !insert_chat(&transaction, id, engine, now)? && chat_engine(&transaction, id)? != engine
        {
            return Err(HistoryError::EngineMismatch);
        }
        let turn = insert_turn(&transaction, id, user_text, now)?;
        transaction.commit()?;
        Ok(turn)
    }

    /// Persist the visible user message in a chat that already exists.
    pub fn begin_turn(&mut self, chat_id: &str, user_text: &str) -> Result<TurnId, HistoryError> {
        validate_id(chat_id)?;
        validate_user_text(user_text)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        chat_engine(&transaction, chat_id)?;
        let id = insert_turn(&transaction, chat_id, user_text, now_ms())?;
        transaction.commit()?;
        Ok(id)
    }

    /// Write a terminal state once, even if two connections race to finish.
    pub fn finish_turn(
        &mut self,
        id: TurnId,
        terminal: TerminalTurn<'_>,
    ) -> Result<(), HistoryError> {
        if id <= 0 {
            return Err(HistoryError::InvalidInput);
        }
        let (state, assistant_text, failure_code) = match terminal {
            TerminalTurn::Completed { assistant_text } => {
                if assistant_text.len() > MAX_ASSISTANT_TEXT_BYTES || assistant_text.contains('\0')
                {
                    return Err(HistoryError::InvalidInput);
                }
                ("completed", Some(assistant_text), None)
            }
            TerminalTurn::Failed { code } => ("failed", None, Some(code.as_str())),
            TerminalTurn::Cancelled => ("cancelled", None, None),
            TerminalTurn::Interrupted => ("interrupted", None, None),
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        let chat_id: Option<String> = transaction
            .query_row(
                "UPDATE turns SET state = ?2, assistant_text = ?3, failure_code = ?4,
                              finished_at_ms = max(started_at_ms, ?5)
             WHERE id = ?1 AND state = 'active' RETURNING chat_id",
                params![id, state, assistant_text, failure_code, now],
                |row| row.get(0),
            )
            .optional()?;
        let Some(chat_id) = chat_id else {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM turns WHERE id = ?1)",
                [id],
                |row| row.get(0),
            )?;
            return Err(if exists {
                HistoryError::TurnAlreadyFinished
            } else {
                HistoryError::TurnNotFound
            });
        };
        update_chat_activity(&transaction, &chat_id, now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Retire all abandoned active turns in one transaction. Call only at an
    /// exclusive host startup, before any runs may be live or start.
    pub fn recover_interrupted(&mut self) -> Result<usize, HistoryError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        transaction.execute(
            "UPDATE chats SET updated_at_ms = max(updated_at_ms, ?1)
             WHERE id IN (SELECT chat_id FROM turns WHERE state = 'active')",
            [now],
        )?;
        let interrupted = transaction.execute(
            "UPDATE turns SET state = 'interrupted', finished_at_ms = max(started_at_ms, ?1)
             WHERE state = 'active'",
            [now],
        )?;
        transaction.commit()?;
        Ok(interrupted)
    }
}

fn validate_id(id: &str) -> Result<(), HistoryError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(HistoryError::InvalidInput);
    }
    Ok(())
}

fn validate_user_text(text: &str) -> Result<(), HistoryError> {
    if text.len() > MAX_USER_TEXT_BYTES || text.contains('\0') || text.trim().is_empty() {
        return Err(HistoryError::InvalidInput);
    }
    Ok(())
}

fn insert_chat(
    connection: &Connection,
    id: &str,
    engine: Engine,
    now: i64,
) -> Result<bool, HistoryError> {
    let inserted = connection.execute(
        "INSERT INTO chats(id, engine, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(id) DO NOTHING",
        params![id, engine.as_str(), now],
    )?;
    Ok(inserted == 1)
}

fn chat_engine(connection: &Connection, id: &str) -> Result<Engine, HistoryError> {
    let engine: String = connection
        .query_row("SELECT engine FROM chats WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(HistoryError::ChatNotFound)?;
    Engine::from_str(&engine)
}

fn insert_turn(
    transaction: &rusqlite::Transaction<'_>,
    chat_id: &str,
    user_text: &str,
    now: i64,
) -> Result<TurnId, HistoryError> {
    let id = transaction.query_row(
        "INSERT INTO turns(chat_id, user_text, state, started_at_ms) VALUES (?1, ?2, 'active', ?3)
         ON CONFLICT(chat_id) WHERE state = 'active' DO NOTHING RETURNING id",
        params![chat_id, user_text, now], |row| row.get(0),
    ).optional()?.ok_or(HistoryError::ActiveTurn)?;
    transaction.execute(
        "UPDATE chats SET updated_at_ms = max(updated_at_ms, ?2),
                          title = CASE WHEN title = '' THEN ?3 ELSE title END WHERE id = ?1",
        params![chat_id, now, chat_title(user_text)],
    )?;
    Ok(id)
}

fn chat_title(user_text: &str) -> String {
    let mut title: String = user_text
        .split_whitespace()
        .flat_map(|word| std::iter::once(' ').chain(word.chars()))
        .skip(1)
        .take(MAX_TITLE_CHARS)
        .collect();
    if title.ends_with(' ') {
        title.pop();
    }
    title
}

fn now_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

fn update_chat_activity(connection: &Connection, id: &str, now: i64) -> Result<(), HistoryError> {
    connection.execute(
        "UPDATE chats SET updated_at_ms = max(updated_at_ms, ?2) WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

type SummaryRow = (String, String, String, i64, i64);

fn summary_row(row: &Row<'_>) -> rusqlite::Result<SummaryRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn decode_summary(
    (id, engine, title, created_at_ms, updated_at_ms): SummaryRow,
) -> Result<ChatSummary, HistoryError> {
    Ok(ChatSummary {
        id,
        engine: Engine::from_str(&engine)?,
        title,
        created_at_ms,
        updated_at_ms,
    })
}

type TurnRow = (
    TurnId,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<i64>,
);

fn decode_turn(row: TurnRow) -> Result<Turn, HistoryError> {
    let (id, user_text, state, text, failure, started_at_ms, finished_at_ms) = row;
    let state = match (state.as_str(), text, failure, finished_at_ms) {
        ("active", None, None, None) => TurnState::Active,
        ("completed", Some(assistant_text), None, Some(_)) => {
            TurnState::Completed { assistant_text }
        }
        ("failed", None, Some(code), Some(_)) => TurnState::Failed {
            code: FailureCode::from_str(&code)?,
        },
        ("cancelled", None, None, Some(_)) => TurnState::Cancelled,
        ("interrupted", None, None, Some(_)) => TurnState::Interrupted,
        _ => return Err(HistoryError::Corrupt),
    };
    Ok(Turn {
        id,
        user_text,
        state,
        started_at_ms,
        finished_at_ms,
        actor_label_receipts: Vec::new(),
    })
}
