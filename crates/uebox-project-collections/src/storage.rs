use std::{
    ffi::OsString,
    fs::{self, Metadata, OpenOptions as FileOptions},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{config::DbConfig, ffi, Connection, OpenFlags};

use crate::CollectionError;

const APPLICATION_ID: u32 = 0x5542_5043; // UBPC
const VERSION: u32 = 1;
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

const COLLECTIONS: &str = "CREATE TABLE collections (
    id TEXT PRIMARY KEY CHECK (length(CAST(id AS BLOB)) BETWEEN 1 AND 256 AND instr(id, char(0)) = 0),
    name TEXT CHECK (length(CAST(name AS BLOB)) BETWEEN 1 AND 1024 AND instr(name, char(0)) = 0),
    sort_order INTEGER NOT NULL
) STRICT, WITHOUT ROWID";
const MEMBERSHIPS: &str = "CREATE TABLE memberships (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    project_id INTEGER NOT NULL CHECK (project_id > 0),
    PRIMARY KEY (collection_id, project_id)
) STRICT, WITHOUT ROWID";
const COLLECTIONS_BY_ORDER: &str =
    "CREATE INDEX collections_by_order ON collections(sort_order, id)";
const MEMBERSHIPS_BY_PROJECT: &str =
    "CREATE INDEX memberships_by_project ON memberships(project_id, collection_id)";
const SCHEMA: [(&str, &str, &str, &str); 4] = [
    ("table", "collections", "collections", COLLECTIONS),
    ("table", "memberships", "memberships", MEMBERSHIPS),
    (
        "index",
        "collections_by_order",
        "collections",
        COLLECTIONS_BY_ORDER,
    ),
    (
        "index",
        "memberships_by_project",
        "memberships",
        MEMBERSHIPS_BY_PROJECT,
    ),
];

pub(super) fn create_new(path: &Path) -> Result<Connection, CollectionError> {
    let path = database_path(path)?;
    reject_existing(&path)?;
    let parent = path.parent().ok_or(CollectionError::InvalidInput)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".uebox-project-collections-incomplete-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(parent).map_err(io_error)?;
    let staged_path = staging.path().join("collections.sqlite");
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
    for (_, _, _, sql) in SCHEMA {
        transaction.execute_batch(sql)?;
    }
    transaction.commit()?;
    validate_database(&connection)?;
    connection
        .close()
        .map_err(|(_, error)| CollectionError::from(error))?;
    FileOptions::new()
        .write(true)
        .open(&staged_path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)?;

    reject_existing(&path)?;
    publish_without_replacing(&staged_path, &path)?;
    open_existing(&path)
}

// A no-replace rename leaves exactly one link even if the process is killed
// immediately after publication. A hard link followed by unlink does not.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn publish_without_replacing(source: &Path, destination: &Path) -> Result<(), CollectionError> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let source =
        CString::new(source.as_os_str().as_bytes()).map_err(|_| CollectionError::InvalidInput)?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| CollectionError::InvalidInput)?;
    // Both C strings are live and NUL-terminated for the duration of the call.
    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
fn publish_without_replacing(source: &Path, destination: &Path) -> Result<(), CollectionError> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }
    let source: Vec<_> = source.as_os_str().encode_wide().chain([0]).collect();
    let destination: Vec<_> = destination.as_os_str().encode_wide().chain([0]).collect();
    // Both buffers are live and NUL-terminated. Without flags this move may
    // neither overwrite an existing destination nor fall back to a copy.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) } == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn publish_without_replacing(_: &Path, _: &Path) -> Result<(), CollectionError> {
    Err(CollectionError::Unsupported)
}

pub(super) fn open_existing(path: &Path) -> Result<Connection, CollectionError> {
    let path = database_path(path)?;
    reject_sidecars(&path)?;
    let metadata = metadata_at(&path)?.ok_or(CollectionError::Missing)?;
    if !regular_file(&metadata) {
        return Err(CollectionError::Unsupported);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
            return Err(CollectionError::Unsupported);
        }
    }
    if metadata.len() < 100 {
        return Err(CollectionError::Corrupt);
    }
    {
        let read_only = open_sqlite(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_database(&read_only)?;
    }
    reject_sidecars(&path)?;
    let connection = open_sqlite(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    validate_header(&connection)?;
    configure(&connection)?;
    Ok(connection)
}

fn regular_file(metadata: &Metadata) -> bool {
    metadata.is_file() && !is_link(metadata)
}

fn open_sqlite(path: &Path, access: OpenFlags) -> Result<Connection, CollectionError> {
    let connection = Connection::open_with_flags(
        path,
        access | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    connection.busy_timeout(Duration::from_secs(3))?;
    Ok(connection)
}

fn configure(connection: &Connection) -> Result<(), CollectionError> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

fn validate_path(path: &Path) -> Result<(), CollectionError> {
    if !path.is_absolute() {
        return Err(CollectionError::InvalidInput);
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(CollectionError::InvalidInput)?;
    let normalized = name.trim_end_matches([' ', '.']).to_ascii_lowercase();
    if normalized.is_empty()
        || name.contains(['\0', ':'])
        || name.trim_end_matches([' ', '.']) != name
        || SIDECARS.iter().any(|suffix| normalized.ends_with(suffix))
    {
        return Err(CollectionError::InvalidInput);
    }
    Ok(())
}

fn database_path(path: &Path) -> Result<PathBuf, CollectionError> {
    validate_path(path)?;
    let parent = path.parent().ok_or(CollectionError::InvalidInput)?;
    let name = path.file_name().ok_or(CollectionError::InvalidInput)?;
    let parent = fs::canonicalize(parent).map_err(io_error)?;
    Ok(parent.join(name))
}

fn metadata_at(path: &Path) -> Result<Option<Metadata>, CollectionError> {
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

fn reject_existing(path: &Path) -> Result<(), CollectionError> {
    if metadata_at(path)?.is_some() {
        return Err(CollectionError::AlreadyExists);
    }
    for suffix in SIDECARS {
        if metadata_at(&sidecar(path, suffix))?.is_some() {
            return Err(CollectionError::AlreadyExists);
        }
    }
    Ok(())
}

fn reject_sidecars(path: &Path) -> Result<(), CollectionError> {
    reject_wal_sidecars(path)?;
    if metadata_at(&sidecar(path, "-journal"))?.is_some() {
        return Err(CollectionError::RecoveryRequired);
    }
    Ok(())
}

fn reject_wal_sidecars(path: &Path) -> Result<(), CollectionError> {
    for suffix in ["-wal", "-shm"] {
        if metadata_at(&sidecar(path, suffix))?.is_some() {
            return Err(CollectionError::Unsupported);
        }
    }
    Ok(())
}

fn io_error(error: io::Error) -> CollectionError {
    match error.kind() {
        io::ErrorKind::NotFound => CollectionError::Missing,
        io::ErrorKind::AlreadyExists => CollectionError::AlreadyExists,
        _ => CollectionError::Io,
    }
}

fn validate_header(connection: &Connection) -> Result<(), CollectionError> {
    let header = raw_header(connection)?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err(CollectionError::Corrupt);
    }
    // SQLite header: read/write formats, text encoding, user_version, application_id.
    if header[18..20] != [1, 1]
        || header[56..60] != 1_u32.to_be_bytes()
        || header[60..64] != VERSION.to_be_bytes()
        || header[68..72] != APPLICATION_ID.to_be_bytes()
    {
        return Err(CollectionError::Unsupported);
    }
    Ok(())
}

fn validate_database(connection: &Connection) -> Result<(), CollectionError> {
    validate_header(connection)?;
    let transaction = connection.unchecked_transaction()?;
    {
        let application_id: u32 =
            transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: u32 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let mode: String =
            transaction.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if application_id != APPLICATION_ID
            || version != VERSION
            || !mode.eq_ignore_ascii_case("delete")
        {
            return Err(CollectionError::Unsupported);
        }
        let mut schema = transaction
            .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name")?;
        let mut rows = schema.query([])?;
        let mut expected_schema = SCHEMA;
        expected_schema.sort_unstable_by_key(|entry| entry.1);
        for expected in expected_schema {
            let row = rows.next()?.ok_or(CollectionError::Unsupported)?;
            let actual: (String, String, String, Option<String>) =
                (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
            if (
                actual.0.as_str(),
                actual.1.as_str(),
                actual.2.as_str(),
                actual.3.as_deref(),
            ) != (expected.0, expected.1, expected.2, Some(expected.3))
            {
                return Err(CollectionError::Unsupported);
            }
        }
        if rows.next()?.is_some() {
            return Err(CollectionError::Unsupported);
        }

        let mut check = transaction.prepare("PRAGMA integrity_check")?;
        let mut results = check.query([])?;
        let first = results.next()?.ok_or(CollectionError::Corrupt)?;
        if first.get::<_, String>(0)? != "ok" || results.next()?.is_some() {
            return Err(CollectionError::Corrupt);
        }
        let mut foreign_keys = transaction.prepare("PRAGMA foreign_key_check")?;
        if foreign_keys.query([])?.next()?.is_some() {
            return Err(CollectionError::Corrupt);
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Use SQLite's own descriptor. Closing an independent POSIX file handle can
/// release SQLite locks held elsewhere in the same process.
fn raw_header(connection: &Connection) -> Result<[u8; 100], CollectionError> {
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
        return Err(CollectionError::Unsupported);
    }
    let read = unsafe { (*(*file).pMethods).xRead }.ok_or(CollectionError::Unsupported)?;
    let mut header = [0; 100];
    let result = unsafe { read(file, header.as_mut_ptr().cast(), 100, 0) };
    match result {
        ffi::SQLITE_OK => Ok(header),
        ffi::SQLITE_IOERR_SHORT_READ => Err(CollectionError::Corrupt),
        _ => Err(CollectionError::Io),
    }
}
