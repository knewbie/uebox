//! Read-only, explicitly selected reader for the legacy Electron offline notes database.
//!
//! The host supplies an explicit absolute path to the Electron `app-data.db`.
//! The connection is opened read-only with `NOFOLLOW` and `query_only`, and the
//! expected tables and columns are checked before any row is read, so a wrong
//! or hostile file fails closed. No write, journal, or sidecar is ever created.
//!
//! Values read from the database are data, never paths: `file_name`, `file_path`,
//! `media_url`, and `source_url` are returned as opaque strings and are never
//! opened or followed. Content is returned verbatim — rich-text HTML and inline
//! attachments are not stripped, so the migration can decide how to preserve them.
//! Stable Electron identities (`notebook_id`, `source_id`, and `note:<id>`) are
//! retained unchanged.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::{NoteFormat, NotesError, V2Handoff, V2Note, V2Notebook, V2OriginalFile};

const MAX_NOTEBOOKS: usize = 4096;
const MAX_NOTES: usize = 16_384;
const MAX_ORIGINAL_FILES: usize = 16_384;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElectronNotebook {
    /// Original Electron `notebook_id`; a stable identity, not a path.
    pub identity: String,
    pub title: String,
    pub description: Option<String>,
    pub cover_image: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElectronNote {
    /// `note:<id>` for an independent note, `source:<source_id>` for a source.
    pub identity: String,
    pub notebook_identity: Option<String>,
    pub title: String,
    /// Verbatim body. Rich text is HTML and is not flattened.
    pub content: String,
    pub rich_text: bool,
    /// Count of inline `<img>` references, a signal for the attachment gap.
    pub inline_attachments: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ElectronSourceKind {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "link")]
    Link,
    #[serde(rename = "youtube")]
    Youtube,
    #[serde(rename = "bilibili")]
    Bilibili,
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "note")]
    Note,
    #[serde(rename = "ue-project")]
    UeProject,
    #[serde(rename = "wechat")]
    Wechat,
    #[serde(rename = "mp")]
    Mp,
    #[serde(rename = "unknown")]
    Unknown,
}

impl ElectronSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Link => "link",
            Self::Youtube => "youtube",
            Self::Bilibili => "bilibili",
            Self::Text => "text",
            Self::Note => "note",
            Self::UeProject => "ue-project",
            Self::Wechat => "wechat",
            Self::Mp => "mp",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_name(value: &str) -> Self {
        match value {
            "file" => Self::File,
            "link" => Self::Link,
            "youtube" => Self::Youtube,
            "bilibili" => Self::Bilibili,
            "text" => Self::Text,
            "note" => Self::Note,
            "ue-project" => Self::UeProject,
            "wechat" => Self::Wechat,
            "mp" => Self::Mp,
            _ => Self::Unknown,
        }
    }
}

/// A knowledge-base source that is not a note or text note: the original files
/// (PDFs, documents, links, media) the current note import does not yet carry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElectronOriginalFile {
    /// Original Electron `source_id`.
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ElectronNotesSnapshot {
    pub notebooks: Vec<ElectronNotebook>,
    pub notes: Vec<ElectronNote>,
    pub original_files: Vec<ElectronOriginalFile>,
}

pub struct ElectronNotes {
    connection: Connection,
}

impl ElectronNotes {
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, NotesError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(NotesError::InvalidInput);
        }
        // Resolve intermediate symlinks but keep the final component intact, so
        // `NOFOLLOW` still rejects a swapped file while opening succeeds on
        // platforms whose temp directory itself is a symlink (macOS /var).
        let parent = path.parent().ok_or(NotesError::InvalidInput)?;
        let name = path.file_name().ok_or(NotesError::InvalidInput)?;
        let parent = std::fs::canonicalize(parent).map_err(|_| NotesError::InvalidInput)?;
        let path = parent.join(name);
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.pragma_update(None, "query_only", true)?;
        // Force the SQLite header to be read so a non-database file fails closed
        // here rather than being misreported as an unsupported schema.
        connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })?;
        verify(&connection)?;
        Ok(Self { connection })
    }

    pub fn snapshot(&self) -> Result<ElectronNotesSnapshot, NotesError> {
        Ok(ElectronNotesSnapshot {
            notebooks: self.notebooks()?,
            notes: self.notes()?,
            original_files: self.original_files()?,
        })
    }

    pub fn notebooks(&self) -> Result<Vec<ElectronNotebook>, NotesError> {
        let columns = table_columns(&self.connection, "notebooks")?;
        let select = select_list(
            &columns,
            &[
                ("notebook_id", true),
                ("title", true),
                ("description", false),
                ("cover_image", false),
            ],
        );
        let mut statement = self.connection.prepare(&format!(
            "SELECT {select} FROM notebooks ORDER BY notebook_id LIMIT ?1"
        ))?;
        let rows = statement.query_map([(MAX_NOTEBOOKS + 1) as i64], |row| {
            Ok(ElectronNotebook {
                identity: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                cover_image: row.get(3)?,
            })
        })?;
        let items: Vec<_> = rows.collect::<rusqlite::Result<_>>()?;
        if items.len() > MAX_NOTEBOOKS {
            return Err(NotesError::LimitExceeded);
        }
        Ok(items)
    }

    /// Independent notes and note/text sources, in the stable identity form the
    /// importer expects. Neither list is flattened or otherwise transformed.
    pub fn notes(&self) -> Result<Vec<ElectronNote>, NotesError> {
        let mut notes = self.independent_notes()?;
        notes.extend(self.note_sources()?);
        if notes.len() > MAX_NOTES {
            return Err(NotesError::LimitExceeded);
        }
        Ok(notes)
    }

    pub fn original_files(&self) -> Result<Vec<ElectronOriginalFile>, NotesError> {
        let columns = table_columns(&self.connection, "notebook_sources")?;
        let select = select_list(
            &columns,
            &[
                ("source_id", true),
                ("notebook_id", true),
                ("type", true),
                ("title", true),
                ("content", true),
                ("raw_content", false),
                ("source_url", false),
                ("file_name", false),
                ("file_path", false),
                ("media_url", false),
            ],
        );
        let mut statement = self.connection.prepare(&format!(
            "SELECT {select} FROM notebook_sources
             WHERE type IS NULL OR type NOT IN ('note','text')
             ORDER BY source_id LIMIT ?1"
        ))?;
        let rows = statement.query_map([(MAX_ORIGINAL_FILES + 1) as i64], |row| {
            let kind = match row.get::<_, Option<String>>(2)? {
                Some(value) => ElectronSourceKind::from_name(&value),
                None => ElectronSourceKind::Unknown,
            };
            Ok(ElectronOriginalFile {
                identity: row.get(0)?,
                notebook_identity: row.get(1)?,
                kind,
                title: row.get(3)?,
                content: row.get(4)?,
                raw_content: row.get(5)?,
                source_url: row.get(6)?,
                file_name: row.get(7)?,
                file_path: row.get(8)?,
                media_url: row.get(9)?,
            })
        })?;
        let items: Vec<_> = rows.collect::<rusqlite::Result<_>>()?;
        if items.len() > MAX_ORIGINAL_FILES {
            return Err(NotesError::LimitExceeded);
        }
        Ok(items)
    }

    fn independent_notes(&self) -> Result<Vec<ElectronNote>, NotesError> {
        let columns = table_columns(&self.connection, "notes")?;
        let select = select_list(
            &columns,
            &[("id", true), ("title", true), ("content", true)],
        );
        let mut statement = self
            .connection
            .prepare(&format!("SELECT {select} FROM notes ORDER BY id LIMIT ?1"))?;
        let rows = statement.query_map([(MAX_NOTES + 1) as i64], |row| {
            let id: i64 = row.get(0)?;
            let title: String = row.get(1)?;
            let content: String = row.get(2)?;
            let rich_text = looks_like_html(&content);
            let inline_attachments = count_inline_images(&content);
            Ok(ElectronNote {
                identity: format!("note:{id}"),
                notebook_identity: None,
                title,
                content,
                rich_text,
                inline_attachments,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn note_sources(&self) -> Result<Vec<ElectronNote>, NotesError> {
        let columns = table_columns(&self.connection, "notebook_sources")?;
        let select = select_list(
            &columns,
            &[
                ("source_id", true),
                ("notebook_id", true),
                ("type", true),
                ("title", true),
                ("content", true),
            ],
        );
        let mut statement = self.connection.prepare(&format!(
            "SELECT {select} FROM notebook_sources
             WHERE type IN ('note','text') ORDER BY source_id LIMIT ?1"
        ))?;
        let rows = statement.query_map([(MAX_NOTES + 1) as i64], |row| {
            let identity: String = row.get(0)?;
            let notebook_identity: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let title: String = row.get(3)?;
            let content: String = row.get(4)?;
            let rich_text = kind == "note";
            let inline_attachments = count_inline_images(&content);
            Ok(ElectronNote {
                identity: format!("source:{identity}"),
                notebook_identity: Some(notebook_identity),
                title,
                content,
                rich_text,
                inline_attachments,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn verify(connection: &Connection) -> Result<(), NotesError> {
    for (table, required) in [
        ("notes", &["id", "title", "content"][..]),
        ("notebooks", &["notebook_id", "title"][..]),
        (
            "notebook_sources",
            &["source_id", "notebook_id", "title", "type", "content"][..],
        ),
    ] {
        let columns = table_columns(connection, table)?;
        if required
            .iter()
            .any(|name| !columns.iter().any(|c| c == name))
        {
            return Err(NotesError::Unsupported);
        }
    }
    Ok(())
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, NotesError> {
    let mut statement = connection
        .prepare("SELECT name FROM pragma_table_info(?1)")
        .map_err(|_| NotesError::Unsupported)?;
    let rows = statement.query_map([table], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn select_list(columns: &[String], desired: &[(&str, bool)]) -> String {
    desired
        .iter()
        .map(|(name, required)| {
            if *required || columns.iter().any(|column| column == name) {
                (*name).to_owned()
            } else {
                format!("CAST(NULL AS TEXT) AS {name}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Mirrors the Electron exporter's HTML classification: a `<` followed by
/// optional whitespace and then an ASCII tag letter or an HTML comment.
fn looks_like_html(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        let mut cursor = index + 1;
        while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t' | b'\n' | b'\r') {
            cursor += 1;
        }
        if cursor + 3 <= bytes.len() && &bytes[cursor..cursor + 3] == b"!--" {
            return true;
        }
        if cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
            return true;
        }
        index = cursor;
    }
    false
}

fn count_inline_images(content: &str) -> u32 {
    let bytes = content.as_bytes();
    let mut count = 0u32;
    let mut index = 0;
    while index + 4 <= bytes.len() {
        if bytes[index] == b'<'
            && (bytes[index + 1] | 0x20) == b'i'
            && (bytes[index + 2] | 0x20) == b'm'
            && (bytes[index + 3] | 0x20) == b'g'
        {
            count += 1;
        }
        index += 1;
    }
    count
}

impl ElectronNotesSnapshot {
    /// Wire the read-only reader into the v2 handoff consumed by
    /// `NotesStore::import_v2`. Format metadata and original-file provenance
    /// are carried; inline `<img>` references remain inside the verbatim HTML
    /// content and are not extracted into attachments here.
    pub fn into_v2(self, source: &str) -> V2Handoff {
        V2Handoff {
            format: "uebox-notes".to_owned(),
            version: 2,
            source: source.to_owned(),
            notebooks: self
                .notebooks
                .into_iter()
                .map(|notebook| V2Notebook {
                    identity: notebook.identity,
                    title: notebook.title,
                })
                .collect(),
            notes: self
                .notes
                .into_iter()
                .map(|note| V2Note {
                    identity: note.identity,
                    notebook_identity: note.notebook_identity,
                    title: note.title,
                    content: note.content,
                    format: if note.rich_text {
                        NoteFormat::Html
                    } else {
                        NoteFormat::Plain
                    },
                    attachments: Vec::new(),
                })
                .collect(),
            original_files: self
                .original_files
                .into_iter()
                .map(|file| V2OriginalFile {
                    identity: file.identity,
                    notebook_identity: file.notebook_identity,
                    kind: file.kind,
                    title: file.title,
                    content: file.content,
                    raw_content: file.raw_content,
                    source_url: file.source_url,
                    file_name: file.file_name,
                    file_path: file.file_path,
                    media_url: file.media_url,
                })
                .collect(),
        }
    }
}
