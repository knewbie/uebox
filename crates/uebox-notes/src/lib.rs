//! Standalone offline notebooks and text notes.
//!
//! The host supplies an explicit database path in an existing private local
//! directory. Notebook-owned notes are independent entries. A standalone note
//! has no parent and survives deletion of any notebook. Content is stored as
//! supplied, without interpreting Markdown, HTML, links, or local file paths.

use std::{fmt, path::Path};

use rusqlite::Transaction;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Row, TransactionBehavior};

mod storage;

const MAX_TITLE_BYTES: usize = 1024;
const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAGE_SIZE: u32 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotesError {
    InvalidInput,
    Missing,
    AlreadyExists,
    Unsupported,
    Corrupt,
    RecoveryRequired,
    Locked,
    NotebookMissing,
    ImportConflict,
    Io,
    Database,
}

impl NotesError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Missing => "missing",
            Self::AlreadyExists => "already_exists",
            Self::Unsupported => "unsupported",
            Self::Corrupt => "corrupt",
            Self::RecoveryRequired => "recovery_required",
            Self::Locked => "locked",
            Self::NotebookMissing => "notebook_missing",
            Self::ImportConflict => "import_conflict",
            Self::Io => "io",
            Self::Database => "database",
        }
    }
}

impl fmt::Display for NotesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for NotesError {}

impl From<rusqlite::Error> for NotesError {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            rusqlite::Error::SqliteFailure(error, _) => match error.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Self::Locked,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notebook {
    pub id: i64,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteSummary {
    pub id: i64,
    pub notebook_id: Option<i64>,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note {
    pub id: i64,
    pub notebook_id: Option<i64>,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy)]
pub struct LegacyNotebook<'a> {
    pub identity: &'a str,
    pub title: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct LegacyNote<'a> {
    pub identity: &'a str,
    pub notebook_identity: Option<&'a str>,
    pub title: &'a str,
    pub content: &'a str,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoteImportSummary {
    pub notebooks_inserted: u32,
    pub notebooks_existing: u32,
    pub notes_inserted: u32,
    pub notes_existing: u32,
}

#[derive(Debug)]
pub struct NotesStore {
    connection: Connection,
}

impl NotesStore {
    /// Create and publish a fresh database without replacing an existing file.
    pub fn create_new(path: impl AsRef<Path>) -> Result<Self, NotesError> {
        Ok(Self {
            connection: storage::create_new(path.as_ref())?,
        })
    }

    /// Validate a database with a read-only connection before permitting writes.
    /// Open between transactions. An existing rollback journal is left in place.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self, NotesError> {
        Ok(Self {
            connection: storage::open_existing(path.as_ref())?,
        })
    }

    /// Import one validated legacy batch atomically. Retries with identical source identities and fields are idempotent.
    pub fn import_legacy_batch(
        &mut self,
        source: &str,
        notebooks: &[LegacyNotebook<'_>],
        notes: &[LegacyNote<'_>],
    ) -> Result<NoteImportSummary, NotesError> {
        validate_text(source, 64)?;
        if source.is_empty() {
            return Err(NotesError::InvalidInput);
        }
        for notebook in notebooks {
            validate_text(notebook.identity, 256)?;
            validate_text(notebook.title, MAX_TITLE_BYTES)?;
            if notebook.identity.is_empty() {
                return Err(NotesError::InvalidInput);
            }
        }
        for note in notes {
            validate_text(note.identity, 256)?;
            validate_text(note.title, MAX_TITLE_BYTES)?;
            validate_text(note.content, MAX_CONTENT_BYTES)?;
            if note.identity.is_empty() {
                return Err(NotesError::InvalidInput);
            }
        }

        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut summary = NoteImportSummary::default();
        for notebook in notebooks {
            let fingerprint = fingerprint(&[notebook.title]);
            let existing = tx.query_row(
                "SELECT record_id, fingerprint FROM legacy_identities WHERE source=?1 AND identity=?2",
                params![format!("{source}:notebook"), notebook.identity],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            ).optional()?;
            if let Some((_, stored)) = existing {
                if stored != fingerprint.as_slice() {
                    return Err(NotesError::ImportConflict);
                }
                summary.notebooks_existing += 1;
                continue;
            }
            tx.execute("INSERT INTO notebooks(title) VALUES (?1)", [notebook.title])?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO legacy_identities(source, identity, record_id, fingerprint) VALUES (?1, ?2, ?3, ?4)",
                params![format!("{source}:notebook"), notebook.identity, id, fingerprint],
            )?;
            summary.notebooks_inserted += 1;
        }

        for note in notes {
            let fingerprint = fingerprint(&[
                note.notebook_identity.unwrap_or(""),
                note.title,
                note.content,
            ]);
            let existing = tx.query_row(
                "SELECT record_id, fingerprint FROM legacy_identities WHERE source=?1 AND identity=?2",
                params![format!("{source}:note"), note.identity],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            ).optional()?;
            if let Some((_, stored)) = existing {
                if stored != fingerprint.as_slice() {
                    return Err(NotesError::ImportConflict);
                }
                summary.notes_existing += 1;
                continue;
            }
            let parent = match note.notebook_identity {
                Some(identity) => Some(legacy_parent(&tx, source, identity)?),
                None => None,
            };
            tx.execute(
                "INSERT INTO notes(notebook_id,title,content) VALUES (?1,?2,?3)",
                params![parent, note.title, note.content],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO legacy_identities(source, identity, record_id, fingerprint) VALUES (?1, ?2, ?3, ?4)",
                params![format!("{source}:note"), note.identity, id, fingerprint],
            )?;
            summary.notes_inserted += 1;
        }
        tx.commit()?;
        Ok(summary)
    }

    pub fn create_notebook(&mut self, title: &str) -> Result<i64, NotesError> {
        validate_text(title, MAX_TITLE_BYTES)?;
        self.connection
            .execute("INSERT INTO notebooks(title) VALUES (?1)", [title])?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn get_notebook(&self, id: i64) -> Result<Option<Notebook>, NotesError> {
        validate_id(id)?;
        Ok(self
            .connection
            .query_row(
                "SELECT id, title FROM notebooks WHERE id = ?1",
                [id],
                notebook_row,
            )
            .optional()?)
    }

    /// Page in increasing ID order with an exclusive cursor and a limit of 1–100.
    pub fn list_notebooks(
        &self,
        after_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<Notebook>, NotesError> {
        let after_id = page_start(after_id, limit)?;
        let mut statement = self
            .connection
            .prepare("SELECT id, title FROM notebooks WHERE id > ?1 ORDER BY id LIMIT ?2")?;
        let rows = statement.query_map(params![after_id, limit], notebook_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_notebook(&mut self, id: i64, title: &str) -> Result<bool, NotesError> {
        validate_id(id)?;
        validate_text(title, MAX_TITLE_BYTES)?;
        Ok(self.connection.execute(
            "UPDATE notebooks SET title = ?1 WHERE id = ?2",
            params![title, id],
        )? != 0)
    }

    /// Cascade to notes owned by this notebook.
    pub fn delete_notebook(&mut self, id: i64) -> Result<bool, NotesError> {
        validate_id(id)?;
        Ok(self
            .connection
            .execute("DELETE FROM notebooks WHERE id = ?1", [id])?
            != 0)
    }

    /// Create a standalone note (None) or a note owned by an existing notebook.
    pub fn create_note(
        &mut self,
        notebook_id: Option<i64>,
        title: &str,
        content: &str,
    ) -> Result<i64, NotesError> {
        if let Some(id) = notebook_id {
            validate_id(id)?;
        }
        validate_text(title, MAX_TITLE_BYTES)?;
        validate_text(content, MAX_CONTENT_BYTES)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction.execute(
            "INSERT INTO notes(notebook_id, title, content)
             SELECT ?1, ?2, ?3 WHERE ?1 IS NULL OR EXISTS (SELECT 1 FROM notebooks WHERE id = ?1)",
            params![notebook_id, title, content],
        )? == 0
        {
            return Err(NotesError::NotebookMissing);
        }
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(id)
    }

    pub fn get_note(&self, id: i64) -> Result<Option<Note>, NotesError> {
        validate_id(id)?;
        Ok(self
            .connection
            .query_row(
                "SELECT id, notebook_id, title, content FROM notes WHERE id = ?1",
                [id],
                |row| {
                    Ok(Note {
                        id: row.get(0)?,
                        notebook_id: row.get(1)?,
                        title: row.get(2)?,
                        content: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// List one notebook's note summaries, or the standalone collection (None).
    /// The cursor is exclusive and a page never includes note bodies.
    pub fn list_notes(
        &self,
        notebook_id: Option<i64>,
        after_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<NoteSummary>, NotesError> {
        if let Some(id) = notebook_id {
            validate_id(id)?;
        }
        let after_id = page_start(after_id, limit)?;
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(id) = notebook_id {
            let exists = transaction.query_row(
                "SELECT EXISTS (SELECT 1 FROM notebooks WHERE id = ?1)",
                [id],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(NotesError::NotebookMissing);
            }
        }
        let summaries = {
            let mut statement = transaction.prepare(
                "SELECT id, notebook_id, title FROM notes
                 WHERE notebook_id IS ?1 AND id > ?2 ORDER BY id LIMIT ?3",
            )?;
            let rows = statement.query_map(params![notebook_id, after_id, limit], |row| {
                Ok(NoteSummary {
                    id: row.get(0)?,
                    notebook_id: row.get(1)?,
                    title: row.get(2)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        transaction.commit()?;
        Ok(summaries)
    }

    /// Replace the visible fields while retaining the note's ID and parent.
    pub fn update_note(&mut self, id: i64, title: &str, content: &str) -> Result<bool, NotesError> {
        validate_id(id)?;
        validate_text(title, MAX_TITLE_BYTES)?;
        validate_text(content, MAX_CONTENT_BYTES)?;
        Ok(self.connection.execute(
            "UPDATE notes SET title = ?1, content = ?2 WHERE id = ?3",
            params![title, content, id],
        )? != 0)
    }

    pub fn delete_note(&mut self, id: i64) -> Result<bool, NotesError> {
        validate_id(id)?;
        Ok(self
            .connection
            .execute("DELETE FROM notes WHERE id = ?1", [id])?
            != 0)
    }
}

fn fingerprint(fields: &[&str]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest.finalize().into()
}

fn legacy_parent(tx: &Transaction<'_>, source: &str, identity: &str) -> Result<i64, NotesError> {
    tx.query_row(
        "SELECT record_id FROM legacy_identities WHERE source=?1 AND identity=?2",
        params![format!("{source}:notebook"), identity],
        |row| row.get(0),
    )
    .optional()?
    .ok_or(NotesError::NotebookMissing)
}

fn notebook_row(row: &Row<'_>) -> rusqlite::Result<Notebook> {
    Ok(Notebook {
        id: row.get(0)?,
        title: row.get(1)?,
    })
}

fn validate_id(id: i64) -> Result<(), NotesError> {
    if id <= 0 {
        return Err(NotesError::InvalidInput);
    }
    Ok(())
}

fn page_start(after_id: Option<i64>, limit: u32) -> Result<i64, NotesError> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(NotesError::InvalidInput);
    }
    if let Some(id) = after_id {
        validate_id(id)?;
    }
    Ok(after_id.unwrap_or(0))
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), NotesError> {
    if value.len() > max_bytes || value.contains('\0') {
        return Err(NotesError::InvalidInput);
    }
    Ok(())
}
