#![cfg(unix)]

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier, Mutex};

use rusqlite::{params, Connection};
use tempfile::TempDir;
use uebox_data::{
    create_snapshot as default_snapshot, inspect_database as default_inspect,
    read_projects as default_projects, DataError, DatabaseInspection, ErrorKind, PrivateSource,
    ProjectRecord, SnapshotReport, SqlValue,
};

// Each call site must close every source connection before using these helpers.
// Fixtures are isolated temporary files with no other access to their inode.
fn inspect_database(path: impl AsRef<std::path::Path>) -> Result<DatabaseInspection, DataError> {
    let mut guard = ();
    unsafe { PrivateSource::from_host_verified_copy(path, &mut guard) }.inspect()
}

fn read_projects(path: impl AsRef<std::path::Path>) -> Result<Vec<ProjectRecord>, DataError> {
    let mut guard = ();
    unsafe { PrivateSource::from_host_verified_copy(path, &mut guard) }.read_projects()
}

fn create_snapshot(
    source: impl AsRef<std::path::Path>,
    destination: impl AsRef<std::path::Path>,
) -> Result<SnapshotReport, DataError> {
    let mut guard = ();
    unsafe { PrivateSource::from_host_verified_copy(source, &mut guard) }
        .create_snapshot(destination)
}

#[test]
fn lock_competitor_process() {
    let Some(path) = std::env::var_os("UEBOX_DATA_LOCK_PROBE_PATH") else {
        return;
    };
    let command = std::env::var("UEBOX_DATA_LOCK_PROBE_SQL").unwrap();
    let should_block = std::env::var("UEBOX_DATA_LOCK_PROBE_BLOCKED").unwrap() == "yes";
    let connection = Connection::open(path).unwrap();
    connection.busy_timeout(std::time::Duration::ZERO).unwrap();
    let result = connection.execute_batch(&command);
    if should_block {
        assert!(
            matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)),
            "external write unexpectedly acquired lock"
        );
    } else {
        result.unwrap();
    }
}

fn external_lock_probe(path: &std::path::Path, sql: &str, should_block: bool) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_competitor_process", "--nocapture"])
        .env("UEBOX_DATA_LOCK_PROBE_PATH", path)
        .env("UEBOX_DATA_LOCK_PROBE_SQL", sql)
        .env(
            "UEBOX_DATA_LOCK_PROBE_BLOCKED",
            if should_block { "yes" } else { "no" },
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "external lock probe failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn default_entrypoints_refuse_while_same_process_read_lock_is_held() {
    for operation in ["inspect", "projects", "snapshot"] {
        let dir = tempfile::tempdir().unwrap();
        let (source, reader) = database(&dir, "source.db");
        reader.execute_batch("CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('one'); BEGIN; SELECT * FROM projects;").unwrap();
        let probe = "BEGIN EXCLUSIVE; UPDATE projects SET projectKey = 'unexpected'; COMMIT;";
        external_lock_probe(&source, probe, true);
        match operation {
            "inspect" => {
                assert_eq!(
                    default_inspect(&source).unwrap_err().kind,
                    ErrorKind::ExclusiveAccessRequired
                );
            }
            "projects" => {
                assert_eq!(
                    default_projects(&source).unwrap_err().kind,
                    ErrorKind::ExclusiveAccessRequired
                );
            }
            "snapshot" => {
                assert_eq!(
                    default_snapshot(&source, &dir.path().join("snapshot.db"))
                        .unwrap_err()
                        .kind,
                    ErrorKind::ExclusiveAccessRequired
                );
            }
            _ => unreachable!(),
        }
        external_lock_probe(&source, probe, true);
        reader.execute_batch("COMMIT").unwrap();
    }
}

#[cfg(unix)]
#[test]
fn default_rejection_does_not_release_a_read_lock_with_symlinked_wal() {
    let dir = tempfile::tempdir().unwrap();
    let (source, reader) = database(&dir, "source.db");
    reader.execute_batch("CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('one'); BEGIN; SELECT * FROM projects;").unwrap();
    let probe = "BEGIN EXCLUSIVE; UPDATE projects SET projectKey = 'unexpected'; COMMIT;";
    external_lock_probe(&source, probe, true);
    let wal = dir.path().join("source.db-wal");
    std::os::unix::fs::symlink(&source, &wal).unwrap();
    assert_eq!(
        default_inspect(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    fs::remove_file(wal).unwrap();
    external_lock_probe(&source, probe, true);
    reader.execute_batch("COMMIT").unwrap();
}

#[cfg(unix)]
#[test]
fn default_rejection_preserves_same_process_write_lock_and_both_updates() {
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer.execute_batch("CREATE TABLE projects (a INTEGER, b INTEGER); INSERT INTO projects VALUES (0,0); BEGIN IMMEDIATE; UPDATE projects SET a=1;").unwrap();
    let probe = "BEGIN IMMEDIATE; UPDATE projects SET b=2; COMMIT;";
    external_lock_probe(&source, probe, true);
    assert_eq!(
        default_inspect(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    external_lock_probe(&source, probe, true);
    writer.execute_batch("COMMIT").unwrap();
    external_lock_probe(&source, probe, false);
    let pair: (i64, i64) = writer
        .query_row("SELECT a,b FROM projects", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(pair, (1, 2));
}

#[test]
fn wal_holder_process() {
    let Some(path) = std::env::var_os("UEBOX_DATA_WAL_HOLDER_PATH") else {
        return;
    };
    let marker = std::env::var_os("UEBOX_DATA_WAL_HOLDER_READY").unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
         PRAGMA wal_autocheckpoint=0;
         CREATE TABLE projects (projectKey TEXT);
         INSERT INTO projects VALUES ('baseline');
         PRAGMA wal_checkpoint(TRUNCATE);
         INSERT INTO projects VALUES ('committed-in-wal');",
        )
        .unwrap();
    fs::write(marker, b"ready").unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}

struct TestChild(Child);

impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
#[test]
fn hardlink_alias_is_rejected_after_wal_holder_exits() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.db");
    let marker = dir.path().join("ready");
    let mut child = TestChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "wal_holder_process", "--nocapture"])
            .env("UEBOX_DATA_WAL_HOLDER_PATH", &source)
            .env("UEBOX_DATA_WAL_HOLDER_READY", &marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = std::time::Instant::now();
    while !marker.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "WAL holder exited before ready"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "WAL holder did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        default_projects(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    child.0.stdin.take().unwrap().write_all(b"stop\n").unwrap();
    assert!(child.0.wait().unwrap().success());
    assert_eq!(read_projects(&source).unwrap().len(), 2);
    let alias = dir.path().join("alias.db");
    fs::hard_link(&source, &alias).unwrap();
    assert_eq!(
        inspect_database(&alias).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        read_projects(&alias).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    let snapshot = dir.path().join("snapshot.db");
    assert_eq!(
        create_snapshot(&alias, &snapshot).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        read_projects(&source).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert!(!snapshot.exists());
}

#[test]
fn snapshot_rejects_all_source_sidecar_names_and_source_itself() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch(
            "CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('before');",
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        create_snapshot(&source, &source).unwrap_err().kind,
        ErrorKind::SourceConflict
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        let destination = dir.path().join(format!("source.db{suffix}"));
        assert_eq!(
            create_snapshot(&source, &destination).unwrap_err().kind,
            ErrorKind::SourceConflict
        );
        assert!(!destination.exists());
    }
    assert_eq!(
        create_snapshot(&source, dir.path().join("unrelated.db-WAL"))
            .unwrap_err()
            .kind,
        ErrorKind::SourceConflict
    );
    let destination = dir.path().join("safe.db");
    create_snapshot(&source, &destination).unwrap();
    let before = fs::read(&destination).unwrap();
    let connection = Connection::open(&source).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL; INSERT INTO projects VALUES ('after');")
        .unwrap();
    drop(connection);
    assert_eq!(fs::read(destination).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn source_sidecar_through_a_symlinked_parent_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let actual = dir.path().join("actual");
    fs::create_dir(&actual).unwrap();
    let source = actual.join("source.db");
    let connection = Connection::open(&source).unwrap();
    connection
        .execute_batch("CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(connection);
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        assert_eq!(
            create_snapshot(
                alias.join("source.db"),
                alias.join(format!("source.db{suffix}"))
            )
            .unwrap_err()
            .kind,
            ErrorKind::SourceConflict
        );
        assert!(!actual.join(format!("source.db{suffix}")).exists());
    }
}

#[cfg(unix)]
#[test]
fn symlinked_source_reads_and_snapshots_the_real_wal() {
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
         PRAGMA wal_autocheckpoint=0;
         CREATE TABLE projects (projectKey TEXT);
         INSERT INTO projects VALUES ('baseline');
         PRAGMA wal_checkpoint(TRUNCATE);
         INSERT INTO projects VALUES ('wal-committed');",
        )
        .unwrap();
    writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    drop(writer);
    let alias = dir.path().join("source-alias.db");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let records = read_projects(&alias).unwrap();
    assert_eq!(records.len(), 2);
    let snapshot = dir.path().join("snapshot.db");
    create_snapshot(&alias, &snapshot).unwrap();
    assert_eq!(read_projects(snapshot).unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn default_project_read_does_not_release_an_existing_read_lock() {
    let dir = tempfile::tempdir().unwrap();
    let (source, reader) = database(&dir, "source.db");
    reader.execute_batch("CREATE TABLE notes (content TEXT); INSERT INTO notes VALUES ('keep'); BEGIN; SELECT * FROM notes;").unwrap();
    let probe = "BEGIN EXCLUSIVE; UPDATE notes SET content='unexpected'; COMMIT;";
    external_lock_probe(&source, probe, true);
    assert_eq!(
        default_projects(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    external_lock_probe(&source, probe, true);
    reader.execute_batch("COMMIT").unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn default_rejection_preserves_system_sqlite_locks() {
    use std::ffi::{c_char, c_int, c_void, CString};
    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }
    type Open = unsafe extern "C" fn(*const c_char, *mut *mut c_void) -> c_int;
    type Exec = unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *mut c_void,
        *mut c_void,
        *mut *mut c_char,
    ) -> c_int;
    type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
    struct Handle {
        library: *mut c_void,
        database: *mut c_void,
        close: Close,
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                (self.close)(self.database);
                dlclose(self.library);
            }
        }
    }
    for (begin, probe) in [
        (
            c"BEGIN; SELECT * FROM projects;",
            "BEGIN EXCLUSIVE; UPDATE projects SET projectKey='unexpected'; COMMIT;",
        ),
        (
            c"BEGIN IMMEDIATE; UPDATE projects SET projectKey='pending';",
            "BEGIN IMMEDIATE; UPDATE projects SET projectKey='unexpected'; COMMIT;",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (source, setup) = database(&dir, "source.db");
        setup
            .execute_batch(
                "CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('one')",
            )
            .unwrap();
        drop(setup);
        unsafe {
            let library = dlopen(c"/usr/lib/libsqlite3.dylib".as_ptr(), 2);
            assert!(!library.is_null());
            let symbol = |name: &std::ffi::CStr| {
                let ptr = dlsym(library, name.as_ptr());
                assert!(!ptr.is_null());
                ptr
            };
            let open: Open = std::mem::transmute(symbol(c"sqlite3_open"));
            let exec: Exec = std::mem::transmute(symbol(c"sqlite3_exec"));
            let close: Close = std::mem::transmute(symbol(c"sqlite3_close"));
            assert_ne!(
                open as *const (),
                rusqlite::ffi::sqlite3_open as *const (),
                "fixture must use a distinct SQLite library"
            );
            let path = CString::new(source.to_str().unwrap()).unwrap();
            let mut database = std::ptr::null_mut();
            assert_eq!(open(path.as_ptr(), &mut database), 0);
            let _handle = Handle {
                library,
                database,
                close,
            };
            assert_eq!(
                exec(
                    database,
                    begin.as_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                0
            );
            external_lock_probe(&source, probe, true);
            assert_eq!(
                default_inspect(&source).unwrap_err().kind,
                ErrorKind::ExclusiveAccessRequired
            );
            external_lock_probe(&source, probe, true);
        }
    }
}

#[test]
fn missing_fts_tokenizer_is_reported_as_unsupported_and_not_backed_up() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection.execute_batch(
        "CREATE TABLE projects (projectKey TEXT);
         CREATE VIRTUAL TABLE index_doc USING fts5(content);
         INSERT INTO index_doc VALUES ('kept text');
         PRAGMA writable_schema=ON;
         UPDATE sqlite_schema SET sql = 'CREATE VIRTUAL TABLE index_doc USING fts5(content, tokenize = ''not_installed'')' WHERE name='index_doc';
         PRAGMA writable_schema=OFF;"
    ).unwrap();
    drop(connection);
    let inspection_error = inspect_database(&source).unwrap_err();
    assert_eq!(inspection_error.kind, ErrorKind::Unsupported);
    assert!(inspection_error.message.contains("tokenizer"));
    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert!(!destination.exists());
    let connection =
        Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let content: String = connection
        .query_row("SELECT c0 FROM index_doc_content", [], |row| row.get(0))
        .unwrap();
    assert_eq!(content, "kept text");
}

#[test]
fn non_utf8_project_column_name_returns_an_error_instead_of_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    let sql = b"CREATE TABLE projects (\"\xff\" TEXT); INSERT INTO projects VALUES ('kept');\0";
    let result = unsafe {
        rusqlite::ffi::sqlite3_exec(
            connection.handle(),
            sql.as_ptr().cast(),
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, rusqlite::ffi::SQLITE_OK);
    drop(connection);
    let before = fs::read(&source).unwrap();
    let outcome = std::panic::catch_unwind(|| read_projects(&source));
    assert!(
        outcome.is_ok(),
        "read_projects panicked on a non-UTF-8 column name"
    );
    assert_eq!(outcome.unwrap().unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(fs::read(&source).unwrap(), before);
    let destination = dir.path().join("snapshot.db");
    create_snapshot(&source, &destination).unwrap();
    let snapshot =
        Connection::open_with_flags(destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let sql_bytes: Vec<u8> = snapshot
        .query_row(
            "SELECT CAST(sql AS BLOB) FROM sqlite_schema WHERE name='projects'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(sql_bytes.contains(&0xff));
}

fn database(dir: &TempDir, name: &str) -> (std::path::PathBuf, Connection) {
    let path = dir.path().join(name);
    let connection = Connection::open(&path).unwrap();
    (path, connection)
}

#[test]
fn snapshot_includes_committed_wal_rows_but_not_a_rolled_back_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE projects (id INTEGER PRIMARY KEY, projectKey TEXT, EngineAssociation TEXT, projectData TEXT);
             CREATE TABLE notes (id INTEGER PRIMARY KEY, content TEXT);
             INSERT INTO projects VALUES (1, 'ue-project', '5.5', '{\"FileVersion\":3}');
             INSERT INTO notes VALUES (8, '独有的正文');
             BEGIN;
             INSERT INTO projects VALUES (2, 'uncommitted', '5.6', '{}');",
        )
        .unwrap();
    let wal = dir.path().join("source.db-wal");
    assert!(wal.exists(), "fixture must retain committed data in WAL");
    writer.execute_batch("ROLLBACK").unwrap();
    writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    drop(writer);
    let main_before = fs::read(&source).unwrap();
    let wal_before = fs::read(&wal).unwrap();

    let destination = dir.path().join("snapshot.db");
    create_snapshot(&source, &destination).unwrap();
    assert_eq!(fs::read(&source).unwrap(), main_before);
    assert_eq!(fs::read(&wal).unwrap(), wal_before);

    let snapshot =
        Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let journal_mode: String = snapshot
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "delete");
    let projects: Vec<(i64, String)> = snapshot
        .prepare("SELECT id, projectKey FROM projects ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(projects, [(1, "ue-project".into())]);
    let note: String = snapshot
        .query_row("SELECT content FROM notes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(note, "独有的正文");
    drop(snapshot);
    let inspection = inspect_database(&destination).unwrap();
    assert!(inspection.tables.iter().any(|name| name == "notes"));
}

#[test]
fn empty_wal_after_a_checkpoint_is_not_treated_as_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE projects (projectKey TEXT);
             INSERT INTO projects VALUES ('checkpointed');
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    assert_eq!(
        fs::metadata(dir.path().join("source.db-wal"))
            .unwrap()
            .len(),
        0
    );
    writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    drop(writer);
    let destination = dir.path().join("snapshot.db");
    create_snapshot(&source, &destination).unwrap();
    assert_eq!(read_projects(destination).unwrap().len(), 1);
}

#[test]
fn closing_the_only_read_connection_does_not_checkpoint_or_remove_source_wal() {
    let dir = tempfile::tempdir().unwrap();
    let (live, writer) = database(&dir, "live.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
         PRAGMA wal_autocheckpoint=0;
         CREATE TABLE projects (projectKey TEXT);
         INSERT INTO projects VALUES ('committed-in-wal');",
        )
        .unwrap();
    let source = dir.path().join("inactive.db");
    let source_wal = dir.path().join("inactive.db-wal");
    fs::copy(live, &source).unwrap();
    fs::copy(dir.path().join("live.db-wal"), &source_wal).unwrap();
    let main_before = fs::read(&source).unwrap();
    let wal_before = fs::read(&source_wal).unwrap();

    let inspection = inspect_database(&source).unwrap();
    assert!(inspection.tables.iter().any(|name| name == "projects"));
    assert_eq!(fs::read(&source).unwrap(), main_before);
    assert_eq!(fs::read(&source_wal).unwrap(), wal_before);
    let destination = dir.path().join("snapshot.db");
    create_snapshot(&source, &destination).unwrap();
    assert_eq!(fs::read(source).unwrap(), main_before);
    assert_eq!(fs::read(source_wal).unwrap(), wal_before);
    assert_eq!(read_projects(destination).unwrap().len(), 1);
}

#[test]
fn existing_destination_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch("CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(connection);
    let destination = dir.path().join("snapshot.db");
    fs::write(&destination, b"existing data").unwrap();

    let error = create_snapshot(&source, &destination).unwrap_err();
    assert_eq!(error.kind, ErrorKind::DestinationExists);
    assert_eq!(fs::read(destination).unwrap(), b"existing data");
}

#[test]
fn stale_destination_sqlite_sidecars_are_not_reused() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch("CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(connection);
    for suffix in ["-wal", "-shm", "-journal"] {
        let destination = dir.path().join(format!("snapshot{suffix}.db"));
        let sidecar = dir.path().join(format!("snapshot{suffix}.db{suffix}"));
        fs::write(&sidecar, b"old sidecar").unwrap();
        let error = create_snapshot(&source, &destination).unwrap_err();
        assert_eq!(error.kind, ErrorKind::DestinationExists);
        assert!(!destination.exists());
        assert_eq!(fs::read(sidecar).unwrap(), b"old sidecar");
    }
}

#[test]
fn corrupt_or_empty_source_fails_without_publishing_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("empty.db", &b""[..]),
        ("corrupt.db", &b"not a sqlite database"[..]),
    ] {
        let source = dir.path().join(name);
        let destination = dir.path().join(format!("{name}.snapshot"));
        fs::write(&source, bytes).unwrap();
        assert_eq!(
            inspect_database(&source).unwrap_err().kind,
            ErrorKind::Corrupt
        );
        assert_eq!(
            create_snapshot(&source, &destination).unwrap_err().kind,
            ErrorKind::Corrupt
        );
        assert!(!destination.exists());
        assert_eq!(fs::read(source).unwrap(), bytes);
    }
}

#[test]
fn missing_source_is_reported_and_never_created() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("missing.db");
    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        inspect_database(&source).unwrap_err().kind,
        ErrorKind::Missing
    );
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::Missing
    );
    assert!(!source.exists());
    assert!(!destination.exists());
}

#[test]
fn default_rejection_precedes_locked_database_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer
        .execute_batch("CREATE TABLE projects (projectKey TEXT); BEGIN EXCLUSIVE;")
        .unwrap();
    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        default_inspect(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert_eq!(
        default_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert!(!destination.exists());
}

#[test]
fn project_rows_preserve_legacy_and_unknown_columns_and_sqlite_types() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch(
            "CREATE TABLE projects (
           id INTEGER PRIMARY KEY, projectKey TEXT, EngineAssociation TEXT, projectData TEXT,
           projectConfig TEXT, projectPath TEXT, note TEXT, collectionKey TEXT,
           legacy_blob BLOB, unusual_text TEXT, sort_order REAL, optional_value
         );",
        )
        .unwrap();
    connection.execute(
        "INSERT INTO projects VALUES (7, ?1, ?2, ?3, ?4, ?5, ?6, ?7, x'00ff', CAST(x'fffe' AS TEXT), 1.25, NULL)",
        params!["project-A", "UE-custom", "{\"Plugins\":[1]}", "{\"ini\":true}", "X:\\offline\\Project", "用户备注", "old-group"]
    ).unwrap();
    drop(connection);

    let projects = read_projects(&source).unwrap();
    assert_eq!(projects.len(), 1);
    let record = &projects[0];
    assert_eq!(record.get("id"), Some(&SqlValue::Integer(7)));
    assert_eq!(
        record.get("EngineAssociation"),
        Some(&SqlValue::Text(b"UE-custom".to_vec()))
    );
    assert_eq!(
        record.get("projectData"),
        Some(&SqlValue::Text(b"{\"Plugins\":[1]}".to_vec()))
    );
    assert_eq!(
        record.get("projectConfig"),
        Some(&SqlValue::Text(b"{\"ini\":true}".to_vec()))
    );
    assert_eq!(
        record.get("projectPath"),
        Some(&SqlValue::Text(b"X:\\offline\\Project".to_vec()))
    );
    assert_eq!(
        record.get("note"),
        Some(&SqlValue::Text("用户备注".as_bytes().to_vec()))
    );
    assert_eq!(
        record.get("collectionKey"),
        Some(&SqlValue::Text(b"old-group".to_vec()))
    );
    assert_eq!(
        record.get("legacy_blob"),
        Some(&SqlValue::Blob(vec![0, 255]))
    );
    assert_eq!(
        record.get("unusual_text"),
        Some(&SqlValue::Text(vec![255, 254]))
    );
    assert_eq!(record.get("sort_order"), Some(&SqlValue::Real(1.25)));
    assert_eq!(record.get("optional_value"), Some(&SqlValue::Null));
    assert_eq!(record.get("coverMode"), None);
}

#[test]
fn project_generated_columns_are_retained_in_query_order() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection.execute_batch(
        "CREATE TABLE projects (projectKey TEXT, generated TEXT GENERATED ALWAYS AS (projectKey || '-gen') VIRTUAL);
         INSERT INTO projects (projectKey) VALUES ('one');"
    ).unwrap();
    drop(connection);
    let records = read_projects(source).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].get("generated"),
        Some(&SqlValue::Text(b"one-gen".to_vec()))
    );
}

#[test]
fn duplicate_offline_project_rows_are_not_normalized_or_merged() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch(
            "CREATE TABLE projects (projectKey TEXT, projectPath TEXT, note TEXT);
             INSERT INTO projects VALUES ('one', 'Z:\\offline\\Game', 'first');
             INSERT INTO projects VALUES ('two', 'Z:\\offline\\Game', 'second');
             PRAGMA user_version=7;",
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&source).unwrap();
    let rows = read_projects(&source).unwrap();
    assert_eq!(rows.len(), 2);
    let keys: Vec<_> = rows
        .iter()
        .map(|row| row.get("projectKey").unwrap())
        .collect();
    assert!(keys.contains(&&SqlValue::Text(b"one".to_vec())));
    assert!(keys.contains(&&SqlValue::Text(b"two".to_vec())));
    assert!(rows
        .iter()
        .all(|row| row.get("projectPath") == Some(&SqlValue::Text(b"Z:\\offline\\Game".to_vec()))));
    assert_eq!(inspect_database(&source).unwrap().user_version, 7);
    assert_eq!(fs::read(source).unwrap(), before);
}

#[test]
fn missing_project_table_is_not_reported_as_an_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch("CREATE TABLE notebook_sources_old (content TEXT)")
        .unwrap();
    drop(connection);
    let inspection = inspect_database(&source).unwrap();
    assert!(inspection
        .tables
        .iter()
        .any(|table| table == "notebook_sources_old"));
    assert_eq!(
        read_projects(&source).unwrap_err().kind,
        ErrorKind::MissingProjectsTable
    );
}

#[test]
fn sqlite_page_corruption_is_detected_even_with_a_valid_header() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch(
            "CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('keep')",
        )
        .unwrap();
    let page: u64 = connection
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'projects'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let page_size: u64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .unwrap();
    connection.close().unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&source).unwrap();
    file.seek(SeekFrom::Start((page - 1) * page_size)).unwrap();
    file.write_all(&[0xff; 16]).unwrap();
    drop(file);

    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        inspect_database(&source).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert!(!destination.exists());
}

#[test]
fn invalid_wal_header_is_not_silently_treated_as_an_old_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (live, writer) = database(&dir, "live.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
         PRAGMA wal_autocheckpoint=0;
         CREATE TABLE projects (projectKey TEXT);
         INSERT INTO projects VALUES ('checkpointed');
         PRAGMA wal_checkpoint(TRUNCATE);
         INSERT INTO projects VALUES ('committed-in-wal');",
        )
        .unwrap();
    for (name, offset, damage) in [("magic", 0, &b"BAD!"[..]), ("checksum", 24, &b"BAD!"[..])] {
        let source = dir.path().join(format!("{name}.db"));
        let source_wal = dir.path().join(format!("{name}.db-wal"));
        fs::copy(&live, &source).unwrap();
        fs::copy(dir.path().join("live.db-wal"), &source_wal).unwrap();
        let mut wal_file = fs::OpenOptions::new()
            .write(true)
            .open(&source_wal)
            .unwrap();
        wal_file.seek(SeekFrom::Start(offset)).unwrap();
        wal_file.write_all(damage).unwrap();
        drop(wal_file);
        let wal_before = fs::read(&source_wal).unwrap();

        let destination = dir.path().join(format!("{name}.snapshot"));
        assert_eq!(
            inspect_database(&source).unwrap_err().kind,
            ErrorKind::Corrupt
        );
        assert_eq!(
            create_snapshot(&source, &destination).unwrap_err().kind,
            ErrorKind::Corrupt
        );
        assert!(!destination.exists());
        assert_eq!(fs::read(source_wal).unwrap(), wal_before);
    }
}

#[test]
fn invalid_wal_frame_is_not_silently_treated_as_an_old_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (live, writer) = database(&dir, "live.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE projects (projectKey TEXT);
             INSERT INTO projects VALUES ('checkpointed');
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO projects VALUES ('committed-in-wal');",
        )
        .unwrap();
    let source = dir.path().join("source.db");
    let wal = dir.path().join("source.db-wal");
    fs::copy(&live, &source).unwrap();
    fs::copy(dir.path().join("live.db-wal"), &wal).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&wal).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&[0x5a]).unwrap();
    drop(file);

    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        inspect_database(&source).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert!(!destination.exists());
}

#[test]
fn incomplete_wal_tail_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (live, writer) = database(&dir, "live.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE projects (projectKey TEXT);
             INSERT INTO projects VALUES ('committed');",
        )
        .unwrap();
    let source = dir.path().join("source.db");
    let wal = dir.path().join("source.db-wal");
    fs::copy(&live, &source).unwrap();
    fs::copy(dir.path().join("live.db-wal"), &wal).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&wal)
        .unwrap()
        .write_all(b"partial")
        .unwrap();
    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::Locked
    );
    assert!(!destination.exists());
}

#[test]
fn ambiguous_older_wal_frames_do_not_produce_a_successful_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE projects (projectKey TEXT, extra BLOB);
             INSERT INTO projects VALUES ('older', zeroblob(131072));",
        )
        .unwrap();
    let wal = dir.path().join("source.db-wal");
    let previous_length = fs::metadata(&wal).unwrap().len();
    writer
        .execute_batch(
            "PRAGMA wal_checkpoint(RESTART);
             INSERT INTO projects VALUES ('newer', NULL);",
        )
        .unwrap();
    assert_eq!(fs::metadata(wal).unwrap().len(), previous_length);
    writer
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    drop(writer);
    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::AmbiguousWalTail
    );
    assert!(!destination.exists());
}

#[test]
fn wal_frame_salt_damage_cannot_silently_hide_committed_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (live, writer) = database(&dir, "live.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE projects (projectKey TEXT);
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO projects VALUES ('committed-in-wal');",
        )
        .unwrap();
    let source = dir.path().join("source.db");
    let wal = dir.path().join("source.db-wal");
    fs::copy(live, &source).unwrap();
    fs::copy(dir.path().join("live.db-wal"), &wal).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&wal).unwrap();
    file.seek(SeekFrom::Start(32 + 8)).unwrap();
    file.write_all(b"badsalt!").unwrap();
    drop(file);
    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::AmbiguousWalTail
    );
    assert!(!destination.exists());
}

#[test]
fn missing_virtual_table_extension_is_not_misreported_as_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection.execute_batch(
        "CREATE TABLE projects (projectKey TEXT);
         CREATE VIRTUAL TABLE vector_data USING fts5(value);
         INSERT INTO vector_data VALUES ('underlying content');
         PRAGMA writable_schema = ON;
         UPDATE sqlite_schema SET sql = 'CREATE VIRTUAL TABLE vector_data USING unavailable_vec(value)' WHERE name = 'vector_data';
         PRAGMA writable_schema = OFF;"
    ).unwrap();
    connection.close().unwrap();

    let destination = dir.path().join("snapshot.db");
    let inspection = inspect_database(&source).unwrap();
    assert_eq!(inspection.unavailable_virtual_tables.len(), 1);
    assert_eq!(
        inspection.unavailable_virtual_tables[0].table,
        "vector_data"
    );
    assert!(inspection.unavailable_virtual_tables[0]
        .reason
        .contains("unavailable_vec"));
    let report = create_snapshot(&source, &destination).unwrap();
    assert_eq!(
        report.unavailable_virtual_tables,
        inspection.unavailable_virtual_tables
    );
    let snapshot =
        Connection::open_with_flags(&destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let content: String = snapshot
        .query_row("SELECT c0 FROM vector_data_content", [], |row| row.get(0))
        .unwrap();
    assert_eq!(content, "underlying content");
    let schema: String = snapshot
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'vector_data'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(schema.contains("unavailable_vec"));
}

#[test]
fn coordinated_snapshot_publications_have_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let sources: Vec<_> = (0..2)
        .map(|index| {
            let (source, connection) = database(&dir, &format!("source-{index}.db"));
            connection
                .execute_batch(
                    "CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('one')",
                )
                .unwrap();
            drop(connection);
            source
        })
        .collect();
    let destination = dir.path().join("snapshot.db");
    let barrier = Arc::new(Barrier::new(2));
    let destination_guard = Arc::new(Mutex::new(()));
    let handles: Vec<_> = sources
        .into_iter()
        .map(|source| {
            let destination = destination.clone();
            let barrier = Arc::clone(&barrier);
            let destination_guard = Arc::clone(&destination_guard);
            std::thread::spawn(move || {
                barrier.wait();
                let _exclusive_destination = destination_guard.lock().unwrap();
                create_snapshot(source, destination)
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(
                |result| matches!(result, Err(error) if error.kind == ErrorKind::DestinationExists)
            )
            .count(),
        1
    );
    assert_eq!(read_projects(&destination).unwrap().len(), 1);
    assert!(fs::read_dir(dir.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".uebox-snapshot-incomplete-")));
}

#[cfg(unix)]
#[test]
fn dangling_destination_symlink_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch("CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(connection);
    let destination = dir.path().join("snapshot.db");
    std::os::unix::fs::symlink("missing-target", &destination).unwrap();
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::DestinationExists
    );
    assert_eq!(
        fs::read_link(&destination).unwrap(),
        std::path::Path::new("missing-target")
    );
    assert!(!dir.path().join("missing-target").exists());
}

#[cfg(unix)]
#[test]
fn symlinked_source_checks_the_real_databases_wal() {
    let dir = tempfile::tempdir().unwrap();
    let (live, writer) = database(&dir, "live.db");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
         CREATE TABLE projects (projectKey TEXT);
         PRAGMA wal_checkpoint(TRUNCATE);
         INSERT INTO projects VALUES ('committed-in-wal');",
        )
        .unwrap();
    let source = dir.path().join("source.db");
    let wal = dir.path().join("source.db-wal");
    fs::copy(&live, &source).unwrap();
    fs::copy(dir.path().join("live.db-wal"), &wal).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&wal).unwrap();
    file.write_all(b"BAD!").unwrap();
    drop(file);
    let alias = dir.path().join("alias.db");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let destination = dir.path().join("snapshot.db");
    assert_eq!(
        inspect_database(&alias).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert_eq!(
        create_snapshot(&alias, &destination).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn snapshot_mode_bits_exclude_group_and_other_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch("CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(connection);
    let destination = dir.path().join("snapshot.db");
    create_snapshot(&source, &destination).unwrap();
    assert_eq!(
        fs::metadata(destination).unwrap().permissions().mode() & 0o077,
        0
    );
}

#[cfg(target_os = "macos")]
#[test]
fn snapshot_mode_bits_do_not_erase_inherited_acl() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (source, connection) = database(&dir, "source.db");
    connection
        .execute_batch("CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(connection);
    let output_dir = dir.path().join("acl-output");
    fs::create_dir(&output_dir).unwrap();
    fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new("/bin/chmod")
        .args([
            "+a",
            "everyone allow read,write,execute,delete,file_inherit,directory_inherit",
        ])
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "cannot install temporary ACL: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let destination = output_dir.join("snapshot.db");
    create_snapshot(&source, &destination).unwrap();
    assert_eq!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o077,
        0
    );
    let listing = Command::new("/bin/ls")
        .args(["-le"])
        .arg(&destination)
        .output()
        .unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(
        listing.contains("inherited allow read,write,execute,delete"),
        "expected inherited ACL, got: {listing}"
    );
}

#[cfg(unix)]
#[test]
fn readonly_wal_database_can_create_empty_coordination_sidecars() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('one');").unwrap();
    drop(writer);
    let wal = dir.path().join("source.db-wal");
    let shm = dir.path().join("source.db-shm");
    assert!(!wal.exists() && !shm.exists());
    fs::set_permissions(&source, fs::Permissions::from_mode(0o444)).unwrap();
    let before = fs::read(&source).unwrap();
    assert!(inspect_database(&source)
        .unwrap()
        .tables
        .contains(&"projects".to_string()));
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(fs::metadata(wal).unwrap().len(), 0);
    assert!(shm.exists());
}

#[cfg(unix)]
#[test]
fn readonly_wal_database_in_unwritable_directory_reports_io_error() {
    use std::os::unix::fs::PermissionsExt;
    struct RestorePermissions(std::path::PathBuf);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (source, writer) = database(&dir, "source.db");
    writer
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(writer);
    fs::set_permissions(&source, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let _restore = RestorePermissions(dir.path().to_path_buf());
    let probe = dir.path().join("permission-probe");
    if fs::write(&probe, b"").is_ok() {
        // A privileged runner can bypass mode bits, so this fixture is unavailable.
        fs::remove_file(probe).unwrap();
        return;
    }
    assert_eq!(inspect_database(&source).unwrap_err().kind, ErrorKind::Io);
    assert!(!dir.path().join("source.db-wal").exists());
    assert!(!dir.path().join("source.db-shm").exists());
}
