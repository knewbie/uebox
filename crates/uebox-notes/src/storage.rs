use std::{
    ffi::OsString,
    fs::{self, Metadata, OpenOptions as FileOptions},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{config::DbConfig, ffi, Connection, OpenFlags};

use crate::NotesError;

const APPLICATION_ID: u32 = 0x5542_4e54; // UBNT
const VERSION: u32 = 1;
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

const NOTEBOOKS: &str = "CREATE TABLE notebooks (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) <= 1024 AND instr(title, char(0)) = 0)
) STRICT";
const NOTES: &str = "CREATE TABLE notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    notebook_id INTEGER REFERENCES notebooks(id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) <= 1024 AND instr(title, char(0)) = 0),
    content TEXT NOT NULL CHECK (length(CAST(content AS BLOB)) <= 4194304 AND instr(content, char(0)) = 0)
) STRICT";
const LEGACY_IDENTITIES: &str = "CREATE TABLE legacy_identities (source TEXT NOT NULL, identity TEXT NOT NULL, record_id INTEGER NOT NULL, fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32), PRIMARY KEY (source, identity)) STRICT, WITHOUT ROWID";
const NOTES_BY_NOTEBOOK: &str = "CREATE INDEX notes_by_notebook ON notes(notebook_id, id)";
// SQLite creates sqlite_sequence for the AUTOINCREMENT primary keys.
const SCHEMA: [(&str, &str, &str, &str); 5] = [
    (
        "table",
        "legacy_identities",
        "legacy_identities",
        LEGACY_IDENTITIES,
    ),
    ("table", "notebooks", "notebooks", NOTEBOOKS),
    ("table", "notes", "notes", NOTES),
    ("index", "notes_by_notebook", "notes", NOTES_BY_NOTEBOOK),
    (
        "table",
        "sqlite_sequence",
        "sqlite_sequence",
        "CREATE TABLE sqlite_sequence(name,seq)",
    ),
];

pub(super) fn create_new(path: &Path) -> Result<Connection, NotesError> {
    let path = database_path(path)?;
    reject_existing(&path)?;
    let parent = path.parent().ok_or(NotesError::InvalidInput)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".uebox-notes-incomplete-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(parent).map_err(io_error)?;
    let staged_path = staging.path().join("notes.sqlite");
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
    for sql in [NOTEBOOKS, NOTES, LEGACY_IDENTITIES, NOTES_BY_NOTEBOOK] {
        transaction.execute_batch(sql)?;
    }
    transaction.commit()?;
    validate_database(&connection)?;
    connection
        .close()
        .map_err(|(_, error)| NotesError::from(error))?;
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
fn publish_without_replacing(source: &Path, destination: &Path) -> Result<(), NotesError> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let source =
        CString::new(source.as_os_str().as_bytes()).map_err(|_| NotesError::InvalidInput)?;
    let destination =
        CString::new(destination.as_os_str().as_bytes()).map_err(|_| NotesError::InvalidInput)?;
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
fn publish_without_replacing(source: &Path, destination: &Path) -> Result<(), NotesError> {
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
fn publish_without_replacing(_: &Path, _: &Path) -> Result<(), NotesError> {
    Err(NotesError::Unsupported)
}

pub(super) fn open_existing(path: &Path) -> Result<Connection, NotesError> {
    let path = database_path(path)?;
    reject_sidecars(&path)?;
    let metadata = metadata_at(&path)?.ok_or(NotesError::Missing)?;
    if !regular_file(&metadata) {
        return Err(NotesError::Unsupported);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
            return Err(NotesError::Unsupported);
        }
    }
    if metadata.len() < 100 {
        return Err(NotesError::Corrupt);
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

fn open_sqlite(path: &Path, access: OpenFlags) -> Result<Connection, NotesError> {
    let connection = Connection::open_with_flags(
        path,
        access | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    connection.busy_timeout(Duration::from_secs(3))?;
    Ok(connection)
}

fn configure(connection: &Connection) -> Result<(), NotesError> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

fn validate_path(path: &Path) -> Result<(), NotesError> {
    if !path.is_absolute() {
        return Err(NotesError::InvalidInput);
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(NotesError::InvalidInput)?;
    let normalized = name.trim_end_matches([' ', '.']).to_ascii_lowercase();
    if normalized.is_empty()
        || name.contains(['\0', ':'])
        || name.trim_end_matches([' ', '.']) != name
        || SIDECARS.iter().any(|suffix| normalized.ends_with(suffix))
    {
        return Err(NotesError::InvalidInput);
    }
    Ok(())
}

fn database_path(path: &Path) -> Result<PathBuf, NotesError> {
    validate_path(path)?;
    let parent = path.parent().ok_or(NotesError::InvalidInput)?;
    let name = path.file_name().ok_or(NotesError::InvalidInput)?;
    let parent = fs::canonicalize(parent).map_err(io_error)?;
    Ok(parent.join(name))
}

fn metadata_at(path: &Path) -> Result<Option<Metadata>, NotesError> {
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

fn reject_existing(path: &Path) -> Result<(), NotesError> {
    if metadata_at(path)?.is_some() {
        return Err(NotesError::AlreadyExists);
    }
    for suffix in SIDECARS {
        if metadata_at(&sidecar(path, suffix))?.is_some() {
            return Err(NotesError::AlreadyExists);
        }
    }
    Ok(())
}

fn reject_sidecars(path: &Path) -> Result<(), NotesError> {
    reject_wal_sidecars(path)?;
    if metadata_at(&sidecar(path, "-journal"))?.is_some() {
        return Err(NotesError::RecoveryRequired);
    }
    Ok(())
}

fn reject_wal_sidecars(path: &Path) -> Result<(), NotesError> {
    for suffix in ["-wal", "-shm"] {
        if metadata_at(&sidecar(path, suffix))?.is_some() {
            return Err(NotesError::Unsupported);
        }
    }
    Ok(())
}

fn io_error(error: io::Error) -> NotesError {
    match error.kind() {
        io::ErrorKind::NotFound => NotesError::Missing,
        io::ErrorKind::AlreadyExists => NotesError::AlreadyExists,
        _ => NotesError::Io,
    }
}

fn validate_header(connection: &Connection) -> Result<(), NotesError> {
    let header = raw_header(connection)?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err(NotesError::Corrupt);
    }
    // SQLite header: read/write formats, text encoding, user_version, application_id.
    if header[18..20] != [1, 1]
        || header[56..60] != 1_u32.to_be_bytes()
        || header[60..64] != VERSION.to_be_bytes()
        || header[68..72] != APPLICATION_ID.to_be_bytes()
    {
        return Err(NotesError::Unsupported);
    }
    Ok(())
}

fn validate_database(connection: &Connection) -> Result<(), NotesError> {
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
            return Err(NotesError::Unsupported);
        }
        let mut schema = transaction
            .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name")?;
        let mut rows = schema.query([])?;
        for expected in SCHEMA {
            let row = rows.next()?.ok_or(NotesError::Unsupported)?;
            let actual: (String, String, String, Option<String>) =
                (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
            if (
                actual.0.as_str(),
                actual.1.as_str(),
                actual.2.as_str(),
                actual.3.as_deref(),
            ) != (expected.0, expected.1, expected.2, Some(expected.3))
            {
                return Err(NotesError::Unsupported);
            }
        }
        if rows.next()?.is_some() {
            return Err(NotesError::Unsupported);
        }

        let mut check = transaction.prepare("PRAGMA integrity_check")?;
        let mut results = check.query([])?;
        let first = results.next()?.ok_or(NotesError::Corrupt)?;
        if first.get::<_, String>(0)? != "ok" || results.next()?.is_some() {
            return Err(NotesError::Corrupt);
        }
        let mut foreign_keys = transaction.prepare("PRAGMA foreign_key_check")?;
        if foreign_keys.query([])?.next()?.is_some() {
            return Err(NotesError::Corrupt);
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Use SQLite's own descriptor. Closing an independent POSIX file handle can
/// release SQLite locks held elsewhere in the same process.
fn raw_header(connection: &Connection) -> Result<[u8; 100], NotesError> {
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
        return Err(NotesError::Unsupported);
    }
    let read = unsafe { (*(*file).pMethods).xRead }.ok_or(NotesError::Unsupported)?;
    let mut header = [0; 100];
    let result = unsafe { read(file, header.as_mut_ptr().cast(), 100, 0) };
    match result {
        ffi::SQLITE_OK => Ok(header),
        ffi::SQLITE_IOERR_SHORT_READ => Err(NotesError::Corrupt),
        _ => Err(NotesError::Io),
    }
}
