#[cfg(not(unix))]
use rusqlite::Connection;
#[cfg(not(unix))]
use uebox_data::{create_snapshot, inspect_database, read_projects};
use uebox_data::{ErrorKind, PrivateSource};

#[cfg(not(unix))]
#[test]
fn unverified_source_identity_is_rejected_before_any_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.db");
    let connection = Connection::open(&source).unwrap();
    connection
        .execute_batch("CREATE TABLE projects (projectKey TEXT)")
        .unwrap();
    drop(connection);
    let destination = dir.path().join("snapshot.db");
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
    assert!(!destination.exists());
    let mut guard = ();
    // This newly created fixture has no other open connection or process.
    let mut private = unsafe { PrivateSource::from_host_verified_copy(&source, &mut guard) };
    assert_eq!(private.inspect().unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(
        private.read_projects().unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        private.create_snapshot(&destination).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert!(!destination.exists());
}

#[test]
fn input_errors_can_precede_unsupported_identity() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.db");
    let destination = dir.path().join("snapshot.db");
    let mut guard = ();
    // The missing candidate is inside a fresh, exclusively owned temporary directory.
    let mut private = unsafe { PrivateSource::from_host_verified_copy(&missing, &mut guard) };
    assert_eq!(private.inspect().unwrap_err().kind, ErrorKind::Missing);
    assert_eq!(
        private.read_projects().unwrap_err().kind,
        ErrorKind::Missing
    );
    assert_eq!(
        private.create_snapshot(&destination).unwrap_err().kind,
        ErrorKind::Missing
    );
    assert_eq!(
        private
            .create_snapshot(dir.path().join("reserved-wal"))
            .unwrap_err()
            .kind,
        ErrorKind::SourceConflict
    );
    assert!(!destination.exists());

    let mut directory_guard = ();
    // This fresh temporary directory is not a regular database file.
    let mut directory =
        unsafe { PrivateSource::from_host_verified_copy(dir.path(), &mut directory_guard) };
    assert_eq!(directory.inspect().unwrap_err().kind, ErrorKind::Io);
}
