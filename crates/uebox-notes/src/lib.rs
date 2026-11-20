//! Standalone offline notebooks and notes, with per-note content format and
//! content-addressed attachments.
//!
//! The host supplies an explicit database path in an existing private local
//! directory. Notebook-owned notes are independent entries. A standalone note
//! has no parent and survives deletion of any notebook. Content is stored as
//! supplied; a per-note format (`markdown` / `html` / `plain`) tells the caller
//! how to render it, and HTML is never treated as trusted. Attachments are
//! stored as binary payloads or HTML fragments, distinguished explicitly.

use std::{fmt, path::Path};

use rusqlite::Transaction;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Row, TransactionBehavior};

mod electron;
mod storage;

pub use electron::{
    ElectronNote, ElectronNotebook, ElectronNotes, ElectronNotesSnapshot, ElectronOriginalFile,
    ElectronSourceKind,
};

const MAX_TITLE_BYTES: usize = 1024;
const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ATTACHMENTS_PER_NOTE: usize = 256;
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
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
    LimitExceeded,
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
            Self::LimitExceeded => "limit_exceeded",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteFormat {
    Markdown,
    Html,
    Plain,
}

impl NoteFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Plain => "plain",
        }
    }

    pub fn from_name(value: &str) -> Option<Self> {
        match value {
            "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            "plain" => Some(Self::Plain),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Binary,
    Html,
}

impl AttachmentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Html => "html",
        }
    }

    pub fn from_name(value: &str) -> Option<Self> {
        match value {
            "binary" => Some(Self::Binary),
            "html" => Some(Self::Html),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentSummary {
    pub id: i64,
    pub kind: AttachmentKind,
    pub media_type: Option<String>,
    pub file_name: Option<String>,
    pub sha256: [u8; 32],
    pub position: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    pub summary: AttachmentSummary,
    pub bytes: Vec<u8>,
}

/// A safe summary of one original file: no external URL or local path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginalFileSummary {
    pub id: i64,
    pub identity: String,
    pub kind: ElectronSourceKind,
    pub title: String,
    pub file_name: Option<String>,
}

/// The safe text body of one original file, fetched explicitly by ID.
/// External URLs and local paths are intentionally absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginalFileBody {
    pub id: i64,
    pub identity: String,
    pub kind: ElectronSourceKind,
    pub title: String,
    pub file_name: Option<String>,
    pub content: String,
    pub raw_content: Option<String>,
}

/// Version 2 handoff: the explicitly selected transfer document between the
/// Electron export (or the read-only `ElectronNotes` reader) and this store.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct V2Handoff {
    pub format: String,
    pub version: u32,
    pub source: String,
    pub notebooks: Vec<V2Notebook>,
    pub notes: Vec<V2Note>,
    #[serde(default, rename = "originalFiles")]
    pub original_files: Vec<V2OriginalFile>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct V2Notebook {
    pub identity: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct V2Note {
    pub identity: String,
    pub notebook_identity: Option<String>,
    pub title: String,
    pub content: String,
    pub format: NoteFormat,
    #[serde(default)]
    pub attachments: Vec<V2Attachment>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct V2Attachment {
    pub kind: AttachmentKind,
    pub media_type: Option<String>,
    pub file_name: Option<String>,
    /// Base64-encoded payload.
    pub bytes: String,
    /// 64 lowercase hex characters, the SHA-256 of `bytes`.
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct V2OriginalFile {
    pub identity: String,
    pub notebook_identity: String,
    pub kind: ElectronSourceKind,
    pub title: String,
    pub content: String,
    pub raw_content: Option<String>,
    pub source_url: Option<String>,
    pub file_name: Option<String>,
    pub file_path: Option<String>,
    pub media_url: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct V2ImportSummary {
    pub notebooks_inserted: u32,
    pub notebooks_existing: u32,
    pub notes_inserted: u32,
    pub notes_existing: u32,
    pub attachments_inserted: u32,
    pub original_files_inserted: u32,
    pub original_files_existing: u32,
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

    /// Import one validated v2 handoff atomically. Retries with identical
    /// source identities and fields are idempotent; changed content conflicts.
    /// Per-note format, content-addressed attachments, and original-file
    /// provenance are stored together with the note.
    pub fn import_v2(&mut self, handoff: &V2Handoff) -> Result<V2ImportSummary, NotesError> {
        if handoff.format != "uebox-notes" || handoff.version != 2 {
            return Err(NotesError::InvalidInput);
        }
        validate_text(&handoff.source, 64)?;
        if handoff.source.is_empty() {
            return Err(NotesError::InvalidInput);
        }
        if handoff.notebooks.len() > 4096
            || handoff.notes.len() > 16_384
            || handoff.original_files.len() > 16_384
        {
            return Err(NotesError::LimitExceeded);
        }
        for notebook in &handoff.notebooks {
            validate_text(&notebook.identity, 256)?;
            validate_text(&notebook.title, MAX_TITLE_BYTES)?;
            if notebook.identity.is_empty() {
                return Err(NotesError::InvalidInput);
            }
        }
        for note in &handoff.notes {
            validate_text(&note.identity, 256)?;
            validate_text(&note.title, MAX_TITLE_BYTES)?;
            validate_text(&note.content, MAX_CONTENT_BYTES)?;
            if note.identity.is_empty() {
                return Err(NotesError::InvalidInput);
            }
            if note.attachments.len() > MAX_ATTACHMENTS_PER_NOTE {
                return Err(NotesError::LimitExceeded);
            }
            for attachment in &note.attachments {
                if let Some(media_type) = &attachment.media_type {
                    validate_text(media_type, 256)?;
                }
                if let Some(file_name) = &attachment.file_name {
                    validate_text(file_name, 1024)?;
                }
                let expected = decode_hex(&attachment.sha256)?;
                let bytes = STANDARD
                    .decode(&attachment.bytes)
                    .map_err(|_| NotesError::InvalidInput)?;
                if bytes.len() > MAX_ATTACHMENT_BYTES {
                    return Err(NotesError::LimitExceeded);
                }
                if STANDARD.encode(&bytes) != attachment.bytes || sha256_bytes(&bytes) != expected {
                    return Err(NotesError::InvalidInput);
                }
            }
        }
        for file in &handoff.original_files {
            validate_text(&file.identity, 256)?;
            validate_text(&file.notebook_identity, 256)?;
            validate_text(&file.title, MAX_TITLE_BYTES)?;
            validate_text(&file.content, MAX_CONTENT_BYTES)?;
            if file.identity.is_empty() || file.notebook_identity.is_empty() {
                return Err(NotesError::InvalidInput);
            }
            if let Some(raw) = &file.raw_content {
                validate_text(raw, MAX_CONTENT_BYTES)?;
            }
            if let Some(url) = &file.source_url {
                validate_text(url, 4096)?;
            }
            if let Some(name) = &file.file_name {
                validate_text(name, 1024)?;
            }
            if let Some(path) = &file.file_path {
                validate_text(path, 4096)?;
            }
            if let Some(url) = &file.media_url {
                validate_text(url, 4096)?;
            }
        }

        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut summary = V2ImportSummary::default();

        for notebook in &handoff.notebooks {
            let fingerprint = fingerprint(&[&notebook.title]);
            let existing = tx
                .query_row(
                    "SELECT record_id, fingerprint FROM legacy_identities WHERE source=?1 AND identity=?2",
                    params![format!("{}:notebook", handoff.source), notebook.identity],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            if let Some((_, stored)) = existing {
                if stored != fingerprint.as_slice() {
                    return Err(NotesError::ImportConflict);
                }
                summary.notebooks_existing += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO notebooks(title) VALUES (?1)",
                [&notebook.title],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO legacy_identities(source, identity, record_id, fingerprint) VALUES (?1, ?2, ?3, ?4)",
                params![format!("{}:notebook", handoff.source), notebook.identity, id, fingerprint.as_slice()],
            )?;
            summary.notebooks_inserted += 1;
        }

        for note in &handoff.notes {
            let v1_fingerprint = note_fingerprint_v1(note);
            let v2_fingerprint = note_fingerprint_v2(note);
            let existing = tx
                .query_row(
                    "SELECT record_id, fingerprint FROM legacy_identities WHERE source=?1 AND identity=?2",
                    params![format!("{}:note", handoff.source), note.identity],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            if let Some((record_id, stored)) = existing {
                if stored == v2_fingerprint.as_slice() {
                    summary.notes_existing += 1;
                    continue;
                }
                if stored == v1_fingerprint.as_slice() {
                    // A v1-imported note with identical content: enrich it with
                    // the v2 format and attachments while preserving its id.
                    tx.execute(
                        "INSERT INTO note_formats(note_id, format) VALUES (?1, ?2)",
                        params![record_id, note.format.as_str()],
                    )?;
                    summary.attachments_inserted += insert_attachments(&tx, record_id, note)?;
                    tx.execute(
                        "UPDATE legacy_identities SET fingerprint = ?1 WHERE source = ?2 AND identity = ?3",
                        params![v2_fingerprint.as_slice(), format!("{}:note", handoff.source), note.identity],
                    )?;
                    summary.notes_existing += 1;
                    continue;
                }
                return Err(NotesError::ImportConflict);
            }
            let parent = match &note.notebook_identity {
                Some(identity) => Some(legacy_parent(&tx, &handoff.source, identity)?),
                None => None,
            };
            tx.execute(
                "INSERT INTO notes(notebook_id, title, content) VALUES (?1, ?2, ?3)",
                params![parent, &note.title, &note.content],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO legacy_identities(source, identity, record_id, fingerprint) VALUES (?1, ?2, ?3, ?4)",
                params![format!("{}:note", handoff.source), note.identity, id, v2_fingerprint.as_slice()],
            )?;
            tx.execute(
                "INSERT INTO note_formats(note_id, format) VALUES (?1, ?2)",
                params![id, note.format.as_str()],
            )?;
            summary.attachments_inserted += insert_attachments(&tx, id, note)?;
            summary.notes_inserted += 1;
        }

        for file in &handoff.original_files {
            let fingerprint = fingerprint(&[
                &file.notebook_identity,
                file.kind.as_str(),
                &file.title,
                &file.content,
                file.raw_content.as_deref().unwrap_or(""),
                file.source_url.as_deref().unwrap_or(""),
                file.file_name.as_deref().unwrap_or(""),
                file.file_path.as_deref().unwrap_or(""),
                file.media_url.as_deref().unwrap_or(""),
            ]);
            let existing = tx
                .query_row(
                    "SELECT fingerprint FROM original_files WHERE source=?1 AND identity=?2",
                    params![handoff.source, file.identity],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?;
            if let Some(stored) = existing {
                if stored != fingerprint.as_slice() {
                    return Err(NotesError::ImportConflict);
                }
                summary.original_files_existing += 1;
                continue;
            }
            let notebook_id = legacy_parent(&tx, &handoff.source, file.notebook_identity.as_str())?;
            tx.execute(
                "INSERT INTO original_files(source, identity, notebook_id, notebook_identity, kind, title, content, raw_content, source_url, file_name, file_path, media_url, fingerprint) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    handoff.source,
                    file.identity,
                    notebook_id,
                    file.notebook_identity.as_str(),
                    file.kind.as_str(),
                    file.title.as_str(),
                    file.content.as_str(),
                    file.raw_content.as_deref(),
                    file.source_url.as_deref(),
                    file.file_name.as_deref(),
                    file.file_path.as_deref(),
                    file.media_url.as_deref(),
                    fingerprint.as_slice(),
                ],
            )?;
            summary.original_files_inserted += 1;
        }

        tx.commit()?;
        Ok(summary)
    }

    /// Parse and import a v2 handoff JSON document.
    pub fn import_v2_json(&mut self, manifest: &str) -> Result<V2ImportSummary, NotesError> {
        if manifest.len() > MAX_MANIFEST_BYTES {
            return Err(NotesError::LimitExceeded);
        }
        let handoff: V2Handoff =
            serde_json::from_str(manifest).map_err(|_| NotesError::InvalidInput)?;
        self.import_v2(&handoff)
    }

    /// The per-note content format, or `None` when the note predates formats
    /// and should be treated as Markdown.
    pub fn note_format(&self, id: i64) -> Result<Option<NoteFormat>, NotesError> {
        validate_id(id)?;
        let format: Option<String> = self
            .connection
            .query_row(
                "SELECT format FROM note_formats WHERE note_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        match format {
            None => Ok(None),
            Some(value) => NoteFormat::from_name(&value)
                .map(Some)
                .ok_or(NotesError::Corrupt),
        }
    }

    /// Metadata for one note's attachments, ordered by position.
    pub fn note_attachments(&self, note_id: i64) -> Result<Vec<AttachmentSummary>, NotesError> {
        validate_id(note_id)?;
        let mut statement = self.connection.prepare(
            "SELECT id, kind, media_type, file_name, sha256, position FROM note_attachments WHERE note_id = ?1 ORDER BY position, id",
        )?;
        let rows = statement.query_map([note_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (id, kind, media_type, file_name, sha, position) = row?;
            let kind = AttachmentKind::from_name(&kind).ok_or(NotesError::Corrupt)?;
            let sha: [u8; 32] = sha.try_into().map_err(|_| NotesError::Corrupt)?;
            items.push(AttachmentSummary {
                id,
                kind,
                media_type,
                file_name,
                sha256: sha,
                position: position as u32,
            });
        }
        Ok(items)
    }

    /// One attachment including its bytes, without re-verifying the stored hash.
    pub fn get_attachment(&self, id: i64) -> Result<Option<Attachment>, NotesError> {
        validate_id(id)?;
        let row = self
            .connection
            .query_row(
                "SELECT id, kind, media_type, file_name, sha256, position, bytes FROM note_attachments WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Vec<u8>>(6)?,
                    ))
                },
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((id, kind, media_type, file_name, sha, position, bytes)) => {
                let kind = AttachmentKind::from_name(&kind).ok_or(NotesError::Corrupt)?;
                let sha: [u8; 32] = sha.try_into().map_err(|_| NotesError::Corrupt)?;
                Ok(Some(Attachment {
                    summary: AttachmentSummary {
                        id,
                        kind,
                        media_type,
                        file_name,
                        sha256: sha,
                        position: position as u32,
                    },
                    bytes,
                }))
            }
        }
    }

    /// Safe summaries of one registered notebook's original files, paged in
    /// insertion order with an exclusive ID cursor. External URLs and local
    /// paths are never returned.
    pub fn list_original_files(
        &self,
        notebook_id: i64,
        after_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<OriginalFileSummary>, NotesError> {
        validate_id(notebook_id)?;
        let after_id = page_start(after_id, limit)?;
        let mut statement = self.connection.prepare(
            "SELECT id, identity, kind, title, file_name FROM original_files
             WHERE notebook_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        let rows = statement.query_map(params![notebook_id, after_id, limit], |row| {
            Ok(OriginalFileSummary {
                id: row.get(0)?,
                identity: row.get(1)?,
                kind: ElectronSourceKind::from_name(&row.get::<_, String>(2)?),
                title: row.get(3)?,
                file_name: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The safe text body of one original file, fetched explicitly by ID.
    pub fn get_original_file(
        &self,
        notebook_id: i64,
        id: i64,
    ) -> Result<Option<OriginalFileBody>, NotesError> {
        validate_id(notebook_id)?;
        validate_id(id)?;
        let row = self
            .connection
            .query_row(
                "SELECT id, identity, kind, title, file_name, content, raw_content FROM original_files WHERE notebook_id = ?1 AND id = ?2",
                params![notebook_id, id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((id, identity, kind, title, file_name, content, raw_content)) => {
                Ok(Some(OriginalFileBody {
                    id,
                    identity,
                    kind: ElectronSourceKind::from_name(&kind),
                    title,
                    file_name,
                    content,
                    raw_content,
                }))
            }
        }
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

/// The legacy v1 note fingerprint: notebook identity, title, and content only.
fn note_fingerprint_v1(note: &V2Note) -> [u8; 32] {
    fingerprint(&[
        note.notebook_identity.as_deref().unwrap_or(""),
        &note.title,
        &note.content,
    ])
}

/// The v2 note fingerprint: the v1 fields plus format and, in order, each
/// attachment's kind, media type, file name, and SHA-256 digest.
fn note_fingerprint_v2(note: &V2Note) -> [u8; 32] {
    let mut fields: Vec<&str> = Vec::with_capacity(4 + note.attachments.len() * 4);
    fields.push(note.notebook_identity.as_deref().unwrap_or(""));
    fields.push(&note.title);
    fields.push(&note.content);
    fields.push(note.format.as_str());
    for attachment in &note.attachments {
        fields.push(attachment.kind.as_str());
        fields.push(attachment.media_type.as_deref().unwrap_or(""));
        fields.push(attachment.file_name.as_deref().unwrap_or(""));
        fields.push(&attachment.sha256);
    }
    fingerprint(fields.as_slice())
}

fn insert_attachments(
    tx: &Transaction<'_>,
    note_id: i64,
    note: &V2Note,
) -> Result<u32, NotesError> {
    let mut inserted = 0u32;
    for (position, attachment) in note.attachments.iter().enumerate() {
        let bytes = STANDARD
            .decode(&attachment.bytes)
            .map_err(|_| NotesError::InvalidInput)?;
        let sha = decode_hex(&attachment.sha256)?;
        tx.execute(
            "INSERT INTO note_attachments(note_id, kind, media_type, file_name, bytes, sha256, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                note_id,
                attachment.kind.as_str(),
                attachment.media_type.as_deref(),
                attachment.file_name.as_deref(),
                bytes,
                sha.as_slice(),
                position as u32,
            ],
        )?;
        inserted += 1;
    }
    Ok(inserted)
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

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    Sha256::digest(data).into()
}

fn decode_hex(value: &str) -> Result<[u8; 32], NotesError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(NotesError::InvalidInput);
    }
    let mut output = [0u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        let high = hex_nibble(value.as_bytes()[index * 2]);
        let low = hex_nibble(value.as_bytes()[index * 2 + 1]);
        *byte = (high << 4) | low;
    }
    Ok(output)
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}
