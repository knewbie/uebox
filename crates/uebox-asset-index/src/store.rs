use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{config::DbConfig, params, Connection, OpenFlags, OptionalExtension, Row};
use serde::Serialize;

use crate::manifest::identity;
use crate::scan::{self, Directory, ScanLimits};
use crate::{Archive, Asset, Error, Folder, Vault};

const APPLICATION_ID: u32 = 0x5542_4149;
const VERSION: u32 = 2;
const MAX_PAGE_SIZE: u32 = 100;

const SCHEMA: &str = "
CREATE TABLE vaults (
    identity TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    root_path TEXT NOT NULL,
    index_available INTEGER NOT NULL CHECK (index_available IN (0,1))
) STRICT;
CREATE TABLE local_vaults (
    vault TEXT PRIMARY KEY NOT NULL REFERENCES vaults(identity) ON DELETE CASCADE,
    root_key TEXT NOT NULL,
    online INTEGER NOT NULL CHECK (online IN (0,1)),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    last_report TEXT NOT NULL
) STRICT, WITHOUT ROWID;
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

pub struct AssetLocation {
    pub root: String,
    pub relative: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalVault {
    pub identity: String,
    pub name: String,
    pub root_path: String,
    pub online: bool,
    pub index_available: bool,
    pub revision: u64,
    pub last_report: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub assets: usize,
    pub folders: usize,
    pub skipped: usize,
    pub metadata_bytes: usize,
    pub revision: u64,
    pub offline: bool,
    pub retained_index: bool,
}

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
        let mut connection = open(path)?;
        let id: u32 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let journal: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if id != APPLICATION_ID || journal != "delete" {
            return Err(Error::Unavailable);
        }
        match version {
            1 => {
                // v1 had no local registrations; the upgrade adds the registry atomically.
                let transaction = connection
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                transaction.execute_batch(
                    "CREATE TABLE local_vaults (
                    vault TEXT PRIMARY KEY NOT NULL REFERENCES vaults(identity) ON DELETE CASCADE,
                    root_key TEXT NOT NULL,
                    online INTEGER NOT NULL CHECK (online IN (0,1)),
                    revision INTEGER NOT NULL CHECK (revision >= 0),
                    last_report TEXT NOT NULL
                ) STRICT, WITHOUT ROWID;",
                )?;
                transaction.pragma_update(None, "user_version", VERSION)?;
                transaction.commit()?;
            }
            VERSION => {}
            _ => return Err(Error::Unavailable),
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
        // Local registrations pin their own root; imports may not move or replace them.
        let pinned: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_vaults WHERE vault = ?)",
            [&vault.identity],
            |row| row.get(0),
        )?;
        if pinned {
            return Err(Error::Conflict);
        }
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

        let (asset_count, folder_count) = (assets.len(), folders.len());
        if vault.index_available {
            emit(
                &transaction,
                &vault.identity,
                &scan::Snapshot {
                    folders,
                    assets,
                    skipped: 0,
                    bytes: 0,
                },
            )?;
        }
        transaction.commit()?;
        Ok(ImportReport {
            assets: asset_count,
            folders: folder_count,
            retained_index,
        })
    }

    /// Register a fixed local root without enumerating it. The path is opened one
    /// component at a time without following symlinks and its identity is pinned:
    /// only the originally registered directory may serve future rescans.
    pub fn register_local(&mut self, name: &str, root: &Path) -> Result<LocalVault, Error> {
        if !identity(name) || name.len() > 1024 {
            return Err(Error::Invalid);
        }
        let root = scan::root_path(root)?;
        let root_text = root.to_str().ok_or(Error::Invalid)?.to_owned();
        let directory = Directory::open_root(&root)?;
        let metadata = directory.metadata()?;
        let identity = scan::stable_id("vault", &[&root_text]);
        let transaction = self
            .0
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let pinned: Option<(String, String, bool, u64, String)> = transaction
            .query_row(
                "SELECT vaults.root_path, local_vaults.root_key, vaults.index_available,
                        local_vaults.revision, local_vaults.last_report
                 FROM local_vaults JOIN vaults ON vaults.identity = local_vaults.vault
                 WHERE local_vaults.vault = ?",
                [&identity],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_root, stored_key, available, revision, report)) = pinned {
            if stored_root != root_text || stored_key != metadata.key {
                return Err(Error::Conflict);
            }
            transaction.execute(
                "UPDATE vaults SET name = ?1 WHERE identity = ?2",
                params![name, identity],
            )?;
            transaction.commit()?;
            return Ok(LocalVault {
                identity,
                name: name.to_owned(),
                root_path: root_text,
                online: true,
                index_available: available,
                revision,
                last_report: report,
            });
        }
        let identity_taken: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM vaults WHERE identity = ?)",
            [&identity],
            |row| row.get(0),
        )?;
        let root_taken: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_vaults WHERE root_key = ?)",
            [&metadata.key],
            |row| row.get(0),
        )?;
        if identity_taken || root_taken {
            return Err(Error::Conflict);
        }
        transaction.execute(
            "INSERT INTO vaults(identity, name, root_path, index_available) VALUES (?1, ?2, ?3, 0)",
            params![identity, name, root_text],
        )?;
        let report = last_report(0, false, false);
        transaction.execute(
            "INSERT INTO local_vaults(vault, root_key, online, revision, last_report)
             VALUES (?1, ?2, 1, 0, ?3)",
            params![identity, metadata.key, report],
        )?;
        transaction.commit()?;
        Ok(LocalVault {
            identity,
            name: name.to_owned(),
            root_path: root_text,
            online: true,
            index_available: false,
            revision: 0,
            last_report: report,
        })
    }

    /// Re-scan an explicitly registered local root. The complete snapshot replaces the
    /// previous one in a single transaction; a missing or unreadable root keeps the
    /// last data, and a stale call cannot overwrite a newer revision.
    pub fn rescan_local(&mut self, vault: &str, limits: ScanLimits) -> Result<ScanReport, Error> {
        let limits = limits.validate()?;
        let expected = self.revision(vault)?.ok_or(Error::Missing)?;
        let root_text = self.root_text(vault)?.ok_or(Error::Missing)?;
        let directory = match Directory::open_root(Path::new(&root_text)) {
            Ok(directory) => directory,
            Err(Error::Missing | Error::Invalid) => return self.mark_offline(vault, expected),
            Err(error) => return Err(error),
        };
        let metadata = directory.metadata()?;
        let pinned_key: String = self.0.query_row(
            "SELECT root_key FROM local_vaults WHERE vault = ?",
            [vault],
            |row| row.get(0),
        )?;
        if pinned_key != metadata.key {
            return Err(Error::Conflict);
        }
        let snapshot = scan::collect(&directory, vault, limits)?;
        let report_values = (
            snapshot.assets.len(),
            snapshot.folders.len(),
            snapshot.skipped,
            snapshot.bytes,
        );
        let transaction = self
            .0
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let actual: u64 = transaction
            .query_row(
                "SELECT revision FROM local_vaults WHERE vault = ?",
                [vault],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Missing)?;
        if actual != expected {
            return Err(Error::Conflict);
        }
        emit(&transaction, vault, &snapshot)?;
        transaction.execute(
            "UPDATE vaults SET index_available = 1 WHERE identity = ?",
            [vault],
        )?;
        let revision = actual + 1;
        let report = last_report(report_values.0, false, false);
        transaction.execute(
            "UPDATE local_vaults SET online = 1, revision = ?2, last_report = ?3 WHERE vault = ?1",
            params![vault, revision, report],
        )?;
        transaction.commit()?;
        Ok(ScanReport {
            assets: report_values.0,
            folders: report_values.1,
            skipped: report_values.2,
            metadata_bytes: report_values.3,
            revision,
            offline: false,
            retained_index: false,
        })
    }

    pub fn local_vaults(&self, after: Option<&str>, limit: u32) -> Result<Vec<LocalVault>, Error> {
        page(after, limit)?;
        let mut statement = self.0.prepare(
            "SELECT vaults.identity, vaults.name, vaults.root_path, local_vaults.online,
                    vaults.index_available, local_vaults.revision, local_vaults.last_report
             FROM local_vaults JOIN vaults ON vaults.identity = local_vaults.vault
             WHERE (?1 IS NULL OR local_vaults.vault > ?1) ORDER BY local_vaults.vault LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, limit], |row| {
            Ok(LocalVault {
                identity: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                online: row.get(3)?,
                index_available: row.get(4)?,
                revision: row.get(5)?,
                last_report: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn revision(&self, vault: &str) -> Result<Option<u64>, Error> {
        self.0
            .query_row(
                "SELECT revision FROM local_vaults WHERE vault = ?",
                [vault],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn root_text(&self, vault: &str) -> Result<Option<String>, Error> {
        self.0
            .query_row(
                "SELECT root_path FROM vaults WHERE identity = ?",
                [vault],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn mark_offline(&mut self, vault: &str, expected: u64) -> Result<ScanReport, Error> {
        let transaction = self
            .0
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let actual: u64 = transaction
            .query_row(
                "SELECT revision FROM local_vaults WHERE vault = ?",
                [vault],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Missing)?;
        if actual != expected {
            return Err(Error::Conflict);
        }
        let available: bool = transaction.query_row(
            "SELECT index_available FROM vaults WHERE identity = ?",
            [vault],
            |row| row.get(0),
        )?;
        let retained: usize = transaction.query_row(
            "SELECT COUNT(*) FROM assets WHERE vault = ?",
            [vault],
            |row| row.get(0),
        )?;
        let report = last_report(retained, true, available);
        transaction.execute(
            "UPDATE local_vaults SET online = 0, last_report = ?2 WHERE vault = ?1",
            params![vault, report],
        )?;
        transaction.commit()?;
        Ok(ScanReport {
            assets: retained,
            folders: 0,
            skipped: 0,
            metadata_bytes: 0,
            revision: actual,
            offline: true,
            retained_index: available,
        })
    }

    pub fn location(&self, vault: &str, asset: &str) -> Result<Option<AssetLocation>, Error> {
        if !identity(vault) || !identity(asset) {
            return Err(Error::Invalid);
        }
        self.0
            .query_row(
                "SELECT vaults.root_path, assets.relative_path
             FROM assets JOIN vaults ON vaults.identity = assets.vault
             WHERE assets.vault = ?1 AND assets.identity = ?2 AND assets.relative_path IS NOT NULL",
                params![vault, asset],
                |row| {
                    Ok(AssetLocation {
                        root: row.get(0)?,
                        relative: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
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

fn emit(
    transaction: &rusqlite::Transaction<'_>,
    vault: &str,
    snapshot: &scan::Snapshot,
) -> Result<(), Error> {
    transaction.execute("DELETE FROM assets WHERE vault = ?", [vault])?;
    transaction.execute("DELETE FROM folders WHERE vault = ?", [vault])?;
    let mut folder_statement =
        transaction.prepare("INSERT INTO folders VALUES (?1, ?2, ?3, ?4)")?;
    for folder in &snapshot.folders {
        folder_statement.execute(params![
            vault,
            folder.identity,
            folder.parent_identity,
            folder.name,
        ])?;
    }
    drop(folder_statement);
    let mut asset_statement =
        transaction.prepare("INSERT INTO assets VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
    for asset in &snapshot.assets {
        let tags = serde_json::to_string(&asset.tags).map_err(|_| Error::Invalid)?;
        asset_statement.execute(params![
            vault,
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
    Ok(())
}

fn last_report(assets: usize, offline: bool, retained: bool) -> String {
    let status = if offline {
        "offline"
    } else if assets == 0 {
        "empty"
    } else {
        "available"
    };
    serde_json::json!({ "status": status, "assets": assets, "retained": retained }).to_string()
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
