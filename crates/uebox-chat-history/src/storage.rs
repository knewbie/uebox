use std::{
    ffi::OsString,
    fs::{self, Metadata, OpenOptions as FileOptions},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{config::DbConfig, ffi, Connection, OpenFlags};

use crate::HistoryError;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod recovery;

const APPLICATION_ID: u32 = 0x5542_4348; // UBCH
const VERSION: u32 = 2;
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

const CHATS: &str = "CREATE TABLE chats (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) BETWEEN 1 AND 128 AND id NOT GLOB '*[^A-Za-z0-9_-]*'
        AND instr(id, char(0)) = 0
    ),
    engine TEXT NOT NULL CHECK (engine IN ('unreal', 'unity', 'godot')),
    title TEXT NOT NULL DEFAULT '' CHECK (length(title) <= 80 AND instr(title, char(0)) = 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
) STRICT, WITHOUT ROWID";
const CHATS_BY_ACTIVITY: &str = "CREATE INDEX chats_by_activity ON chats(updated_at_ms DESC, id)";
const TURNS: &str = "CREATE TABLE turns (
    id INTEGER PRIMARY KEY CHECK (id > 0),
    chat_id TEXT NOT NULL REFERENCES chats(id),
    user_text TEXT NOT NULL CHECK (
        length(CAST(user_text AS BLOB)) BETWEEN 1 AND 1048576 AND instr(user_text, char(0)) = 0
    ),
    state TEXT NOT NULL CHECK (state IN ('active', 'completed', 'failed', 'cancelled', 'interrupted')),
    assistant_text TEXT CHECK (
        length(CAST(assistant_text AS BLOB)) <= 4194304 AND instr(assistant_text, char(0)) = 0
    ),
    failure_code TEXT CHECK (
        failure_code IN ('model_unavailable', 'model_response', 'limit_reached', 'target_unavailable', 'internal')
    ),
    started_at_ms INTEGER NOT NULL CHECK (started_at_ms >= 0),
    finished_at_ms INTEGER CHECK (finished_at_ms >= started_at_ms),
    CHECK (
        (state = 'active' AND finished_at_ms IS NULL AND assistant_text IS NULL AND failure_code IS NULL)
        OR (state = 'completed' AND finished_at_ms IS NOT NULL AND assistant_text IS NOT NULL AND failure_code IS NULL)
        OR (state = 'failed' AND finished_at_ms IS NOT NULL AND assistant_text IS NULL AND failure_code IS NOT NULL)
        OR (state IN ('cancelled', 'interrupted') AND finished_at_ms IS NOT NULL AND assistant_text IS NULL AND failure_code IS NULL)
    )
) STRICT";
const ONE_ACTIVE_TURN: &str =
    "CREATE UNIQUE INDEX one_active_turn ON turns(chat_id) WHERE state = 'active'";
const TURNS_BY_CHAT: &str = "CREATE INDEX turns_by_chat ON turns(chat_id, id)";
const ACTOR_LABEL_RECEIPTS: &str = "CREATE TABLE actor_label_receipts (
    turn_id INTEGER NOT NULL REFERENCES turns(id),
    approval_id TEXT NOT NULL CHECK (
        length(approval_id) BETWEEN 1 AND 128 AND approval_id NOT GLOB '*[^A-Za-z0-9_-]*'
        AND instr(approval_id, char(0)) = 0
    ),
    status TEXT NOT NULL CHECK (status IN ('applied', 'mismatch', 'not_executed', 'unknown')),
    actual_label TEXT CHECK (
        length(CAST(actual_label AS BLOB)) <= 4096 AND instr(actual_label, char(0)) = 0
        AND actual_label NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || '-' || char(159) || ']*')
    ),
    CHECK ((status IN ('applied', 'mismatch') AND actual_label IS NOT NULL)
        OR (status IN ('not_executed', 'unknown') AND actual_label IS NULL)),
    PRIMARY KEY (turn_id, approval_id)
) STRICT, WITHOUT ROWID";

const NODE_NAME_RECEIPTS: &str = "CREATE TABLE node_name_receipts (
    turn_id INTEGER NOT NULL REFERENCES turns(id),
    approval_id TEXT NOT NULL CHECK (
        length(approval_id) BETWEEN 1 AND 128 AND approval_id NOT GLOB '*[^A-Za-z0-9_-]*'
        AND instr(approval_id, char(0)) = 0
    ),
    status TEXT NOT NULL CHECK (status IN ('applied', 'mismatch', 'not_executed', 'unknown')),
    actual_name TEXT CHECK (
        length(CAST(actual_name AS BLOB)) <= 4096 AND instr(actual_name, char(0)) = 0
        AND actual_name NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || '-' || char(159) || ']*')
    ),
    CHECK ((status IN ('applied', 'mismatch') AND actual_name IS NOT NULL)
        OR (status IN ('not_executed', 'unknown') AND actual_name IS NULL)),
    PRIMARY KEY (turn_id, approval_id)
) STRICT, WITHOUT ROWID";

type SchemaEntry = (&'static str, &'static str, &'static str, &'static str);
const SCHEMA_V1: [SchemaEntry; 6] = [
    ("table", "chats", "chats", CHATS),
    ("table", "turns", "turns", TURNS),
    (
        "table",
        "actor_label_receipts",
        "actor_label_receipts",
        ACTOR_LABEL_RECEIPTS,
    ),
    ("index", "chats_by_activity", "chats", CHATS_BY_ACTIVITY),
    ("index", "one_active_turn", "turns", ONE_ACTIVE_TURN),
    ("index", "turns_by_chat", "turns", TURNS_BY_CHAT),
];
const NODE_NAME_ENTRY: SchemaEntry = (
    "table",
    "node_name_receipts",
    "node_name_receipts",
    NODE_NAME_RECEIPTS,
);

fn schema(version: u32) -> Vec<SchemaEntry> {
    let mut schema = SCHEMA_V1.to_vec();
    if version == VERSION {
        schema.push(NODE_NAME_ENTRY);
    }
    schema.sort_unstable_by_key(|entry| entry.1);
    schema
}

pub(super) fn create_new(path: &Path) -> Result<Connection, HistoryError> {
    let path = database_path(path)?;
    reject_existing(&path)?;
    let parent = path.parent().ok_or(HistoryError::InvalidInput)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".uebox-chat-history-incomplete-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(parent).map_err(io_error)?;
    let staged_path = staging.path().join("history.sqlite");
    let mut options = FileOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(options.open(&staged_path).map_err(io_error)?);

    let mut connection = open_sqlite(&staged_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure(&connection)?;
    let transaction = connection.transaction()?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", VERSION)?;
    // Keep table/index creation ordering so all indexed tables already exist.
    for (_, _, _, sql) in SCHEMA_V1.into_iter().chain([NODE_NAME_ENTRY]) {
        transaction.execute_batch(sql)?;
    }
    transaction.commit()?;
    validate_database(&connection)?;
    connection
        .close()
        .map_err(|(_, error)| HistoryError::from(error))?;
    FileOptions::new()
        .write(true)
        .open(&staged_path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)?;

    reject_existing(&path)?;
    fs::hard_link(&staged_path, &path).map_err(io_error)?;
    drop(staging);
    open_existing(&path)
}

pub(super) fn open_existing(path: &Path) -> Result<Connection, HistoryError> {
    let path = database_path(path)?;
    reject_sidecars(&path)?;
    database_metadata(&path)?;
    let version = {
        let read_only = open_sqlite(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_database(&read_only)?
    };
    reject_sidecars(&path)?;
    let mut connection = open_sqlite(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    if validate_header(&connection)? != version {
        return Err(HistoryError::Unsupported);
    }
    configure(&connection)?;
    upgrade(&mut connection, version)?;
    Ok(connection)
}

pub(super) fn open_existing_recovering(path: &Path) -> Result<Connection, HistoryError> {
    let path = database_path(path)?;
    reject_wal_sidecars(&path)?;
    let journal = sidecar(&path, "-journal");
    if metadata_at(&journal)?.is_none() {
        return open_existing(&path);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        recovery::check_files(&path, &journal)?;
        let mut connection = open_sqlite(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        // Inspect the raw file before asking SQLite for anything that opens its pager.
        validate_header(&connection)?;
        // SQLite acquires its own recovery locks and either rolls back or reports an error.
        connection.query_row("SELECT count(*) FROM main.sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })?;
        reject_sidecars(&path)?;
        let version = validate_database(&connection)?;
        configure(&connection)?;
        upgrade(&mut connection, version)?;
        Ok(connection)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(HistoryError::RecoveryRequired)
    }
}

fn upgrade(connection: &mut Connection, version: u32) -> Result<(), HistoryError> {
    if version == VERSION {
        return Ok(());
    }
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute_batch(NODE_NAME_RECEIPTS)?;
    transaction.pragma_update(None, "user_version", VERSION)?;
    transaction.commit()?;
    validate_database(connection)?;
    Ok(())
}

fn database_metadata(path: &Path) -> Result<Metadata, HistoryError> {
    let metadata = metadata_at(path)?.ok_or(HistoryError::Missing)?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(HistoryError::Unsupported);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(HistoryError::Unsupported);
        }
    }
    if metadata.len() < 100 {
        return Err(HistoryError::Corrupt);
    }

    Ok(metadata)
}

fn open_sqlite(path: &Path, access: OpenFlags) -> Result<Connection, HistoryError> {
    let connection = Connection::open_with_flags(
        path,
        access | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    connection.busy_timeout(Duration::from_secs(3))?;
    Ok(connection)
}

fn configure(connection: &Connection) -> Result<(), HistoryError> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

fn database_path(path: &Path) -> Result<PathBuf, HistoryError> {
    if !path.is_absolute() {
        return Err(HistoryError::InvalidInput);
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(HistoryError::InvalidInput)?;
    let normalized = name.trim_end_matches([' ', '.']).to_ascii_lowercase();
    if normalized.is_empty()
        || name.trim_end_matches([' ', '.']) != name
        || SIDECARS.iter().any(|suffix| normalized.ends_with(suffix))
    {
        return Err(HistoryError::InvalidInput);
    }
    let parent =
        fs::canonicalize(path.parent().ok_or(HistoryError::InvalidInput)?).map_err(io_error)?;
    Ok(parent.join(name))
}

fn metadata_at(path: &Path) -> Result<Option<Metadata>, HistoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

fn reject_existing(path: &Path) -> Result<(), HistoryError> {
    if metadata_at(path)?.is_some() {
        return Err(HistoryError::AlreadyExists);
    }
    for suffix in SIDECARS {
        if metadata_at(&sidecar(path, suffix))?.is_some() {
            return Err(HistoryError::AlreadyExists);
        }
    }
    Ok(())
}

fn reject_sidecars(path: &Path) -> Result<(), HistoryError> {
    reject_wal_sidecars(path)?;
    if metadata_at(&sidecar(path, "-journal"))?.is_some() {
        return Err(HistoryError::RecoveryRequired);
    }
    Ok(())
}

fn reject_wal_sidecars(path: &Path) -> Result<(), HistoryError> {
    for suffix in ["-wal", "-shm"] {
        if metadata_at(&sidecar(path, suffix))?.is_some() {
            return Err(HistoryError::Unsupported);
        }
    }
    Ok(())
}

fn io_error(error: io::Error) -> HistoryError {
    match error.kind() {
        io::ErrorKind::NotFound => HistoryError::Missing,
        io::ErrorKind::AlreadyExists => HistoryError::AlreadyExists,
        _ => HistoryError::Io,
    }
}

fn validate_header(connection: &Connection) -> Result<u32, HistoryError> {
    let header = raw_header(connection)?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err(HistoryError::Corrupt);
    }
    let version = u32::from_be_bytes(
        header[60..64]
            .try_into()
            .map_err(|_| HistoryError::Corrupt)?,
    );
    // SQLite header: read/write formats, text encoding, user_version, application_id.
    if header[18..20] != [1, 1]
        || header[56..60] != 1_u32.to_be_bytes()
        || !matches!(version, 1 | VERSION)
        || header[68..72] != APPLICATION_ID.to_be_bytes()
    {
        return Err(HistoryError::Unsupported);
    }
    Ok(version)
}

fn validate_database(connection: &Connection) -> Result<u32, HistoryError> {
    let expected_version = validate_header(connection)?;
    let transaction = connection.unchecked_transaction()?;
    {
        let application_id: u32 =
            transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: u32 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let mode: String =
            transaction.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if application_id != APPLICATION_ID
            || version != expected_version
            || !mode.eq_ignore_ascii_case("delete")
        {
            return Err(HistoryError::Unsupported);
        }
        let mut schema = transaction
            .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name")?;
        let mut rows = schema.query([])?;
        for expected in self::schema(version) {
            let row = rows.next()?.ok_or(HistoryError::Unsupported)?;
            let actual: (String, String, String, Option<String>) =
                (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
            if (
                actual.0.as_str(),
                actual.1.as_str(),
                actual.2.as_str(),
                actual.3.as_deref(),
            ) != (expected.0, expected.1, expected.2, Some(expected.3))
            {
                return Err(HistoryError::Unsupported);
            }
        }
        if rows.next()?.is_some() {
            return Err(HistoryError::Unsupported);
        }

        let mut check = transaction.prepare("PRAGMA integrity_check")?;
        let mut results = check.query([])?;
        let first = results.next()?.ok_or(HistoryError::Corrupt)?;
        if first.get::<_, String>(0)? != "ok" || results.next()?.is_some() {
            return Err(HistoryError::Corrupt);
        }
        let mut foreign_keys = transaction.prepare("PRAGMA foreign_key_check")?;
        if foreign_keys.query([])?.next()?.is_some() {
            return Err(HistoryError::Corrupt);
        }
    }
    transaction.commit()?;
    Ok(expected_version)
}

/// Use SQLite's own descriptor. Closing an independent POSIX file handle can
/// release SQLite locks held elsewhere in the same process.
fn raw_header(connection: &Connection) -> Result<[u8; 100], HistoryError> {
    let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
    // SQLite owns the file and its methods throughout the connection's lifetime.
    let result = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            (&mut file as *mut *mut ffi::sqlite3_file).cast(),
        )
    };
    if result != ffi::SQLITE_OK || file.is_null() || unsafe { (*file).pMethods.is_null() } {
        return Err(HistoryError::Unsupported);
    }
    let read = unsafe { (*(*file).pMethods).xRead }.ok_or(HistoryError::Unsupported)?;
    let mut header = [0; 100];
    let result = unsafe { read(file, header.as_mut_ptr().cast(), 100, 0) };
    match result {
        ffi::SQLITE_OK => Ok(header),
        ffi::SQLITE_IOERR_SHORT_READ => Err(HistoryError::Corrupt),
        _ => Err(HistoryError::Io),
    }
}
