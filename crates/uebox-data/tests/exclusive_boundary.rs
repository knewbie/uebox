#![cfg(unix)]

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, ErrorCode};
use uebox_data::{create_snapshot, inspect_database, read_projects, ErrorKind, PrivateSource};

#[test]
fn default_path_entrypoints_require_an_explicit_exclusive_private_copy() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("missing.db");
    let output = dir.path().join("snapshot.db");
    assert_eq!(
        inspect_database(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert_eq!(
        read_projects(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert_eq!(
        create_snapshot(&source, &output).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert!(!source.exists());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn disabled_entrypoints_borrow_named_temporary_files() {
    let source = tempfile::NamedTempFile::new().unwrap();
    let destination = tempfile::NamedTempFile::new().unwrap();
    assert_eq!(
        inspect_database(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert_eq!(
        read_projects(&source).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert_eq!(
        create_snapshot(&source, &destination).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert!(source.path().exists());
    assert!(destination.path().exists());
    assert_eq!(
        inspect_database(source.path()).unwrap_err().kind,
        ErrorKind::ExclusiveAccessRequired
    );
    assert_eq!(
        create_snapshot(source.path(), destination.path())
            .unwrap_err()
            .kind,
        ErrorKind::ExclusiveAccessRequired
    );
}

#[test]
fn host_verified_private_copy_can_be_inspected_and_snapshotted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private.db");
    let setup = Connection::open(&path).unwrap();
    setup
        .execute_batch(
            "CREATE TABLE projects (projectKey TEXT); INSERT INTO projects VALUES ('kept')",
        )
        .unwrap();
    drop(setup);
    let mut host_guard = ();
    // The test owns the only copy and closed its setup connection before this point.
    let mut source = unsafe { PrivateSource::from_host_verified_copy(&path, &mut host_guard) };
    assert!(source
        .inspect()
        .unwrap()
        .tables
        .contains(&"projects".to_string()));
    assert_eq!(source.read_projects().unwrap().len(), 1);
    let destination = dir.path().join("snapshot.db");
    source.create_snapshot(&destination).unwrap();
    let snapshot =
        Connection::open_with_flags(destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let key: String = snapshot
        .query_row("SELECT projectKey FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(key, "kept");
}

#[test]
fn competitor_process() {
    let Some(path) = std::env::var_os("UEBOX_DATA_BOUNDARY_PROBE") else {
        return;
    };
    let should_block = std::env::var_os("UEBOX_DATA_BOUNDARY_BLOCK").unwrap() == "yes";
    let sql = std::env::var("UEBOX_DATA_BOUNDARY_SQL").unwrap();
    let connection = Connection::open(path).unwrap();
    connection.busy_timeout(std::time::Duration::ZERO).unwrap();
    let result = connection.execute_batch(&sql);
    if should_block {
        assert!(
            matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)),
            "independent process unexpectedly acquired lock"
        );
    } else {
        result.unwrap();
    }
}

fn probe(path: &Path, sql: &str, block: bool) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "competitor_process", "--nocapture"])
        .env("UEBOX_DATA_BOUNDARY_PROBE", path)
        .env("UEBOX_DATA_BOUNDARY_SQL", sql)
        .env(
            "UEBOX_DATA_BOUNDARY_BLOCK",
            if block { "yes" } else { "no" },
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "competitor failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn sqlite_amalgamation() -> PathBuf {
    if let Some(path) = std::env::var_os("UEBOX_DATA_SQLITE_AMALGAMATION") {
        return path.into();
    }
    let cargo = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME or CARGO_HOME required"))
                .join(".cargo")
        });
    let registry = cargo.join("registry/src");
    for entry in fs::read_dir(&registry).expect("SQLite source registry must exist") {
        let path = entry
            .unwrap()
            .path()
            .join("libsqlite3-sys-0.30.1/sqlite3/sqlite3.c");
        if path.is_file() {
            return path;
        }
    }
    panic!("SQLite amalgamation not found; set UEBOX_DATA_SQLITE_AMALGAMATION to libsqlite3-sys 0.30.1 sqlite3.c");
}

fn compile_independent_sqlite(dir: &Path) -> PathBuf {
    let exports = dir.join("exports.txt");
    let library = dir.join(if cfg!(target_os = "macos") {
        "independent.dylib"
    } else {
        "independent.so"
    });
    let mut command = Command::new("cc");
    command.args([
        "-O0",
        "-DSQLITE_THREADSAFE=1",
        "-DSQLITE_OMIT_LOAD_EXTENSION=1",
    ]);
    if cfg!(target_os = "macos") {
        fs::write(&exports, "_sqlite3_open\n_sqlite3_exec\n_sqlite3_close\n").unwrap();
        command
            .arg("-dynamiclib")
            .arg(format!("-Wl,-exported_symbols_list,{}", exports.display()));
    } else {
        fs::write(
            &exports,
            "{ global: sqlite3_open; sqlite3_exec; sqlite3_close; local: *; };\n",
        )
        .unwrap();
        command
            .args(["-shared", "-fPIC", "-Wl,-Bsymbolic"])
            .arg(format!("-Wl,--version-script={}", exports.display()));
    }
    let output = command
        .arg(sqlite_amalgamation())
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "independent SQLite compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    library
}

#[cfg_attr(not(target_os = "macos"), link(name = "dl"))]
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

struct IndependentDb {
    library: *mut c_void,
    database: *mut c_void,
    exec: Exec,
    close: Close,
}

impl IndependentDb {
    fn open(library: &Path, source: &Path) -> Self {
        unsafe {
            let library_name = CString::new(library.to_str().unwrap()).unwrap();
            let library = dlopen(library_name.as_ptr(), 2);
            assert!(!library.is_null(), "cannot load independently built SQLite");
            let get_symbol = |name: &std::ffi::CStr| {
                let symbol = dlsym(library, name.as_ptr());
                assert!(!symbol.is_null());
                symbol
            };
            let open: Open = std::mem::transmute(get_symbol(c"sqlite3_open"));
            let exec: Exec = std::mem::transmute(get_symbol(c"sqlite3_exec"));
            let close: Close = std::mem::transmute(get_symbol(c"sqlite3_close"));
            assert_ne!(open as *const (), rusqlite::ffi::sqlite3_open as *const ());
            let mut database = std::ptr::null_mut();
            let path = CString::new(source.to_str().unwrap()).unwrap();
            assert_eq!(open(path.as_ptr(), &mut database), 0);
            Self {
                library,
                database,
                exec,
                close,
            }
        }
    }

    fn execute(&self, sql: &str) -> c_int {
        let sql = CString::new(sql).unwrap();
        unsafe {
            (self.exec)(
                self.database,
                sql.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }
    }
}

impl Drop for IndependentDb {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.database);
            dlclose(self.library);
        }
    }
}

#[test]
fn default_entrypoints_preserve_independent_vfs_read_and_write_locks() {
    let dir = tempfile::tempdir().unwrap();
    let library = compile_independent_sqlite(dir.path());
    for operation in ["inspect", "projects", "snapshot"] {
        for write in [false, true] {
            let source = dir.path().join(format!("{operation}-{write}.db"));
            let output = dir.path().join(format!("{operation}-{write}.snapshot"));
            let setup = Connection::open(&source).unwrap();
            setup.execute_batch("CREATE TABLE projects (projectKey TEXT, a INTEGER, b INTEGER); INSERT INTO projects VALUES ('keep',0,0);").unwrap();
            drop(setup);
            let independent = IndependentDb::open(&library, &source);
            let begin = if write {
                "BEGIN IMMEDIATE; UPDATE projects SET a=1;"
            } else {
                "BEGIN; SELECT * FROM projects;"
            };
            assert_eq!(independent.execute(begin), 0);
            let competing = if write {
                "BEGIN IMMEDIATE; UPDATE projects SET b=2; COMMIT;"
            } else {
                "BEGIN EXCLUSIVE; UPDATE projects SET b=2; COMMIT;"
            };
            probe(&source, competing, true);
            let kind = match operation {
                "inspect" => inspect_database(&source).unwrap_err().kind,
                "projects" => read_projects(&source).unwrap_err().kind,
                "snapshot" => create_snapshot(&source, &output).unwrap_err().kind,
                _ => unreachable!(),
            };
            assert_eq!(kind, ErrorKind::ExclusiveAccessRequired);
            assert!(!output.exists());
            probe(&source, competing, true);
            assert_eq!(independent.execute("COMMIT"), 0);
            if write {
                probe(&source, competing, false);
                drop(independent);
                let connection = Connection::open(&source).unwrap();
                let values: (i64, i64) = connection
                    .query_row("SELECT a,b FROM projects", [], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .unwrap();
                assert_eq!(values, (1, 2));
            }
        }
    }
}

struct HeldFilePath<'a> {
    path: PathBuf,
    _file: fs::File,
    accessed: &'a Cell<bool>,
    dropped: &'a Cell<bool>,
}

impl AsRef<Path> for HeldFilePath<'_> {
    fn as_ref(&self) -> &Path {
        self.accessed.set(true);
        &self.path
    }
}

impl Drop for HeldFilePath<'_> {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

#[test]
fn borrowed_file_holding_wrappers_preserve_independent_vfs_locks() {
    let dir = tempfile::tempdir().unwrap();
    let library = compile_independent_sqlite(dir.path());
    for operation in [
        "inspect",
        "projects",
        "snapshot-source",
        "snapshot-destination",
    ] {
        for write in [false, true] {
            let source = dir.path().join(format!("{operation}-{write}.db"));
            let output = dir.path().join(format!("{operation}-{write}.snapshot"));
            let setup = Connection::open(&source).unwrap();
            setup.execute_batch("CREATE TABLE projects (a INTEGER, b INTEGER); INSERT INTO projects VALUES (0,0);").unwrap();
            drop(setup);

            let independent = IndependentDb::open(&library, &source);
            let begin = if write {
                "BEGIN IMMEDIATE; UPDATE projects SET a=1;"
            } else {
                "BEGIN; SELECT * FROM projects;"
            };
            let competing = if write {
                "BEGIN IMMEDIATE; UPDATE projects SET b=2; COMMIT;"
            } else {
                "BEGIN EXCLUSIVE; UPDATE projects SET b=2; COMMIT;"
            };
            assert_eq!(independent.execute(begin), 0);
            let accessed = Cell::new(false);
            let dropped = Cell::new(false);
            let wrapper = HeldFilePath {
                path: if operation == "snapshot-destination" {
                    output.clone()
                } else {
                    source.clone()
                },
                // Even a destination wrapper can hold a descriptor to the source.
                _file: fs::File::open(&source).unwrap(),
                accessed: &accessed,
                dropped: &dropped,
            };
            probe(&source, competing, true);
            let kind = match operation {
                "inspect" => inspect_database(&wrapper).unwrap_err().kind,
                "projects" => read_projects(&wrapper).unwrap_err().kind,
                "snapshot-source" => create_snapshot(&wrapper, &output).unwrap_err().kind,
                "snapshot-destination" => create_snapshot(&source, &wrapper).unwrap_err().kind,
                _ => unreachable!(),
            };
            assert_eq!(kind, ErrorKind::ExclusiveAccessRequired);
            assert!(!accessed.get(), "{operation} evaluated the path");
            assert!(!dropped.get(), "{operation} dropped the wrapper");
            assert!(!output.exists());
            probe(&source, competing, true);
            assert_eq!(independent.execute("COMMIT"), 0);
            drop(wrapper);
            assert!(dropped.get());
            probe(&source, competing, false);
            drop(independent);
            let connection = Connection::open(&source).unwrap();
            let values: (i64, i64) = connection
                .query_row("SELECT a,b FROM projects", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap();
            assert_eq!(values, (i64::from(write), 2));
        }
    }
}
