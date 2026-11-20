//! Standalone, explicitly located project registrations for the Tauri preview.
//!
//! This does not discover or migrate the Electron database, inspect project
//! files, or start an editor. Creation and opening are separate operations.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::{params, Connection, ErrorCode, OpenFlags, TransactionBehavior};

const APPLICATION_ID: i64 = 0x5542_504c; // UBPL
const SCHEMA_VERSION: i64 = 1;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const CREATE_PROJECTS: &str = "CREATE TABLE projects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engine TEXT NOT NULL CHECK(engine IN ('unreal', 'unity', 'godot')),
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    path TEXT NOT NULL,
    UNIQUE(engine, path)
)";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineKind {
    Unreal,
    Unity,
    Godot,
}

impl EngineKind {
    fn as_db(self) -> &'static str {
        match self {
            Self::Unreal => "unreal",
            Self::Unity => "unity",
            Self::Godot => "godot",
        }
    }

    fn from_db(value: &str) -> Result<Self, LibraryError> {
        match value {
            "unreal" => Ok(Self::Unreal),
            "unity" => Ok(Self::Unity),
            "godot" => Ok(Self::Godot),
            _ => Err(failure(
                ErrorKind::Unsupported,
                format!("unknown engine: {value}"),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Missing,
    AlreadyExists,
    Unsupported,
    Corrupt,
    Locked,
    Duplicate,
    InvalidInput,
    Io,
    Other,
}

#[derive(Debug)]
pub struct LibraryError {
    pub kind: ErrorKind,
    pub message: String,
}

impl fmt::Display for LibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for LibraryError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRecord {
    pub id: i64,
    pub engine: EngineKind,
    pub name: String,
    /// The directory path selected at registration, not a canonicalized path.
    pub path: PathBuf,
}

/// A path to a standalone library. Every operation rechecks its schema.
#[derive(Debug)]
pub struct ProjectLibrary {
    database: PathBuf,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity(u64, u64);

struct VerifiedConnection {
    connection: Connection,
    identity: FileIdentity,
}

fn failure(kind: ErrorKind, message: impl Into<String>) -> LibraryError {
    LibraryError {
        kind,
        message: message.into(),
    }
}

fn io_error(context: &str, error: io::Error) -> LibraryError {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => ErrorKind::Missing,
        io::ErrorKind::AlreadyExists => ErrorKind::AlreadyExists,
        io::ErrorKind::InvalidInput => ErrorKind::InvalidInput,
        _ => ErrorKind::Io,
    };
    failure(kind, format!("{context}: {error}"))
}

fn sqlite_error(context: &str, error: rusqlite::Error) -> LibraryError {
    let kind = match &error {
        rusqlite::Error::SqliteFailure(code, _) => match code.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => ErrorKind::Locked,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => ErrorKind::Corrupt,
            ErrorCode::PermissionDenied
            | ErrorCode::ReadOnly
            | ErrorCode::SystemIoFailure
            | ErrorCode::CannotOpen
            | ErrorCode::DiskFull => ErrorKind::Io,
            ErrorCode::ConstraintViolation => ErrorKind::Duplicate,
            _ => ErrorKind::Other,
        },
        _ => ErrorKind::Other,
    };
    failure(kind, format!("{context}: {error}"))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn database_path(path: &Path) -> Result<PathBuf, LibraryError> {
    if !path.is_absolute() {
        return Err(failure(
            ErrorKind::InvalidInput,
            "database path must be absolute",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| failure(ErrorKind::InvalidInput, "database path must name a file"))?;
    let name_text = name
        .to_str()
        .ok_or_else(|| failure(ErrorKind::InvalidInput, "database filename must be UTF-8"))?;
    let normalized = name_text.trim_end_matches([' ', '.']).to_ascii_lowercase();
    if ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
    {
        return Err(failure(
            ErrorKind::InvalidInput,
            "database filename is reserved for a SQLite sidecar",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| failure(ErrorKind::InvalidInput, "database path has no parent"))?;
    let parent =
        fs::canonicalize(parent).map_err(|error| io_error("resolve database parent", error))?;
    Ok(parent.join(name))
}

fn reject_existing(path: &Path) -> Result<(), LibraryError> {
    for candidate in [
        path.to_path_buf(),
        sidecar(path, "-wal"),
        sidecar(path, "-shm"),
        sidecar(path, "-journal"),
    ] {
        match fs::symlink_metadata(candidate) {
            Ok(_) => {
                return Err(failure(
                    ErrorKind::AlreadyExists,
                    "database destination or sidecar already exists",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("check database destination", error)),
        }
    }
    Ok(())
}

fn reject_wal_sidecars(path: &Path) -> Result<(), LibraryError> {
    for suffix in ["-wal", "-shm"] {
        match fs::symlink_metadata(sidecar(path, suffix)) {
            Ok(_) => {
                return Err(failure(
                    ErrorKind::Unsupported,
                    "WAL-mode project libraries are not supported",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("check database sidecar", error)),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn database_identity(path: &Path) -> Result<FileIdentity, LibraryError> {
    use std::os::unix::fs::MetadataExt;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io_error("stat project database", error))?;
    if !metadata.is_file() {
        return Err(failure(
            ErrorKind::Unsupported,
            "project database must be a regular non-symlink file",
        ));
    }
    if metadata.nlink() != 1 {
        return Err(failure(
            ErrorKind::Unsupported,
            "database has multiple hard links",
        ));
    }
    if metadata.len() < 100 {
        return Err(failure(
            ErrorKind::Corrupt,
            "project database is empty or truncated",
        ));
    }
    if metadata.ino() == 0 {
        return Err(failure(
            ErrorKind::Unsupported,
            "filesystem does not provide a stable database file identity",
        ));
    }
    Ok(FileIdentity(metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn windows_handle_identity(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> Result<(FileIdentity, u32), LibraryError> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(io_error(
            "read Windows file identity",
            io::Error::last_os_error(),
        ));
    }
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    if file_index == 0 || information.nNumberOfLinks == 0 {
        return Err(failure(
            ErrorKind::Unsupported,
            "filesystem did not provide a stable Windows file identity",
        ));
    }
    Ok((
        FileIdentity(u64::from(information.dwVolumeSerialNumber), file_index),
        information.nNumberOfLinks,
    ))
}

#[cfg(windows)]
fn windows_path_identity(
    path: &Path,
    directory: bool,
) -> Result<(FileIdentity, u32), LibraryError> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    let mut options = OpenOptions::new();
    options.read(true);
    if directory {
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = options
        .open(path)
        .map_err(|error| io_error("open Windows file identity", error))?;
    windows_handle_identity(file.as_raw_handle().cast())
}

#[cfg(windows)]
fn database_identity(path: &Path) -> Result<FileIdentity, LibraryError> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io_error("stat project database", error))?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(failure(
            ErrorKind::Unsupported,
            "project database must be a regular non-reparse file",
        ));
    }
    if metadata.len() < 100 {
        return Err(failure(
            ErrorKind::Corrupt,
            "project database is empty or truncated",
        ));
    }
    let (identity, links) = windows_path_identity(path, false)?;
    if links != 1 {
        return Err(failure(
            ErrorKind::Unsupported,
            "database has multiple hard links",
        ));
    }
    Ok(identity)
}

#[cfg(not(any(unix, windows)))]
fn database_identity(_path: &Path) -> Result<FileIdentity, LibraryError> {
    Err(failure(
        ErrorKind::Unsupported,
        "database identity is not supported on this platform",
    ))
}

fn verify_database_identity(
    connection: &Connection,
    path: &Path,
    expected: FileIdentity,
) -> Result<(), LibraryError> {
    let actual = database_identity(path)?;
    if actual != expected {
        return Err(failure(ErrorKind::Locked, "project database was replaced"));
    }
    #[cfg(unix)]
    {
        let mut moved: i32 = 0;
        let result = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut i32).cast(),
            )
        };
        if result != rusqlite::ffi::SQLITE_OK {
            return Err(failure(
                ErrorKind::Unsupported,
                "SQLite VFS cannot verify database identity",
            ));
        }
        if moved != 0 {
            return Err(failure(
                ErrorKind::Locked,
                "SQLite database was moved or replaced",
            ));
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::HANDLE;
        let mut handle: HANDLE = std::ptr::null_mut();
        let result = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
                (&mut handle as *mut HANDLE).cast(),
            )
        };
        if result != rusqlite::ffi::SQLITE_OK || handle.is_null() {
            return Err(failure(
                ErrorKind::Unsupported,
                "SQLite VFS cannot expose its Windows file handle",
            ));
        }
        let (opened, links) = windows_handle_identity(handle)?;
        if links != 1 {
            return Err(failure(
                ErrorKind::Unsupported,
                "opened database has multiple hard links",
            ));
        }
        if opened != expected {
            return Err(failure(
                ErrorKind::Locked,
                "SQLite database was replaced while opening",
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = connection;
    Ok(())
}

fn raw_database_header(connection: &Connection) -> Result<[u8; 100], LibraryError> {
    use rusqlite::ffi;
    let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
    // SQLite owns the only descriptor used for reading this database header.
    let result = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            (&mut file as *mut *mut ffi::sqlite3_file).cast(),
        )
    };
    if result != ffi::SQLITE_OK || file.is_null() || unsafe { (*file).pMethods.is_null() } {
        return Err(failure(
            ErrorKind::Unsupported,
            "SQLite VFS does not expose its main file",
        ));
    }
    let read = unsafe { (*(*file).pMethods).xRead }.ok_or_else(|| {
        failure(
            ErrorKind::Unsupported,
            "SQLite VFS cannot read its main file",
        )
    })?;
    let mut header = [0; 100];
    let result = unsafe { read(file, header.as_mut_ptr().cast(), 100, 0) };
    if result != ffi::SQLITE_OK {
        return Err(failure(
            if result == ffi::SQLITE_IOERR_SHORT_READ {
                ErrorKind::Corrupt
            } else {
                ErrorKind::Io
            },
            format!("read database header through SQLite VFS: {result}"),
        ));
    }
    Ok(header)
}

fn validate_header(connection: &Connection) -> Result<(), LibraryError> {
    let header = raw_database_header(connection)?;
    if &header[..16] != SQLITE_HEADER {
        return Err(failure(
            ErrorKind::Corrupt,
            "database does not have a SQLite header",
        ));
    }
    let word = |offset: usize| {
        u32::from_be_bytes(
            header[offset..offset + 4]
                .try_into()
                .expect("database header word"),
        )
    };
    if word(68) != APPLICATION_ID as u32 || word(60) != SCHEMA_VERSION as u32 {
        return Err(failure(
            ErrorKind::Unsupported,
            "database application ID or schema version is not supported",
        ));
    }
    if header[18] != 1 || header[19] != 1 {
        return Err(failure(
            ErrorKind::Unsupported,
            "WAL-mode project libraries are not supported",
        ));
    }
    Ok(())
}

fn validate_schema(connection: &Connection) -> Result<(), LibraryError> {
    let app_id: i64 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|error| sqlite_error("read application ID", error))?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sqlite_error("read schema version", error))?;
    if app_id != APPLICATION_ID || version != SCHEMA_VERSION {
        return Err(failure(
            ErrorKind::Unsupported,
            "database application ID or schema version changed",
        ));
    }
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|error| sqlite_error("read journal mode", error))?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(failure(
            ErrorKind::Unsupported,
            "database journal mode is not DELETE",
        ));
    }
    let mut statement = connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")
        .map_err(|error| sqlite_error("read database schema", error))?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|error| sqlite_error("query database schema", error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| sqlite_error("read database schema row", error))?;
    let expected = vec![
        (
            "index".to_string(),
            "sqlite_autoindex_projects_1".to_string(),
            "projects".to_string(),
            None,
        ),
        (
            "table".to_string(),
            "projects".to_string(),
            "projects".to_string(),
            Some(CREATE_PROJECTS.to_string()),
        ),
        (
            "table".to_string(),
            "sqlite_sequence".to_string(),
            "sqlite_sequence".to_string(),
            Some("CREATE TABLE sqlite_sequence(name,seq)".to_string()),
        ),
    ];
    if objects != expected {
        return Err(failure(
            ErrorKind::Unsupported,
            "database schema does not match the project library",
        ));
    }
    // Registration relies on the unique index to agree with the stored paths.
    let mut check = connection
        .prepare("PRAGMA integrity_check")
        .map_err(|error| sqlite_error("prepare integrity check", error))?;
    let results = check
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| sqlite_error("run integrity check", error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| sqlite_error("read integrity check", error))?;
    if results.as_slice() != ["ok"] {
        return Err(failure(
            ErrorKind::Corrupt,
            format!("SQLite integrity_check: {}", results.join("; ")),
        ));
    }
    Ok(())
}

fn open_readonly(
    path: &Path,
    expected: Option<FileIdentity>,
) -> Result<VerifiedConnection, LibraryError> {
    let identity = database_identity(path)?;
    if expected.is_some_and(|expected| expected != identity) {
        return Err(failure(ErrorKind::Locked, "project database was replaced"));
    }
    reject_wal_sidecars(path)?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| sqlite_error("open project database read-only", error))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|error| sqlite_error("disable checkpoint on close", error))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| sqlite_error("configure database timeout", error))?;
    verify_database_identity(&connection, path, identity)?;
    validate_header(&connection)?;
    connection
        .execute_batch("BEGIN")
        .map_err(|error| sqlite_error("begin database validation", error))?;
    validate_schema(&connection)?;
    verify_database_identity(&connection, path, identity)?;
    Ok(VerifiedConnection {
        connection,
        identity,
    })
}

fn open_writable(path: &Path, expected: FileIdentity) -> Result<Connection, LibraryError> {
    if database_identity(path)? != expected {
        return Err(failure(ErrorKind::Locked, "project database was replaced"));
    }
    reject_wal_sidecars(path)?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| sqlite_error("open project database for writing", error))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|error| sqlite_error("disable checkpoint on close", error))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| sqlite_error("configure database timeout", error))?;
    verify_database_identity(&connection, path, expected)?;
    validate_header(&connection)?;
    Ok(connection)
}

fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, String, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn into_record(row: (i64, String, String, String)) -> Result<ProjectRecord, LibraryError> {
    let (id, engine, name, path) = row;
    if id <= 0
        || name.trim().is_empty()
        || name.contains('\0')
        || !Path::new(&path).is_absolute()
        || path.contains('\0')
    {
        return Err(failure(
            ErrorKind::Corrupt,
            "stored project record has an invalid ID, name, or absolute path",
        ));
    }
    Ok(ProjectRecord {
        id,
        engine: EngineKind::from_db(&engine)?,
        name,
        path: PathBuf::from(path),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity(u64, u64);

#[cfg(unix)]
fn directory_identity(path: &Path) -> Result<DirectoryIdentity, LibraryError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path).map_err(|error| io_error("stat project directory", error))?;
    if !metadata.is_dir() {
        return Err(failure(
            ErrorKind::InvalidInput,
            "project path is not a directory",
        ));
    }
    if metadata.ino() == 0 {
        return Err(failure(
            ErrorKind::Unsupported,
            "filesystem does not provide a stable project directory identity",
        ));
    }
    Ok(DirectoryIdentity(metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn directory_identity(path: &Path) -> Result<DirectoryIdentity, LibraryError> {
    let metadata = fs::metadata(path).map_err(|error| io_error("stat project directory", error))?;
    if !metadata.is_dir() {
        return Err(failure(
            ErrorKind::InvalidInput,
            "project path is not a directory",
        ));
    }
    let (identity, _) = windows_path_identity(path, true)?;
    Ok(DirectoryIdentity(identity.0, identity.1))
}

#[cfg(not(any(unix, windows)))]
fn directory_identity(_path: &Path) -> Result<DirectoryIdentity, LibraryError> {
    Err(failure(
        ErrorKind::Unsupported,
        "directory identity is not yet verified on this platform",
    ))
}

fn resolve_directory(path: &Path) -> Result<DirectoryIdentity, LibraryError> {
    let canonical =
        fs::canonicalize(path).map_err(|error| io_error("resolve project directory", error))?;
    directory_identity(&canonical)
}

impl ProjectLibrary {
    /// Create only a new database; an existing file or SQLite sidecar is never overwritten.
    pub fn create_new(database: impl AsRef<Path>) -> Result<Self, LibraryError> {
        let database = database_path(database.as_ref())?;
        reject_existing(&database)?;
        let parent = database
            .parent()
            .ok_or_else(|| failure(ErrorKind::InvalidInput, "database has no parent"))?;
        let mut builder = tempfile::Builder::new();
        builder.prefix(".uebox-project-library-incomplete-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let staging = builder
            .tempdir_in(parent)
            .map_err(|error| io_error("create database staging directory", error))?;
        let staged_path = staging.path().join("library.sqlite");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(
            options
                .open(&staged_path)
                .map_err(|error| io_error("create staged database", error))?,
        );
        let connection = Connection::open_with_flags(
            &staged_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| sqlite_error("open staged database", error))?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| sqlite_error("begin database creation", error))?;
        connection
            .execute(CREATE_PROJECTS, [])
            .map_err(|error| sqlite_error("create project schema", error))?;
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .map_err(|error| sqlite_error("set application ID", error))?;
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|error| sqlite_error("set schema version", error))?;
        connection
            .execute_batch("COMMIT")
            .map_err(|error| sqlite_error("commit database creation", error))?;
        validate_header(&connection)?;
        validate_schema(&connection)?;
        connection
            .close()
            .map_err(|(_, error)| sqlite_error("close staged database", error))?;
        OpenOptions::new()
            .write(true)
            .open(&staged_path)
            .and_then(|file| file.sync_all())
            .map_err(|error| io_error("sync staged database", error))?;
        let identity = database_identity(&staged_path)?;
        reject_existing(&database)?;
        fs::hard_link(&staged_path, &database)
            .map_err(|error| io_error("publish new database", error))?;
        Ok(Self { database, identity })
    }

    /// Open a previously created library, refusing unknown or changed schemas.
    pub fn open_existing(database: impl AsRef<Path>) -> Result<Self, LibraryError> {
        let database = database_path(database.as_ref())?;
        let verified = open_readonly(&database, None)?;
        let identity = verified.identity;
        drop(verified);
        Ok(Self { database, identity })
    }

    pub fn register(
        &mut self,
        engine: EngineKind,
        name: &str,
        path: impl AsRef<Path>,
    ) -> Result<ProjectRecord, LibraryError> {
        let path = path.as_ref();
        if name.trim().is_empty() || name.contains('\0') || !path.is_absolute() {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project name must be nonempty and path must be absolute",
            ));
        }
        let path_text = path
            .to_str()
            .ok_or_else(|| failure(ErrorKind::InvalidInput, "project path must be UTF-8"))?;
        // Validate before opening a writable connection, then validate again under
        // the write transaction so a changed schema cannot be used silently.
        drop(open_readonly(&self.database, Some(self.identity))?);
        let mut connection = open_writable(&self.database, self.identity)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_error("begin registration", error))?;
        validate_schema(&transaction)?;
        verify_database_identity(&transaction, &self.database, self.identity)?;
        let identity = resolve_directory(path)?;
        {
            let mut statement = transaction
                .prepare("SELECT path FROM projects WHERE engine = ?1")
                .map_err(|error| sqlite_error("prepare duplicate check", error))?;
            let paths = statement
                .query_map([engine.as_db()], |row| row.get::<_, String>(0))
                .map_err(|error| sqlite_error("query duplicate check", error))?;
            for existing in paths {
                let existing =
                    existing.map_err(|error| sqlite_error("read registered path", error))?;
                if !Path::new(&existing).is_absolute() || existing.contains('\0') {
                    return Err(failure(
                        ErrorKind::Corrupt,
                        "stored project path is not a valid absolute path",
                    ));
                }
                match resolve_directory(Path::new(&existing)) {
                    Ok(other) if other == identity => {
                        return Err(failure(
                            ErrorKind::Duplicate,
                            "this engine already has this online directory registered",
                        ))
                    }
                    Ok(_) => {}
                    Err(error) if error.kind == ErrorKind::Missing => {}
                    Err(error) => return Err(error),
                }
            }
        }
        if resolve_directory(path)? != identity {
            return Err(failure(
                ErrorKind::Locked,
                "project directory changed during registration",
            ));
        }
        transaction
            .execute(
                "INSERT INTO projects (engine, name, path) VALUES (?1, ?2, ?3)",
                params![engine.as_db(), name, path_text],
            )
            .map_err(|error| sqlite_error("register project", error))?;
        let id = transaction.last_insert_rowid();
        let record = transaction
            .query_row(
                "SELECT id, engine, name, path FROM projects WHERE id = ?1",
                [id],
                read_record,
            )
            .map_err(|error| sqlite_error("read registered project", error))?;
        let record = into_record(record)?;
        verify_database_identity(&transaction, &self.database, self.identity)?;
        transaction
            .commit()
            .map_err(|error| sqlite_error("commit registration", error))?;
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<ProjectRecord>, LibraryError> {
        let verified = open_readonly(&self.database, Some(self.identity))?;
        let mut statement = verified
            .connection
            .prepare("SELECT id, engine, name, path FROM projects ORDER BY id")
            .map_err(|error| sqlite_error("prepare project list", error))?;
        let rows = statement
            .query_map([], read_record)
            .map_err(|error| sqlite_error("query project list", error))?;
        let records = rows
            .map(|row| {
                row.map_err(|error| sqlite_error("read project row", error))
                    .and_then(into_record)
            })
            .collect::<Result<Vec<_>, _>>()?;
        verify_database_identity(&verified.connection, &self.database, self.identity)?;
        Ok(records)
    }

    /// Remove only the registration, including when its directory is offline.
    pub fn remove(&mut self, id: i64) -> Result<bool, LibraryError> {
        if id <= 0 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project ID must be positive",
            ));
        }
        drop(open_readonly(&self.database, Some(self.identity))?);
        let mut connection = open_writable(&self.database, self.identity)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_error("begin removal", error))?;
        validate_schema(&transaction)?;
        verify_database_identity(&transaction, &self.database, self.identity)?;
        let removed = transaction
            .execute("DELETE FROM projects WHERE id = ?1", [id])
            .map_err(|error| sqlite_error("remove project registration", error))?;
        verify_database_identity(&transaction, &self.database, self.identity)?;
        transaction
            .commit()
            .map_err(|error| sqlite_error("commit removal", error))?;
        Ok(removed != 0)
    }
}
