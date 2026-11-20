//! Offline project collections associated with an existing Rust project library.
//!
//! The host must supply the same private collections database and registration
//! library throughout their lifetime. Hold the registration lock across any
//! operation consulting both stores so an ID cannot be removed during validation
//! and binding. SQLite serializes concurrent writes to the collections database.

use std::{collections::HashSet, fmt, path::Path};

use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use uebox_project_library::{ErrorKind as LibraryErrorKind, LibraryError, ProjectLibrary};

mod storage;

const MAX_ID_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionError {
    InvalidInput,
    Missing,
    AlreadyExists,
    Unsupported,
    Corrupt,
    RecoveryRequired,
    Locked,
    Duplicate,
    CollectionMissing,
    ProjectMissing,
    Conflict,
    RegistrationUnavailable,
    Io,
    Database,
}

impl CollectionError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Missing => "missing",
            Self::AlreadyExists => "already_exists",
            Self::Unsupported => "unsupported",
            Self::Corrupt => "corrupt",
            Self::RecoveryRequired => "recovery_required",
            Self::Locked => "locked",
            Self::Duplicate => "duplicate",
            Self::CollectionMissing => "collection_missing",
            Self::ProjectMissing => "project_missing",
            Self::Conflict => "conflict",
            Self::RegistrationUnavailable => "registration_unavailable",
            Self::Io => "io",
            Self::Database => "database",
        }
    }
}

impl fmt::Display for CollectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for CollectionError {}

impl From<rusqlite::Error> for CollectionError {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: String,
    /// Unnamed legacy collections retain a missing name.
    pub name: Option<String>,
    pub sort_order: i64,
    pub project_ids: Vec<i64>,
}

/// Host-supplied legacy collection after resolving project keys to Rust registrations.
#[derive(Debug, Clone, Copy)]
pub struct LegacyCollection<'a> {
    pub id: &'a str,
    pub name: Option<&'a str>,
    pub sort_order: i64,
    pub project_ids: &'a [i64],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub inserted: usize,
    pub already_present: usize,
}

#[derive(Debug)]
pub struct ProjectCollections {
    connection: Connection,
}

impl ProjectCollections {
    /// Publish a new database in an existing, private host directory.
    pub fn create_new(path: impl AsRef<Path>) -> Result<Self, CollectionError> {
        Ok(Self {
            connection: storage::create_new(path.as_ref())?,
        })
    }

    /// Open and validate a previously initialized database.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self, CollectionError> {
        Ok(Self {
            connection: storage::open_existing(path.as_ref())?,
        })
    }

    pub fn create_collection(
        &mut self,
        id: &str,
        name: &str,
        sort_order: i64,
    ) -> Result<(), CollectionError> {
        validate_id(id)?;
        let name = if name.is_empty() {
            None
        } else {
            Some(required_name(name)?)
        };
        if !insert_collection(&self.connection, id, name, sort_order)? {
            return Err(CollectionError::Duplicate);
        }
        Ok(())
    }

    pub fn rename_collection(&mut self, id: &str, name: &str) -> Result<(), CollectionError> {
        validate_id(id)?;
        let name = if name.is_empty() {
            None
        } else {
            Some(required_name(name)?)
        };
        if self.connection.execute(
            "UPDATE collections SET name = ?1 WHERE id = ?2",
            params![name, id],
        )? == 0
        {
            return Err(CollectionError::CollectionMissing);
        }
        Ok(())
    }

    /// Apply all sort updates atomically. Ties are ordered by collection ID.
    pub fn set_order(&mut self, updates: &[(&str, i64)]) -> Result<(), CollectionError> {
        let mut ids = HashSet::with_capacity(updates.len());
        for (id, _) in updates {
            validate_id(id)?;
            if !ids.insert(*id) {
                return Err(CollectionError::InvalidInput);
            }
        }

        self.write(|transaction| {
            let mut statement =
                transaction.prepare("UPDATE collections SET sort_order = ?1 WHERE id = ?2")?;
            for (id, order) in updates {
                if statement.execute(params![order, id])? == 0 {
                    return Err(CollectionError::CollectionMissing);
                }
            }
            Ok(())
        })
    }

    /// Returns true if a new membership was inserted.
    pub fn bind_project(
        &mut self,
        library: &ProjectLibrary,
        id: &str,
        project_id: i64,
    ) -> Result<bool, CollectionError> {
        validate_id(id)?;
        validate_project_id(project_id)?;
        if !registered_ids(library)?.contains(&project_id) {
            return Err(CollectionError::ProjectMissing);
        }
        self.write(|transaction| {
            if transaction
                .query_row("SELECT 1 FROM collections WHERE id = ?1", [id], |_| Ok(()))
                .optional()?
                .is_none()
            {
                return Err(CollectionError::CollectionMissing);
            }
            Ok(insert_membership(transaction, id, project_id)?)
        })
    }

    /// Returns the number of removed memberships. None removes all memberships
    /// for this project ID and can also be used after removing a registration.
    pub fn unbind_project(
        &mut self,
        id: Option<&str>,
        project_id: i64,
    ) -> Result<usize, CollectionError> {
        validate_project_id(project_id)?;
        Ok(match id {
            Some(id) => {
                validate_id(id)?;
                self.connection.execute(
                    "DELETE FROM memberships WHERE collection_id = ?1 AND project_id = ?2",
                    params![id, project_id],
                )?
            }
            None => self.connection.execute(
                "DELETE FROM memberships WHERE project_id = ?1",
                [project_id],
            )?,
        })
    }

    /// Empty collections are included. Memberships for removed registrations are
    /// hidden even if an interrupted host cleanup left their IDs in this database.
    pub fn list_collections(
        &self,
        library: &ProjectLibrary,
    ) -> Result<Vec<Collection>, CollectionError> {
        let registered = registered_ids(library)?;
        let mut statement = self.connection.prepare(
            "SELECT c.id, c.name, c.sort_order, m.project_id
             FROM collections AS c LEFT JOIN memberships AS m ON c.id = m.collection_id
             ORDER BY c.sort_order, c.id, m.project_id",
        )?;
        let mut rows = statement.query([])?;
        let mut collections: Vec<Collection> = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            if collections
                .last()
                .is_none_or(|collection| collection.id != id)
            {
                collections.push(Collection {
                    id,
                    name: row.get(1)?,
                    sort_order: row.get(2)?,
                    project_ids: Vec::new(),
                });
            }
            if let Some(project_id) = row.get::<_, Option<i64>>(3)? {
                if registered.contains(&project_id) {
                    collections
                        .last_mut()
                        .expect("current collection")
                        .project_ids
                        .push(project_id);
                }
            }
        }
        Ok(collections)
    }

    /// Dissolve the collection and cascade only its own membership rows.
    pub fn remove_collection(&mut self, id: &str) -> Result<bool, CollectionError> {
        validate_id(id)?;
        Ok(self
            .connection
            .execute("DELETE FROM collections WHERE id = ?1", [id])?
            != 0)
    }

    /// All input either commits together or is rejected. Matching IDs are
    /// idempotent only when metadata and the complete member set agree.
    pub fn import_legacy(
        &mut self,
        library: &ProjectLibrary,
        rows: &[LegacyCollection<'_>],
    ) -> Result<ImportSummary, CollectionError> {
        if rows.is_empty() {
            return Ok(ImportSummary::default());
        }
        let registered = registered_ids(library)?;
        let mut ids = HashSet::with_capacity(rows.len());
        let mut validated = Vec::with_capacity(rows.len());
        for row in rows {
            validate_id(row.id)?;
            if !ids.insert(row.id) {
                return Err(CollectionError::Duplicate);
            }
            let name = row
                .name
                .map(validate_name)
                .transpose()?
                .filter(|name| !name.trim().is_empty());
            let mut project_ids = row.project_ids.to_vec();
            project_ids.sort_unstable();
            project_ids.dedup();
            for &project_id in &project_ids {
                validate_project_id(project_id)?;
                if !registered.contains(&project_id) {
                    return Err(CollectionError::ProjectMissing);
                }
            }
            validated.push((row.id, name, row.sort_order, project_ids));
        }

        self.write(|transaction| {
            let mut report = ImportSummary::default();
            for (id, name, order, project_ids) in validated {
                if insert_collection(transaction, id, name, order)? {
                    for project_id in project_ids {
                        insert_membership(transaction, id, project_id)?;
                    }
                    report.inserted += 1;
                } else {
                    let metadata = transaction.query_row(
                        "SELECT name, sort_order FROM collections WHERE id = ?1",
                        [id],
                        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
                    )?;
                    if (metadata.0.as_deref(), metadata.1) != (name, order)
                        || memberships(transaction, id)? != project_ids
                    {
                        return Err(CollectionError::Conflict);
                    }
                    report.already_present += 1;
                }
            }
            Ok(report)
        })
    }

    fn write<T>(
        &mut self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, CollectionError>,
    ) -> Result<T, CollectionError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn insert_collection(
    connection: &Connection,
    id: &str,
    name: Option<&str>,
    sort_order: i64,
) -> rusqlite::Result<bool> {
    Ok(connection.execute(
        "INSERT INTO collections(id, name, sort_order) VALUES (?1, ?2, ?3) ON CONFLICT(id) DO NOTHING",
        params![id, name, sort_order],
    )? != 0)
}

fn insert_membership(connection: &Connection, id: &str, project_id: i64) -> rusqlite::Result<bool> {
    Ok(connection.execute(
        "INSERT INTO memberships(collection_id, project_id) VALUES (?1, ?2)
         ON CONFLICT(collection_id, project_id) DO NOTHING",
        params![id, project_id],
    )? != 0)
}

fn memberships(connection: &Connection, id: &str) -> rusqlite::Result<Vec<i64>> {
    let mut statement = connection.prepare(
        "SELECT project_id FROM memberships WHERE collection_id = ?1 ORDER BY project_id",
    )?;
    let rows = statement.query_map([id], |row| row.get(0))?;
    rows.collect()
}

fn registered_ids(library: &ProjectLibrary) -> Result<HashSet<i64>, CollectionError> {
    Ok(library
        .list()
        .map_err(registration_error)?
        .into_iter()
        .map(|row| row.id)
        .collect())
}

fn registration_error(error: LibraryError) -> CollectionError {
    match error.kind {
        LibraryErrorKind::Locked => CollectionError::Locked,
        _ => CollectionError::RegistrationUnavailable,
    }
}

fn validate_id(id: &str) -> Result<(), CollectionError> {
    if id.is_empty()
        || id != id.trim()
        || id.len() > MAX_ID_BYTES
        || id.chars().any(char::is_control)
    {
        return Err(CollectionError::InvalidInput);
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<&str, CollectionError> {
    if name.len() > MAX_NAME_BYTES || name.contains('\0') {
        return Err(CollectionError::InvalidInput);
    }
    Ok(name)
}

fn required_name(name: &str) -> Result<&str, CollectionError> {
    let name = validate_name(name)?.trim();
    if name.is_empty() {
        return Err(CollectionError::InvalidInput);
    }
    Ok(name)
}

fn validate_project_id(project_id: i64) -> Result<(), CollectionError> {
    if project_id <= 0 {
        return Err(CollectionError::InvalidInput);
    }
    Ok(())
}
