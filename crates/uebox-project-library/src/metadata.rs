//! Engine-agnostic per-project metadata: note, description, and ordering
//! preferences (pinned, sort order), keyed by the project registration ID.
//!
//! Metadata is always optional and never derived from the project on disk.
//! A project with no metadata row is treated as all defaults (unpinned,
//! `sort_order = 0.0`, no note, no description).

use rusqlite::{params, Connection, OptionalExtension};

use super::cover::cover_from_join;
use super::{
    failure, into_record, open_readonly, read_record, require_registered, sqlite_error,
    verify_database_identity, CoverRecord, ErrorKind, LibraryError, ProjectLibrary, ProjectRecord,
};

pub(crate) const CREATE_METADATA: &str = "CREATE TABLE project_metadata (
    project_id INTEGER PRIMARY KEY CHECK (project_id > 0),
    note TEXT CHECK (note IS NULL OR (length(CAST(note AS BLOB)) BETWEEN 1 AND 1048576 AND instr(note, char(0)) = 0)),
    description TEXT CHECK (description IS NULL OR (length(CAST(description AS BLOB)) BETWEEN 1 AND 1048576 AND instr(description, char(0)) = 0)),
    is_pinned INTEGER NOT NULL DEFAULT 0 CHECK (is_pinned IN (0, 1)),
    sort_order REAL NOT NULL DEFAULT 0.0
) STRICT, WITHOUT ROWID";

const MAX_TEXT_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectMetadata {
    pub project_id: i64,
    pub note: Option<String>,
    pub description: Option<String>,
    pub is_pinned: bool,
    pub sort_order: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectWithMetadata {
    pub project: ProjectRecord,
    pub metadata: ProjectMetadata,
    pub cover: Option<CoverRecord>,
}

impl ProjectLibrary {
    /// Set the full engine-agnostic metadata for a registered project.
    ///
    /// `None` clears a text field. All fields are written together in one
    /// transaction and the committed row is read back before it is returned.
    pub fn set_metadata(
        &mut self,
        project_id: i64,
        note: Option<&str>,
        description: Option<&str>,
        is_pinned: bool,
        sort_order: f64,
    ) -> Result<ProjectMetadata, LibraryError> {
        if project_id <= 0 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project ID must be positive",
            ));
        }
        if !sort_order.is_finite() {
            return Err(failure(
                ErrorKind::InvalidInput,
                "sort order must be a finite number",
            ));
        }
        let note = note.map(normalize_text).transpose()?;
        let description = description.map(normalize_text).transpose()?;
        self.write_transaction("metadata", |transaction| {
            require_registered(transaction, project_id)?;
            transaction
                .execute(
                    "INSERT INTO project_metadata
                         (project_id, note, description, is_pinned, sort_order)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(project_id) DO UPDATE SET
                         note = excluded.note,
                         description = excluded.description,
                         is_pinned = excluded.is_pinned,
                         sort_order = excluded.sort_order",
                    params![project_id, note, description, is_pinned as i64, sort_order],
                )
                .map_err(|error| sqlite_error("set project metadata", error))?;
            read_metadata(transaction, project_id)?.ok_or_else(|| {
                failure(
                    ErrorKind::Other,
                    "metadata row disappeared after it was written",
                )
            })
        })
    }

    /// Read the metadata for one project, or `None` when it has never been set.
    pub fn get_metadata(&self, project_id: i64) -> Result<Option<ProjectMetadata>, LibraryError> {
        if project_id <= 0 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project ID must be positive",
            ));
        }
        let verified = open_readonly(&self.database, Some(self.identity))?;
        let metadata = read_metadata(&verified.connection, project_id)?;
        verify_database_identity(&verified.connection, &self.database, self.identity)?;
        Ok(metadata)
    }

    /// All registrations with their metadata (defaulted) and cover metadata,
    /// ordered pinned-first, then by sort order, then by registration ID.
    pub fn list_with_metadata(&self) -> Result<Vec<ProjectWithMetadata>, LibraryError> {
        let verified = open_readonly(&self.database, Some(self.identity))?;
        let mut statement = verified
            .connection
            .prepare(
                "SELECT p.id, p.engine, p.name, p.path,
                        m.note, m.description, m.is_pinned, m.sort_order,
                        c.format, c.filename, c.source
                 FROM projects AS p
                 LEFT JOIN project_metadata AS m ON m.project_id = p.id
                 LEFT JOIN project_covers AS c ON c.project_id = p.id
                 ORDER BY COALESCE(m.is_pinned, 0) DESC,
                          COALESCE(m.sort_order, 0.0) ASC,
                          p.id ASC",
            )
            .map_err(|error| sqlite_error("prepare project list with metadata", error))?;
        let mut rows = statement
            .query([])
            .map_err(|error| sqlite_error("query project list with metadata", error))?;
        let mut result = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| sqlite_error("read project metadata row", error))?
        {
            let project = into_record(
                read_record(row)
                    .map_err(|error| sqlite_error("read project registration", error))?,
            )?;
            let metadata = ProjectMetadata {
                project_id: project.id,
                note: row
                    .get::<_, Option<String>>(4)
                    .map_err(|error| sqlite_error("read project note", error))?,
                description: row
                    .get::<_, Option<String>>(5)
                    .map_err(|error| sqlite_error("read project description", error))?,
                is_pinned: row
                    .get::<_, Option<i64>>(6)
                    .map_err(|error| sqlite_error("read project pinned flag", error))?
                    .unwrap_or(0)
                    != 0,
                sort_order: row
                    .get::<_, Option<f64>>(7)
                    .map_err(|error| sqlite_error("read project sort order", error))?
                    .unwrap_or(0.0),
            };
            let cover = cover_from_join(
                project.id,
                row.get::<_, Option<String>>(8)
                    .map_err(|error| sqlite_error("read cover format", error))?,
                row.get::<_, Option<String>>(9)
                    .map_err(|error| sqlite_error("read cover filename", error))?,
                row.get::<_, Option<String>>(10)
                    .map_err(|error| sqlite_error("read cover source", error))?,
            )?;
            result.push(ProjectWithMetadata {
                project,
                metadata,
                cover,
            });
        }
        verify_database_identity(&verified.connection, &self.database, self.identity)?;
        Ok(result)
    }
}

fn normalize_text(value: &str) -> Result<Option<String>, LibraryError> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_TEXT_BYTES || value.contains('\0') {
        return Err(failure(
            ErrorKind::InvalidInput,
            "metadata text contains NUL or exceeds the byte limit",
        ));
    }
    Ok(Some(value.to_string()))
}

fn read_metadata(
    connection: &Connection,
    project_id: i64,
) -> Result<Option<ProjectMetadata>, LibraryError> {
    connection
        .query_row(
            "SELECT note, description, is_pinned, sort_order
             FROM project_metadata WHERE project_id = ?1",
            [project_id],
            |row| {
                Ok(ProjectMetadata {
                    project_id,
                    note: row.get(0)?,
                    description: row.get(1)?,
                    is_pinned: row.get::<_, i64>(2)? != 0,
                    sort_order: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_error("read project metadata", error))
}
