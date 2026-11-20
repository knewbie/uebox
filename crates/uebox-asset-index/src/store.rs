use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{config::DbConfig, params, Connection, OpenFlags, OptionalExtension, Row};
use serde::Serialize;

use crate::manifest::identity;
use crate::{Archive, Asset, Error, Folder, Vault};

const APPLICATION_ID: u32 = 0x5542_4149;
const VERSION: u32 = 1;
const MAX_PAGE_SIZE: u32 = 100;

const SCHEMA: &str = "
CREATE TABLE vaults (
    identity TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    root_path TEXT NOT NULL,
    index_available INTEGER NOT NULL CHECK (index_available IN (0,1))
) STRICT;
CREATE TABLE folders (
    vault TEXT NOT NULL REFERENCES vaults(identity) ON DELETE CASCADE,
    identity TEXT NOT NULL,
    parent TEXT,
    name TEXT NOT NULL,
    PRIMARY KEY (vault, identity)
) STRICT, WITHOUT ROWID;
CREATE INDEX folders_listing ON folders(vault, parent, identity);
CREATE TABLE assets (
    vault TEXT NOT NULL REFERENCES vaults(identity) ON DELETE CASCADE,
    identity TEXT NOT NULL,
    folder TEXT,
    name TEXT NOT NULL,
    relative_path TEXT,
    size INTEGER,
    extension TEXT,
    modified_time TEXT,
    tags TEXT NOT NULL,
    PRIMARY KEY (vault, identity),
    FOREIGN KEY (vault, folder) REFERENCES folders(vault, identity)
) STRICT, WITHOUT ROWID;
CREATE INDEX assets_listing ON assets(vault, folder, identity);";

pub struct AssetIndex(Connection);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub assets: usize,
    pub folders: usize,
    pub retained_index: bool,
}

impl AssetIndex {
    /// Create a new, private index via a staged file. The caller owns the profile lock.
    pub fn create(path: &Path) -> Result<Self, Error> {
        let parent = path.parent().ok_or(Error::Invalid)?;
        let temporary = tempfile::Builder::new()
            .prefix(".asset-index-")
            .tempfile_in(parent)
            .map_err(|_| Error::Unavailable)?;
        {
            let mut connection = open(temporary.path())?;
            let transaction = connection.transaction()?;
            transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
            transaction.pragma_update(None, "user_version", VERSION)?;
            transaction.execute_batch(SCHEMA)?;
            transaction.commit()?;
            connection.close().map_err(|_| Error::Unavailable)?;
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| Error::Unavailable)?;
        temporary
            .persist_noclobber(path)
            .map_err(|_| Error::Unavailable)?;
        Self::open_existing(path)
    }

    pub fn open_existing(path: &Path) -> Result<Self, Error> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::Missing
            } else {
                Error::Unavailable
            }
        })?;
        if !metadata.is_file() || metadata.len() < 100 {
            return Err(Error::Unavailable);
        }
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            match fs::symlink_metadata(name) {
                Ok(_) => return Err(Error::RecoveryRequired),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(Error::Unavailable),
            }
        }
        let connection = open(path)?;
        let id: u32 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let journal: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if id != APPLICATION_ID || version != VERSION || journal != "delete" {
            return Err(Error::Unavailable);
        }
        Ok(Self(connection))
    }

    /// Replace a vault's indexed metadata atomically. An offline archive retains
    /// the previous snapshot when it refers to the same registered path.
    pub fn import(&mut self, archive: Archive) -> Result<ImportReport, Error> {
        let Archive {
            vault,
            folders,
            assets,
            ..
        } = archive;
        let transaction = self
            .0
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, bool)> = transaction
            .query_row(
                "SELECT root_path, index_available FROM vaults WHERE identity = ?",
                [&vault.identity],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let retained_index =
            !vault.index_available && existing.as_ref().is_some_and(|(_, available)| *available);
        if retained_index && existing.is_some_and(|(root, _)| root != vault.root_path) {
            return Err(Error::Conflict);
        }
        transaction.execute(
            "INSERT INTO vaults(identity, name, root_path, index_available) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(identity) DO UPDATE SET name=excluded.name, root_path=excluded.root_path,
             index_available=excluded.index_available",
            params![
                vault.identity,
                vault.name,
                vault.root_path,
                vault.index_available || retained_index
            ],
        )?;

        if vault.index_available {
            transaction.execute("DELETE FROM assets WHERE vault = ?", [&vault.identity])?;
            transaction.execute("DELETE FROM folders WHERE vault = ?", [&vault.identity])?;
            {
                let mut folder_statement =
                    transaction.prepare("INSERT INTO folders VALUES (?1, ?2, ?3, ?4)")?;
                for folder in &folders {
                    folder_statement.execute(params![
                        vault.identity,
                        folder.identity,
                        folder.parent_identity,
                        folder.name,
                    ])?;
                }
                let mut asset_statement = transaction
                    .prepare("INSERT INTO assets VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
                for asset in &assets {
                    let tags = serde_json::to_string(&asset.tags).map_err(|_| Error::Invalid)?;
                    asset_statement.execute(params![
                        vault.identity,
                        asset.identity,
                        asset.folder_identity,
                        asset.name,
                        asset.relative_path,
                        asset.size,
                        asset.extension,
                        asset.modified_time,
                        tags,
                    ])?;
                }
            }
        }
        transaction.commit()?;
        Ok(ImportReport {
            assets: assets.len(),
            folders: folders.len(),
            retained_index,
        })
    }

    pub fn vaults(&self, after: Option<&str>, limit: u32) -> Result<Vec<Vault>, Error> {
        page(after, limit)?;
        let mut statement = self.0.prepare(
            "SELECT identity, name, root_path, index_available FROM vaults
             WHERE (?1 IS NULL OR identity > ?1) ORDER BY identity LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, limit], |row| {
            Ok(Vault {
                identity: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                index_available: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn folders(
        &self,
        vault: &str,
        parent: Option<&str>,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Folder>, Error> {
        scope(vault, parent, after, limit)?;
        let mut statement = self.0.prepare(
            "SELECT identity, parent, name FROM folders WHERE vault = ?1 AND parent IS ?2
             AND (?3 IS NULL OR identity > ?3) ORDER BY identity LIMIT ?4",
        )?;
        let rows = statement.query_map(params![vault, parent, after, limit], |row| {
            Ok(Folder {
                identity: row.get(0)?,
                parent_identity: row.get(1)?,
                name: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn assets(
        &self,
        vault: &str,
        folder: Option<&str>,
        after: Option<&str>,
        limit: u32,
        search: Option<&str>,
    ) -> Result<Vec<Asset>, Error> {
        scope(vault, folder, after, limit)?;
        if search.is_some_and(|term| term.len() > 256 || term.chars().any(char::is_control)) {
            return Err(Error::Invalid);
        }
        let mut statement = self.0.prepare(
            "SELECT identity, folder, name, relative_path, size, extension, modified_time, tags
             FROM assets WHERE vault = ?1 AND
             (CASE WHEN ?5 IS NULL THEN folder IS ?2 ELSE instr(lower(name), lower(?5)) > 0 END)
             AND (?3 IS NULL OR identity > ?3) ORDER BY identity LIMIT ?4",
        )?;
        let rows = statement.query_map(params![vault, folder, after, limit, search], asset_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn open(path: &Path) -> Result<Connection, Error> {
    // Resolve OS directory aliases such as macOS /var before SQLite opens the
    // file with NOFOLLOW. The final component still cannot be a symlink.
    let parent = path.parent().ok_or(Error::Invalid)?;
    let parent = fs::canonicalize(parent).map_err(|_| Error::Unavailable)?;
    let path = parent.join(path.file_name().ok_or(Error::Invalid)?);
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.busy_timeout(Duration::from_secs(3))?;
    Ok(connection)
}

fn page(after: Option<&str>, limit: u32) -> Result<(), Error> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) || after.is_some_and(|id| !identity(id)) {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn scope(vault: &str, parent: Option<&str>, after: Option<&str>, limit: u32) -> Result<(), Error> {
    if !identity(vault) || parent.is_some_and(|id| !identity(id)) {
        return Err(Error::Invalid);
    }
    page(after, limit)
}

fn asset_row(row: &Row<'_>) -> rusqlite::Result<Asset> {
    let tags: String = row.get(7)?;
    let tags = serde_json::from_str(&tags).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Asset {
        identity: row.get(0)?,
        folder_identity: row.get(1)?,
        name: row.get(2)?,
        relative_path: row.get(3)?,
        size: row.get(4)?,
        extension: row.get(5)?,
        modified_time: row.get(6)?,
        tags,
    })
}
