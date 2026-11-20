//! Imports only the visible v1 Electron chat-history export into a separate archive.
//!
//! The host supplies a fixed database path in an existing private, local profile
//! directory and holds its cross-process lock until the archive handle is dropped.
//! The archive has no journal recovery entry point. Existing rollback journals
//! produce `recovery_required`, and WAL or SHM sidecars produce `unsupported`.
//! An empty import or a read of a missing database leaves it absent.
//!
//! The source path, if provided, is the explicitly selected export file. Values
//! inside the manifest are data and never become paths. Image checks verify the
//! exporter’s signatures and SHA-256 without decoding image pixels.
use std::{
    fmt,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, ErrorCode};

mod manifest;
mod storage;
mod store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryError {
    InvalidInput,
    InvalidManifest,
    LimitExceeded,
    Missing,
    AlreadyExists,
    Unsupported,
    Corrupt,
    RecoveryRequired,
    Conflict,
    Busy,
    Io,
    Database,
}

#[derive(Debug, Clone, Copy)]
pub enum ManifestSource<'a> {
    Path(&'a Path),
    Bytes(&'a [u8]),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub inserted: u32,
    pub already_present: u32,
    /// Counts images in the validated input, including already-present snapshots.
    pub embedded_attachments: u32,
    /// Counts unavailable attachments in the same input.
    pub unavailable_attachments: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    /// Opaque archive snapshot ID used by `get_chat`.
    pub id: String,
    /// Original Electron session ID. Multiple snapshots may share it.
    pub sid: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDetail {
    pub summary: ChatSummary,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub text: String,
    pub status: MessageStatus,
    pub attachments: Vec<AttachmentMetadata>,
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
        pub enum $name {
            $( #[serde(rename = $wire)] $variant ),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }

            fn from_db(value: &str) -> Result<Self, HistoryError> {
                match value {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(HistoryError::Corrupt),
                }
            }
        }
    };
}

wire_enum!(MessageRole { User => "user", Assistant => "assistant" });
wire_enum!(MessageStatus {
    Completed => "completed", Failed => "failed", Cancelled => "cancelled", Interrupted => "interrupted",
});
wire_enum!(AttachmentKind {
    Image => "image", File => "file", Excel => "excel", Document => "document", Video => "video", Audio => "audio",
});
wire_enum!(Availability { Embedded => "embedded", Unavailable => "unavailable" });
wire_enum!(ImageMediaType {
    Png => "image/png", Jpeg => "image/jpeg", Webp => "image/webp", Gif => "image/gif",
});

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMetadata {
    pub kind: AttachmentKind,
    pub availability: Availability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<ImageMediaType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedAttachment {
    pub media_type: ImageMediaType,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct LegacyHistory {
    database: PathBuf,
    connection: Option<Connection>,
}

impl LegacyHistory {
    /// The host must own the archive directory and hold its lock for all access.
    /// This constructs a lazy handle and does not create or open the database.
    pub fn new(database: impl AsRef<Path>) -> Result<Self, HistoryError> {
        let database = database.as_ref();
        storage::validate_path(database)?;
        Ok(Self {
            database: database.to_path_buf(),
            connection: None,
        })
    }

    pub fn import_manifest(
        &mut self,
        source: ManifestSource<'_>,
    ) -> Result<ImportSummary, HistoryError> {
        let manifest = manifest::parse(source)?;
        if let Some(connection) = self.open()? {
            return store::import(connection, &manifest);
        }
        if manifest.chats.is_empty() {
            return Ok(manifest.summary);
        }

        storage::publish_initial(&self.database, &manifest)
    }

    /// Pages contain 1–100 summaries, most recently updated first.
    pub fn list_chats(
        &mut self,
        limit: u32,
        offset: u64,
    ) -> Result<Vec<ChatSummary>, HistoryError> {
        if !(1..=100).contains(&limit) {
            return Err(HistoryError::InvalidInput);
        }
        let offset = i64::try_from(offset).map_err(|_| HistoryError::InvalidInput)?;
        match self.open()? {
            Some(connection) => store::list_chats(connection, limit, offset),
            None => Ok(Vec::new()),
        }
    }

    /// Returns chat messages and attachment metadata without including image bytes.
    pub fn get_chat(&mut self, snapshot_id: &str) -> Result<Option<ChatDetail>, HistoryError> {
        if !snapshot_id
            .strip_prefix("s-")
            .is_some_and(manifest::is_sha256)
        {
            return Err(HistoryError::InvalidInput);
        }
        match self.open()? {
            Some(connection) => store::get_chat(connection, snapshot_id),
            None => Ok(None),
        }
    }

    /// Returns a single verified image of at most 2 MiB.
    pub fn get_attachment(
        &mut self,
        attachment_id: &str,
    ) -> Result<Option<EmbeddedAttachment>, HistoryError> {
        let digits = attachment_id
            .strip_prefix("a-")
            .ok_or(HistoryError::InvalidInput)?;
        if digits.is_empty()
            || digits.starts_with('0')
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(HistoryError::InvalidInput);
        }
        let id = digits
            .parse::<i64>()
            .map_err(|_| HistoryError::InvalidInput)?;
        match self.open()? {
            Some(connection) => store::get_attachment(connection, id),
            None => Ok(None),
        }
    }

    fn open(&mut self) -> Result<Option<&mut Connection>, HistoryError> {
        if self.connection.is_none() {
            self.connection = storage::open_existing(&self.database)?;
        }
        Ok(self.connection.as_mut())
    }
}

impl HistoryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::InvalidManifest => "invalid_manifest",
            Self::LimitExceeded => "limit_exceeded",
            Self::Missing => "missing",
            Self::AlreadyExists => "already_exists",
            Self::Unsupported => "unsupported",
            Self::Corrupt => "corrupt",
            Self::RecoveryRequired => "recovery_required",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
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
