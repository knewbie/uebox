//! Restricted inspection and single-database snapshots for private SQLite copies.
//!
//! Path-only entrypoints are disabled: a separately loaded SQLite/VFS can lose
//! its same-process POSIX locks when another SQLite instance opens/closes the
//! same inode. Only an explicit unsafe host attestation for an exclusively held,
//! stable private copy enables access. This crate cannot verify that attestation.
//! These functions never initialize or migrate the source. SQLite's normal WAL
//! locking is retained: `immutable=1` could ignore committed WAL data.
//! SQLite may create an empty `-wal` and create or update `-shm`; callers who
//! need an untouched file set must first arrange a stable copy of the database
//! and its WAL/SHM. A snapshot is not a complete application backup. Source
//! identity verification currently fails closed on non-Unix platforms. Input
//! checks may return a different error before platform support is checked.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::config::DbConfig;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags};

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// Classification for a failure. `message` retains the underlying diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Missing,
    Locked,
    Corrupt,
    DestinationExists,
    SourceConflict,
    ExclusiveAccessRequired,
    MissingProjectsTable,
    Unsupported,
    AmbiguousWalTail,
    Io,
    Other,
}

#[derive(Debug)]
pub struct DataError {
    pub kind: ErrorKind,
    pub message: String,
}

impl fmt::Display for DataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for DataError {}

#[derive(Debug)]
pub struct DatabaseInspection {
    /// Table names as stored in sqlite_schema, including historical `_old` tables.
    pub tables: Vec<String>,
    pub user_version: i64,
    /// Modules/tokenizers unavailable here; quick_check cannot verify these tables.
    pub unavailable_virtual_tables: Vec<VirtualTableIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualTableIssue {
    pub table: String,
    pub reason: String,
}

#[derive(Debug)]
pub struct SnapshotReport {
    /// Copied intact, but their module-specific integrity could not be verified.
    pub unavailable_virtual_tables: Vec<VirtualTableIssue>,
}

/// SQLite storage classes. Text returned by SQLite is kept as bytes without lossy decoding.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRecord {
    /// All columns in schema order; no path normalization, deduplication, or defaults.
    pub columns: Vec<(String, SqlValue)>,
}

/// A private, stable source held exclusively by the host, tied to its guard.
///
/// This token is neither `Send` nor `Sync`; it does not itself verify ownership.
#[must_use]
pub struct PrivateSource<'guard> {
    path: PathBuf,
    _guard: std::marker::PhantomData<(&'guard mut (), std::rc::Rc<()>)>,
}

impl<'guard> PrivateSource<'guard> {
    /// # Safety
    /// The host must establish and maintain exclusive ownership of a stable,
    /// private database copy and its complete SQLite sidecar set for the entire
    /// lifetime of this value. A missing or malformed candidate is allowed only
    /// in an equally exclusive private location. The host must first stop all
    /// writers and preserve a coherent source file set; an active file set
    /// cannot be independently copied as a recovery point. No other SQLite/VFS
    /// connection, raw file
    /// handle, or locking code in this or any other process may use the private
    /// source inode or its sidecars. The host must prevent path replacement,
    /// hard-link changes, and concurrent use of the output/sidecar names. The
    /// supplied guard must retain the host's real ownership for this lifetime.
    /// The guard's type is not proof: this crate does not verify these conditions,
    /// acquire a cross-runtime lock, or create the private copy itself.
    ///
    /// The opt-in requires an explicit unsafe attestation:
    ///
    /// ```compile_fail
    /// use uebox_data::PrivateSource;
    /// let mut guard = ();
    /// let source = PrivateSource::from_host_verified_copy("private.db", &mut guard);
    /// # let _ = source;
    /// ```
    ///
    /// A guard cannot be mutably borrowed again while the source is in use:
    ///
    /// ```compile_fail
    /// use uebox_data::PrivateSource;
    /// let mut guard = ();
    /// let mut source = unsafe { PrivateSource::from_host_verified_copy("private.db", &mut guard) };
    /// let second_borrow = &mut guard;
    /// let _ = source.inspect();
    /// let _ = second_borrow;
    /// ```
    pub unsafe fn from_host_verified_copy<T: ?Sized>(
        path: impl AsRef<Path>,
        _guard: &'guard mut T,
    ) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            _guard: std::marker::PhantomData,
        }
    }

    pub fn inspect(&mut self) -> Result<DatabaseInspection, DataError> {
        inspect_private(&self.path)
    }

    pub fn read_projects(&mut self) -> Result<Vec<ProjectRecord>, DataError> {
        read_projects_private(&self.path)
    }

    pub fn create_snapshot(
        &mut self,
        destination: impl AsRef<Path>,
    ) -> Result<SnapshotReport, DataError> {
        create_snapshot_private(&self.path, destination.as_ref())
    }
}

impl ProjectRecord {
    pub fn get(&self, name: &str) -> Option<&SqlValue> {
        self.columns
            .iter()
            .find(|(column, _)| column == name)
            .map(|(_, value)| value)
    }
}

fn failure(kind: ErrorKind, message: impl Into<String>) -> DataError {
    DataError {
        kind,
        message: message.into(),
    }
}

fn io_error(context: &str, error: io::Error) -> DataError {
    failure(ErrorKind::Io, format!("{context}: {error}"))
}

fn sqlite_error(context: &str, error: rusqlite::Error) -> DataError {
    let kind = match &error {
        rusqlite::Error::SqliteFailure(code, message) => {
            if message.as_deref().is_some_and(|message| {
                let message = message.to_ascii_lowercase();
                message.contains("no such module:")
                    || message.contains("unknown tokenizer:")
                    || message.contains("no such tokenizer:")
                    || message.contains("no such collation sequence:")
                    || message.contains("no such function:")
            }) {
                ErrorKind::Unsupported
            } else {
                match code.code {
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => ErrorKind::Locked,
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => ErrorKind::Corrupt,
                    ErrorCode::PermissionDenied
                    | ErrorCode::ReadOnly
                    | ErrorCode::SystemIoFailure
                    | ErrorCode::CannotOpen
                    | ErrorCode::DiskFull => ErrorKind::Io,
                    _ => ErrorKind::Other,
                }
            }
        }
        _ => ErrorKind::Other,
    };
    failure(kind, format!("{context}: {error}"))
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceIdentity {
    device: u64,
    inode: u64,
}

#[cfg(not(unix))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceIdentity;

struct Source {
    connection: Connection,
    path: PathBuf,
    identity: SourceIdentity,
}

#[cfg(unix)]
fn source_identity(metadata: &fs::Metadata) -> Result<SourceIdentity, DataError> {
    use std::os::unix::fs::MetadataExt;
    if metadata.nlink() != 1 {
        return Err(failure(ErrorKind::Unsupported, "source has multiple or unlinked hard-link names; its WAL identity cannot be established"));
    }
    Ok(SourceIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn source_identity(_metadata: &fs::Metadata) -> Result<SourceIdentity, DataError> {
    Err(failure(
        ErrorKind::Unsupported,
        "source file identity and hard-link count are not verified on this platform",
    ))
}

impl Source {
    fn verify_identity(&self) -> Result<(), DataError> {
        let metadata =
            fs::metadata(&self.path).map_err(|error| io_error("recheck source identity", error))?;
        if source_identity(&metadata)? != self.identity {
            return Err(failure(
                ErrorKind::Locked,
                "source file identity changed during inspection",
            ));
        }
        let mut moved: i32 = 0;
        let rc = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                self.connection.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut i32).cast(),
            )
        };
        if rc != rusqlite::ffi::SQLITE_OK {
            return Err(failure(
                ErrorKind::Unsupported,
                format!("SQLite VFS cannot verify source identity: {rc}"),
            ));
        }
        if moved != 0 {
            return Err(failure(
                ErrorKind::Locked,
                "SQLite source was moved or replaced",
            ));
        }
        Ok(())
    }
}

fn open_source(path: &Path) -> Result<Source, DataError> {
    // SQLite's Unix VFS resolves symlinks before choosing the WAL name. Check
    // and open that same resolved path, or an alias could hide a damaged WAL.
    #[cfg(unix)]
    let resolved = fs::canonicalize(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            failure(
                ErrorKind::Missing,
                format!("source {}: {error}", path.display()),
            )
        } else {
            io_error(&format!("resolve source {}", path.display()), error)
        }
    })?;
    #[cfg(unix)]
    let path = resolved.as_path();
    // `metadata` uses stat on Unix and does not open/close another descriptor:
    // closing an arbitrary descriptor for a SQLite inode releases process locks.
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            failure(
                ErrorKind::Missing,
                format!("source {}: {error}", path.display()),
            )
        } else {
            io_error(&format!("open source {}", path.display()), error)
        }
    })?;
    if !metadata.is_file() {
        return Err(failure(ErrorKind::Io, "source is not a regular file"));
    }
    let identity = source_identity(&metadata)?;
    if metadata.len() < 100 {
        return Err(failure(
            ErrorKind::Corrupt,
            "source has no valid SQLite database header",
        ));
    }
    validate_source_sidecars(path)?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| sqlite_error("open source read-only", error))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|error| sqlite_error("disable source checkpoint on close", error))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| sqlite_error("configure source timeout", error))?;
    let source = Source {
        connection,
        path: path.to_path_buf(),
        identity,
    };
    source.verify_identity()?;
    source
        .connection
        .execute_batch("BEGIN")
        .map_err(|error| sqlite_error("begin source read", error))?;
    // Acquiring a read transaction first pins SQLite's committed view.
    source
        .connection
        .query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| sqlite_error("pin source read", error))?;
    source.verify_identity()?;
    validate_source_sidecars(&source.path)?;
    let header = read_sqlite_header(&source.connection)?;
    if &header[..16] != SQLITE_HEADER {
        return Err(failure(
            ErrorKind::Corrupt,
            "source has no valid SQLite database header",
        ));
    }
    let raw_page_size = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if raw_page_size == 1 {
        65_536
    } else {
        u32::from(raw_page_size)
    };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(failure(
            ErrorKind::Corrupt,
            "source has an invalid SQLite page size",
        ));
    }
    check_wal(&source.connection, &source.path, page_size, false)?;

    check_integrity(&source.connection)?;
    check_wal(&source.connection, &source.path, page_size, true)?;
    source.verify_identity()?;
    Ok(source)
}

fn validate_source_sidecars(database: &Path) -> Result<(), DataError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let path = sidecar_path(database, suffix);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(failure(
                        ErrorKind::Unsupported,
                        format!("source {suffix} is not a regular file"),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.nlink() != 1 {
                        return Err(failure(
                            ErrorKind::Unsupported,
                            format!("source {suffix} has multiple hard links"),
                        ));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("inspect source sidecar", error)),
        }
    }
    Ok(())
}

fn read_sqlite_header(connection: &Connection) -> Result<[u8; 100], DataError> {
    let file = sqlite_vfs_file(connection, rusqlite::ffi::SQLITE_FCNTL_FILE_POINTER)?;
    let mut header = [0_u8; 100];
    file.read_exact_at(0, &mut header, ErrorKind::Corrupt)?;
    Ok(header)
}

struct SqliteVfsFile<'a> {
    file: *mut rusqlite::ffi::sqlite3_file,
    // The pointer is owned by this live connection and never escapes it.
    _connection: &'a Connection,
}

fn sqlite_vfs_file(
    connection: &Connection,
    operation: i32,
) -> Result<SqliteVfsFile<'_>, DataError> {
    use rusqlite::ffi;
    let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
    // SQLite owns the descriptor; calls below neither close nor transfer it.
    let rc = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            operation,
            (&mut file as *mut *mut ffi::sqlite3_file).cast(),
        )
    };
    if rc != ffi::SQLITE_OK || file.is_null() || unsafe { (*file).pMethods.is_null() } {
        return Err(failure(
            ErrorKind::Unsupported,
            format!("cannot access SQLite VFS file: {rc}"),
        ));
    }
    Ok(SqliteVfsFile {
        file,
        _connection: connection,
    })
}

impl SqliteVfsFile<'_> {
    fn size(&self) -> Result<u64, DataError> {
        let methods = unsafe { &*(*self.file).pMethods };
        let size = methods
            .xFileSize
            .ok_or_else(|| failure(ErrorKind::Unsupported, "SQLite VFS cannot read file size"))?;
        let mut length = 0_i64;
        let rc = unsafe { size(self.file, &mut length) };
        if rc != rusqlite::ffi::SQLITE_OK || length < 0 {
            return Err(failure(
                ErrorKind::Io,
                format!("read SQLite VFS file size: {rc}"),
            ));
        }
        Ok(length as u64)
    }

    fn read_exact_at(
        &self,
        offset: u64,
        bytes: &mut [u8],
        short_kind: ErrorKind,
    ) -> Result<(), DataError> {
        let methods = unsafe { &*(*self.file).pMethods };
        let read = methods
            .xRead
            .ok_or_else(|| failure(ErrorKind::Unsupported, "SQLite VFS cannot read file"))?;
        let offset = i64::try_from(offset)
            .map_err(|_| failure(ErrorKind::Corrupt, "SQLite VFS offset is too large"))?;
        let length = i32::try_from(bytes.len())
            .map_err(|_| failure(ErrorKind::Other, "SQLite VFS read is too large"))?;
        let rc = unsafe { read(self.file, bytes.as_mut_ptr().cast(), length, offset) };
        if rc != rusqlite::ffi::SQLITE_OK {
            let kind = if rc == rusqlite::ffi::SQLITE_IOERR_SHORT_READ {
                short_kind
            } else {
                ErrorKind::Io
            };
            return Err(failure(kind, format!("read SQLite VFS file: {rc}")));
        }
        Ok(())
    }
}

// SQLite may ignore a malformed WAL and present an older main database. Check
// its VFS-owned handle, including current-generation frames, before success.
fn check_wal(
    connection: &Connection,
    database: &Path,
    database_page_size: u32,
    frames: bool,
) -> Result<(), DataError> {
    let path = sidecar_path(database, "-wal");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("stat source WAL", error)),
    };
    if !metadata.is_file() {
        return Err(failure(
            ErrorKind::Unsupported,
            "source WAL is not a regular file",
        ));
    }
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(|error| sqlite_error("read source journal mode", error))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(failure(
            ErrorKind::AmbiguousWalTail,
            "source has a WAL file but SQLite did not open the database in WAL mode",
        ));
    }
    let wal = sqlite_vfs_file(connection, rusqlite::ffi::SQLITE_FCNTL_JOURNAL_POINTER)?;
    let length = wal.size()?;
    if length == 0 {
        return Ok(());
    }
    if length < 32 {
        return Err(failure(
            ErrorKind::Corrupt,
            "source WAL has a truncated header",
        ));
    }
    let mut header = [0_u8; 32];
    wal.read_exact_at(0, &mut header, ErrorKind::Locked)?;
    let word = |offset: usize| {
        u32::from_be_bytes(
            header[offset..offset + 4]
                .try_into()
                .expect("four-byte WAL word"),
        )
    };
    let magic = word(0);
    let page_size = word(8);
    if magic & !1 != 0x377f_0682 || page_size != database_page_size {
        return Err(failure(
            ErrorKind::Corrupt,
            "source WAL has invalid magic or page size",
        ));
    }
    let big_endian_checksum = magic & 1 != 0;
    let mut checksum = (0_u32, 0_u32);
    update_wal_checksum(&header[..24], big_endian_checksum, &mut checksum);
    if checksum != (word(24), word(28)) {
        return Err(failure(
            ErrorKind::Corrupt,
            "source WAL header checksum does not match",
        ));
    }
    if word(4) != 3_007_000 {
        return Err(failure(
            ErrorKind::Unsupported,
            "source WAL uses an unsupported format version",
        ));
    }
    if !frames {
        return Ok(());
    }

    let frame_size = u64::from(page_size) + 24;
    let bytes_after_header = length - 32;
    let mut frame_header = [0_u8; 24];
    let mut page = vec![0_u8; page_size as usize];
    for index in 0..bytes_after_header / frame_size {
        let offset = 32 + index * frame_size;
        wal.read_exact_at(offset, &mut frame_header, ErrorKind::Locked)?;
        // Old-generation leftovers and damaged salts are indistinguishable
        // from the file alone. Do not silently publish a potentially older view.
        if frame_header[8..16] != header[16..24] {
            return Err(failure(
                ErrorKind::AmbiguousWalTail,
                "source WAL contains a different-salt frame: it may be an older generation or damaged data; preserve the original and inspect it offline",
            ));
        }
        wal.read_exact_at(offset + 24, &mut page, ErrorKind::Locked)?;
        let page_number =
            u32::from_be_bytes(frame_header[..4].try_into().expect("WAL page number"));
        if page_number == 0 {
            return Err(failure(
                ErrorKind::Corrupt,
                "source WAL frame has page number zero",
            ));
        }
        update_wal_checksum(&frame_header[..8], big_endian_checksum, &mut checksum);
        update_wal_checksum(&page, big_endian_checksum, &mut checksum);
        let stored = (
            u32::from_be_bytes(frame_header[16..20].try_into().expect("WAL frame checksum")),
            u32::from_be_bytes(frame_header[20..24].try_into().expect("WAL frame checksum")),
        );
        if checksum != stored {
            return Err(failure(
                ErrorKind::Corrupt,
                "source WAL frame checksum does not match",
            ));
        }
    }
    if bytes_after_header % frame_size != 0 {
        return Err(failure(
            ErrorKind::Locked,
            "source WAL has an incomplete tail or is being written; retry after stopping writers",
        ));
    }
    Ok(())
}

fn update_wal_checksum(bytes: &[u8], big_endian: bool, checksum: &mut (u32, u32)) {
    // Keep the pre-1.88 API and preserve chunks_exact's remainder behavior.
    for pair in bytes.chunks(8).filter(|pair| pair.len() == 8) {
        let first: [u8; 4] = pair[..4].try_into().expect("four-byte WAL checksum word");
        let second: [u8; 4] = pair[4..].try_into().expect("four-byte WAL checksum word");
        let decode = |bytes| {
            if big_endian {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            }
        };
        checksum.0 = checksum
            .0
            .wrapping_add(decode(first))
            .wrapping_add(checksum.1);
        checksum.1 = checksum
            .1
            .wrapping_add(decode(second))
            .wrapping_add(checksum.0);
    }
}

fn check_integrity(connection: &Connection) -> Result<(), DataError> {
    let mut statement = connection
        .prepare("PRAGMA quick_check")
        .map_err(|error| sqlite_error("prepare integrity check", error))?;
    let mut rows = statement
        .query([])
        .map_err(|error| sqlite_error("run integrity check", error))?;
    let mut checked = false;
    while let Some(row) = rows
        .next()
        .map_err(|error| sqlite_error("read integrity check", error))?
    {
        checked = true;
        let result: String = row
            .get(0)
            .map_err(|error| sqlite_error("read integrity result", error))?;
        if result != "ok" {
            return Err(failure(
                ErrorKind::Corrupt,
                format!("SQLite quick_check: {result}"),
            ));
        }
    }
    if !checked {
        return Err(failure(
            ErrorKind::Other,
            "SQLite quick_check returned no result",
        ));
    }
    Ok(())
}

fn unavailable_virtual_tables(
    connection: &Connection,
) -> Result<Vec<VirtualTableIssue>, DataError> {
    let mut statement = connection.prepare(
        "SELECT name FROM pragma_table_list WHERE schema = 'main' AND type = 'virtual' ORDER BY name",
    ).map_err(|error| sqlite_error("list virtual tables", error))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| sqlite_error("list virtual tables", error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| sqlite_error("read virtual table name", error))?;
    let mut unavailable = Vec::new();
    for name in names {
        // Identifiers originate in the database schema, so quote even hostile names.
        let sql = format!(
            "SELECT * FROM main.\"{}\" LIMIT 0",
            name.replace('"', "\"\"")
        );
        if let Err(error) = connection.prepare(&sql) {
            let diagnostic = sqlite_error(&format!("inspect virtual table {name}"), error);
            if diagnostic.kind != ErrorKind::Unsupported {
                return Err(diagnostic);
            }
            unavailable.push(VirtualTableIssue {
                table: name,
                reason: diagnostic.message,
            });
        }
    }
    Ok(unavailable)
}

/// The unrestricted path entrypoint is disabled; it never opens or evaluates the source.
/// The caller retains ownership and must keep any file-holding wrapper alive
/// for as long as its locks require; this function does not drop the wrapper.
///
/// Owned paths cannot be transferred to this entrypoint:
///
/// ```compile_fail
/// use std::path::PathBuf;
/// use uebox_data::inspect_database;
/// fn reject_owned(path: PathBuf) { let _ = inspect_database(path); }
/// ```
pub fn inspect_database<P: AsRef<Path> + ?Sized>(
    _path: &P,
) -> Result<DatabaseInspection, DataError> {
    Err(failure(
        ErrorKind::ExclusiveAccessRequired,
        "a host-verified exclusive private copy is required",
    ))
}

fn inspect_private(path: &Path) -> Result<DatabaseInspection, DataError> {
    let source = open_source(path)?;
    let connection = &source.connection;
    let unavailable_virtual_tables = unavailable_virtual_tables(connection)?;
    let user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sqlite_error("read user_version", error))?;
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .map_err(|error| sqlite_error("list tables", error))?;
    let tables = statement
        .query_map([], |row| row.get(0))
        .map_err(|error| sqlite_error("list tables", error))?
        .collect::<rusqlite::Result<Vec<String>>>()
        .map_err(|error| sqlite_error("read table name", error))?;
    source.verify_identity()?;
    Ok(DatabaseInspection {
        tables,
        user_version,
        unavailable_virtual_tables,
    })
}

/// The unrestricted path entrypoint is disabled; it never opens or evaluates the source.
/// The caller retains ownership and must keep any file-holding wrapper alive
/// for as long as its locks require; this function does not drop the wrapper.
///
/// Owned paths cannot be transferred to this entrypoint:
///
/// ```compile_fail
/// use std::path::PathBuf;
/// use uebox_data::read_projects;
/// fn reject_owned(path: PathBuf) { let _ = read_projects(path); }
/// ```
pub fn read_projects<P: AsRef<Path> + ?Sized>(_path: &P) -> Result<Vec<ProjectRecord>, DataError> {
    Err(failure(
        ErrorKind::ExclusiveAccessRequired,
        "a host-verified exclusive private copy is required",
    ))
}

fn read_projects_private(path: &Path) -> Result<Vec<ProjectRecord>, DataError> {
    let source = open_source(path)?;
    let connection = &source.connection;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'projects' COLLATE NOCASE)",
        [], |row| row.get(0),
    ).map_err(|error| sqlite_error("find projects table", error))?;
    if !exists {
        return Err(failure(
            ErrorKind::MissingProjectsTable,
            "projects table is absent",
        ));
    }
    let mut metadata = connection
        .prepare("SELECT name FROM pragma_table_xinfo('projects') WHERE hidden != 1 ORDER BY cid")
        .map_err(|error| sqlite_error("prepare project column metadata", error))?;
    let mut metadata_rows = metadata
        .query([])
        .map_err(|error| sqlite_error("query project column metadata", error))?;
    let mut names = Vec::new();
    while let Some(row) = metadata_rows
        .next()
        .map_err(|error| sqlite_error("read project column metadata", error))?
    {
        let bytes = match row
            .get_ref(0)
            .map_err(|error| sqlite_error("read project column name", error))?
        {
            ValueRef::Text(bytes) => bytes,
            _ => {
                return Err(failure(
                    ErrorKind::Unsupported,
                    "project column name is not SQLite text",
                ))
            }
        };
        let name = std::str::from_utf8(bytes).map_err(|_| {
            failure(
                ErrorKind::Unsupported,
                "project column name is not valid UTF-8",
            )
        })?;
        names.push(name.to_owned());
    }
    let mut statement = connection
        .prepare("SELECT * FROM projects")
        .map_err(|error| sqlite_error("prepare projects", error))?;
    if names.len() != statement.column_count() {
        return Err(failure(
            ErrorKind::Unsupported,
            "project column metadata does not match the query",
        ));
    }
    let mut rows = statement
        .query([])
        .map_err(|error| sqlite_error("query projects", error))?;
    let mut projects = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| sqlite_error("read project row", error))?
    {
        let mut columns = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            let value = match row
                .get_ref(index)
                .map_err(|error| sqlite_error("read project value", error))?
            {
                ValueRef::Null => SqlValue::Null,
                ValueRef::Integer(value) => SqlValue::Integer(value),
                ValueRef::Real(value) => SqlValue::Real(value),
                ValueRef::Text(value) => SqlValue::Text(value.to_vec()),
                ValueRef::Blob(value) => SqlValue::Blob(value.to_vec()),
            };
            columns.push((name.clone(), value));
        }
        projects.push(ProjectRecord { columns });
    }
    source.verify_identity()?;
    Ok(projects)
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn reject_source_conflict(source: &Path, destination: &Path) -> Result<(), DataError> {
    let name = destination
        .file_name()
        .ok_or_else(|| failure(ErrorKind::Io, "destination must name a file"))?
        .to_string_lossy();
    // Reserve SQLite sidecar suffixes in every directory. This also handles
    // parent symlinks, case-folding filesystems, and sidecars of other DBs.
    let name = name.trim_end_matches([' ', '.']).to_ascii_lowercase();
    if ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return Err(failure(
            ErrorKind::SourceConflict,
            "destination uses a reserved SQLite sidecar name",
        ));
    }
    if let (Ok(source), Ok(destination)) = (fs::metadata(source), fs::metadata(destination)) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if source.dev() == destination.dev() && source.ino() == destination.ino() {
                return Err(failure(
                    ErrorKind::SourceConflict,
                    "destination identifies the source database",
                ));
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (source, destination);
            return Err(failure(
                ErrorKind::Unsupported,
                "source and destination identity cannot be verified on this platform",
            ));
        }
    }
    Ok(())
}

fn reject_existing_destination(path: &Path) -> Result<(), DataError> {
    for candidate in [
        path.to_path_buf(),
        sidecar_path(path, "-wal"),
        sidecar_path(path, "-shm"),
        sidecar_path(path, "-journal"),
    ] {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                return Err(failure(
                    ErrorKind::DestinationExists,
                    format!(
                        "destination or SQLite sidecar exists: {}",
                        candidate.display()
                    ),
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("check destination", error)),
        }
    }
    Ok(())
}

/// The unrestricted path entrypoint is disabled; it never opens or evaluates either path.
/// The caller retains ownership of both arguments and must keep any file-holding
/// wrapper alive for as long as its locks require; this function drops neither.
///
/// Neither argument can transfer an owned path to this entrypoint:
///
/// ```compile_fail
/// use std::path::{Path, PathBuf};
/// use uebox_data::create_snapshot;
/// fn reject_owned_source(source: PathBuf) {
///     let _ = create_snapshot(source, Path::new("snapshot.db"));
/// }
/// ```
///
/// ```compile_fail
/// use std::path::{Path, PathBuf};
/// use uebox_data::create_snapshot;
/// fn reject_owned_destination(destination: PathBuf) {
///     let _ = create_snapshot(Path::new("source.db"), destination);
/// }
/// ```
pub fn create_snapshot<S: AsRef<Path> + ?Sized, D: AsRef<Path> + ?Sized>(
    _source: &S,
    _destination: &D,
) -> Result<SnapshotReport, DataError> {
    Err(failure(
        ErrorKind::ExclusiveAccessRequired,
        "a host-verified exclusive private copy is required",
    ))
}

fn create_snapshot_private(
    source_path: &Path,
    destination: &Path,
) -> Result<SnapshotReport, DataError> {
    if destination.file_name().is_none() {
        return Err(failure(ErrorKind::Io, "destination must name a file"));
    }
    reject_source_conflict(source_path, destination)?;
    reject_existing_destination(destination)?;
    let source = open_source(source_path)?;
    let unavailable_virtual_tables = unavailable_virtual_tables(&source.connection)?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".uebox-snapshot-incomplete-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder
        .tempdir_in(parent)
        .map_err(|error| io_error("create snapshot staging directory", error))?;
    let staged_path = staging.path().join("snapshot.db");
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
            .map_err(|error| io_error("create staged snapshot", error))?,
    );
    let mut target = Connection::open_with_flags(
        &staged_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| sqlite_error("open staged snapshot", error))?;
    target
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| sqlite_error("configure snapshot timeout", error))?;
    {
        let backup = Backup::new(&source.connection, &mut target)
            .map_err(|error| sqlite_error("initialize SQLite backup", error))?;
        match backup
            .step(-1)
            .map_err(|error| sqlite_error("copy SQLite backup", error))?
        {
            StepResult::Done => {}
            StepResult::Busy | StepResult::Locked => {
                return Err(failure(
                    ErrorKind::Locked,
                    "SQLite backup could not acquire a lock",
                ))
            }
            _ => return Err(failure(ErrorKind::Other, "SQLite backup did not complete")),
        }
    }
    // A standalone output must not depend on staging WAL/SHM files.
    let mode: String = target
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .map_err(|error| sqlite_error("make snapshot standalone", error))?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(failure(
            ErrorKind::Other,
            format!("snapshot journal mode is {mode}, not delete"),
        ));
    }
    check_integrity(&target)?;
    if unavailable_virtual_tables != self::unavailable_virtual_tables(&target)? {
        return Err(failure(
            ErrorKind::Other,
            "source and snapshot virtual-table availability differ",
        ));
    }
    target
        .close()
        .map_err(|(_, error)| sqlite_error("close staged snapshot", error))?;
    for suffix in ["-wal", "-shm", "-journal"] {
        match fs::symlink_metadata(sidecar_path(&staged_path, suffix)) {
            Ok(_) => {
                return Err(failure(
                    ErrorKind::Other,
                    format!("staged snapshot still has {suffix}"),
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("check staged sidecars", error)),
        }
    }
    // Windows FlushFileBuffers requires a handle opened with write access.
    OpenOptions::new()
        .write(true)
        .open(&staged_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| io_error("sync staged snapshot", error))?;
    // Recheck sidecars immediately before publication; callers must also avoid
    // concurrent use of this destination name.
    source.verify_identity()?;
    reject_source_conflict(&source.path, destination)?;
    reject_existing_destination(destination)?;
    fs::hard_link(&staged_path, destination).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            failure(
                ErrorKind::DestinationExists,
                format!("destination already exists: {}", destination.display()),
            )
        } else {
            io_error("publish snapshot without overwriting", error)
        }
    })?;
    // No fallible operation follows publication: an error must not leave a
    // successful-looking output. TempDir's best-effort cleanup removes staging.
    Ok(SnapshotReport {
        unavailable_virtual_tables,
    })
}

#[cfg(all(test, unix))]
mod identity_tests {
    use super::*;

    #[test]
    fn detects_hardlink_added_after_source_was_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch("CREATE TABLE projects (projectKey TEXT)")
            .unwrap();
        drop(writer);
        let source = open_source(&path).unwrap();
        fs::hard_link(&path, dir.path().join("alias.db")).unwrap();
        assert_eq!(
            source.verify_identity().unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn detects_source_replaced_after_it_was_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.db");
        let replacement = dir.path().join("replacement.db");
        for candidate in [&path, &replacement] {
            let writer = Connection::open(candidate).unwrap();
            writer
                .execute_batch("CREATE TABLE projects (projectKey TEXT)")
                .unwrap();
        }
        let source = open_source(&path).unwrap();
        fs::rename(&path, dir.path().join("original.db")).unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(
            source.verify_identity().unwrap_err().kind,
            ErrorKind::Locked
        );
    }
}
