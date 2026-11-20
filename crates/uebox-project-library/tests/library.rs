use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use rusqlite::Connection;
use tempfile::TempDir;
use uebox_project_library::{EngineKind, ErrorKind, ProjectLibrary};

fn project(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn three_engines_can_register_the_same_directory_and_persist_names() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    fs::write(root.join("sentinel"), b"do not touch").unwrap();
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let unreal = library
        .register(EngineKind::Unreal, "虚幻项目", &root)
        .unwrap();
    let unity = library
        .register(EngineKind::Unity, "Unity project", &root)
        .unwrap();
    let godot = library
        .register(EngineKind::Godot, "Godot project", &root)
        .unwrap();
    assert_ne!(unreal.id, unity.id);
    assert_ne!(unity.id, godot.id);
    drop(library);

    let reopened = ProjectLibrary::open_existing(&database).unwrap();
    let rows = reopened.list().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        (
            rows[0].engine,
            rows[0].name.as_str(),
            rows[0].path.as_path()
        ),
        (EngineKind::Unreal, "虚幻项目", root.as_path())
    );
    assert_eq!(
        (
            rows[1].engine,
            rows[1].name.as_str(),
            rows[1].path.as_path()
        ),
        (EngineKind::Unity, "Unity project", root.as_path())
    );
    assert_eq!(
        (
            rows[2].engine,
            rows[2].name.as_str(),
            rows[2].path.as_path()
        ),
        (EngineKind::Godot, "Godot project", root.as_path())
    );
    assert_eq!(fs::read(root.join("sentinel")).unwrap(), b"do not touch");
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn offline_record_remains_listable_and_removable_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "offline-project");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let row = library
        .register(EngineKind::Unreal, "Offline", &root)
        .unwrap();
    drop(library);
    fs::rename(&root, dir.path().join("project-disconnected")).unwrap();

    let mut library = ProjectLibrary::open_existing(&database).unwrap();
    assert_eq!(library.list().unwrap(), vec![row.clone()]);
    assert!(library.remove(row.id).unwrap());
    assert!(!library.remove(row.id).unwrap());
    drop(library);
    assert!(ProjectLibrary::open_existing(&database)
        .unwrap()
        .list()
        .unwrap()
        .is_empty());
    assert!(dir.path().join("project-disconnected").is_dir());
}

#[test]
fn a_removed_record_id_is_not_reused_for_a_new_project() {
    let dir = tempfile::tempdir().unwrap();
    let first = project(&dir, "first");
    let second = project(&dir, "second");
    let mut library = ProjectLibrary::create_new(dir.path().join("library.sqlite")).unwrap();
    let old = library.register(EngineKind::Godot, "first", first).unwrap();
    assert!(library.remove(old.id).unwrap());
    let new = library
        .register(EngineKind::Godot, "second", second)
        .unwrap();
    assert_ne!(old.id, new.id);
    assert!(!library.remove(old.id).unwrap());
    assert_eq!(library.list().unwrap(), vec![new]);
}

#[cfg(unix)]
#[test]
fn symlink_alias_of_online_directory_is_duplicate_only_within_same_engine() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "real");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let mut library = ProjectLibrary::create_new(dir.path().join("library.sqlite")).unwrap();
    library.register(EngineKind::Unity, "Real", &root).unwrap();
    assert_eq!(
        library
            .register(EngineKind::Unity, "Alias", &alias)
            .unwrap_err()
            .kind,
        ErrorKind::Duplicate
    );
    let other = library
        .register(EngineKind::Godot, "Other engine", &alias)
        .unwrap();
    assert_eq!(other.path, alias);
    assert_eq!(library.list().unwrap().len(), 2);
}

#[test]
fn invalid_new_project_input_does_not_create_records_or_touch_projects() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let file = dir.path().join("not-a-directory");
    fs::write(&file, b"keep").unwrap();
    let mut library = ProjectLibrary::create_new(dir.path().join("library.sqlite")).unwrap();
    assert_eq!(
        library
            .register(EngineKind::Unreal, "  ", &root)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        library
            .register(EngineKind::Unreal, "missing", dir.path().join("missing"))
            .unwrap_err()
            .kind,
        ErrorKind::Missing
    );
    assert_eq!(
        library
            .register(EngineKind::Unity, "file", &file)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        library
            .register(EngineKind::Godot, "relative", Path::new("relative/path"))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert!(library.list().unwrap().is_empty());
    assert_eq!(fs::read(file).unwrap(), b"keep");
}

#[test]
fn create_refuses_existing_databases_and_empty_files_without_changing_them() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.sqlite");
    let connection = Connection::open(&existing).unwrap();
    connection
        .execute_batch("CREATE TABLE original (value TEXT); INSERT INTO original VALUES ('keep')")
        .unwrap();
    drop(connection);
    let before = fs::read(&existing).unwrap();
    assert_eq!(
        ProjectLibrary::create_new(&existing).unwrap_err().kind,
        ErrorKind::AlreadyExists
    );
    assert_eq!(
        ProjectLibrary::open_existing(&existing).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fs::read(&existing).unwrap(), before);

    let empty = dir.path().join("empty.sqlite");
    fs::write(&empty, b"").unwrap();
    assert_eq!(
        ProjectLibrary::create_new(&empty).unwrap_err().kind,
        ErrorKind::AlreadyExists
    );
    assert_eq!(
        ProjectLibrary::open_existing(&empty).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert_eq!(fs::read(empty).unwrap(), b"");
}

#[test]
fn opening_missing_or_corrupt_database_never_creates_or_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sqlite");
    assert_eq!(
        ProjectLibrary::open_existing(&missing).unwrap_err().kind,
        ErrorKind::Missing
    );
    assert!(!missing.exists());
    let corrupt = dir.path().join("corrupt.sqlite");
    fs::write(&corrupt, b"not a sqlite database").unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&corrupt).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert_eq!(fs::read(corrupt).unwrap(), b"not a sqlite database");
}

#[test]
fn unknown_version_is_rejected_without_altering_existing_records() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    library.register(EngineKind::Unreal, "keep", root).unwrap();
    drop(library);
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA user_version=99").unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    let connection = Connection::open(&database).unwrap();
    let name: String = connection
        .query_row("SELECT name FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(name, "keep");
}

#[test]
fn incompatible_schema_is_rejected_even_with_the_expected_version() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    library.register(EngineKind::Godot, "keep", root).unwrap();
    drop(library);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("ALTER TABLE projects ADD COLUMN unrecognized TEXT")
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn concurrent_same_engine_registration_has_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let database = dir.path().join("library.sqlite");
    drop(ProjectLibrary::create_new(&database).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|index| {
            let mut library = ProjectLibrary::open_existing(&database).unwrap();
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                library.register(EngineKind::Unity, &format!("project-{index}"), root)
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
            .filter(|result| matches!(result, Err(error) if error.kind == ErrorKind::Duplicate))
            .count(),
        3
    );
    assert_eq!(
        ProjectLibrary::open_existing(database)
            .unwrap()
            .list()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn concurrent_creation_never_overwrites_the_winner() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = database.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ProjectLibrary::create_new(path)
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
            .filter(|result| matches!(result, Err(error) if error.kind == ErrorKind::AlreadyExists))
            .count(),
        3
    );
    assert!(ProjectLibrary::open_existing(database)
        .unwrap()
        .list()
        .unwrap()
        .is_empty());
}

#[test]
fn simultaneous_distinct_registrations_are_all_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    drop(ProjectLibrary::create_new(&database).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|index| {
            let mut library = ProjectLibrary::open_existing(&database).unwrap();
            let path = project(&dir, &format!("game-{index}"));
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                library.register(EngineKind::Godot, &format!("game-{index}"), path)
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    assert_eq!(
        ProjectLibrary::open_existing(database)
            .unwrap()
            .list()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn locked_database_returns_an_error_without_losing_records() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let record = library.register(EngineKind::Unreal, "keep", root).unwrap();
    let lock = Connection::open(&database).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert_eq!(library.list().unwrap_err().kind, ErrorKind::Locked);
    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(library.list().unwrap(), vec![record]);
}

#[test]
fn stale_handle_refuses_schema_changes_before_reading_or_writing() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "original");
    let another = project(&dir, "another");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let record = library.register(EngineKind::Unreal, "keep", root).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("ALTER TABLE projects ADD COLUMN unknown TEXT")
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert_eq!(library.list().unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(
        library
            .register(EngineKind::Unity, "new", another)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        library.remove(record.id).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn stale_handle_does_not_operate_on_a_replacement_database() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("library.sqlite");
    let replacement = dir.path().join("replacement.sqlite");
    let root = project(&dir, "original-game");
    let another = project(&dir, "replacement-game");
    let third = project(&dir, "third-game");
    let mut library = ProjectLibrary::create_new(&original).unwrap();
    let original_row = library
        .register(EngineKind::Unreal, "original", root)
        .unwrap();
    let mut other = ProjectLibrary::create_new(&replacement).unwrap();
    let replacement_row = other
        .register(EngineKind::Unity, "replacement", another)
        .unwrap();
    drop(other);
    fs::rename(&original, dir.path().join("original-saved.sqlite")).unwrap();
    fs::rename(&replacement, &original).unwrap();

    assert_eq!(library.list().unwrap_err().kind, ErrorKind::Locked);
    assert_eq!(
        library
            .register(EngineKind::Godot, "third", third)
            .unwrap_err()
            .kind,
        ErrorKind::Locked
    );
    assert_eq!(
        library.remove(original_row.id).unwrap_err().kind,
        ErrorKind::Locked
    );
    assert_eq!(
        ProjectLibrary::open_existing(&original)
            .unwrap()
            .list()
            .unwrap(),
        vec![replacement_row]
    );
    assert_eq!(
        ProjectLibrary::open_existing(dir.path().join("original-saved.sqlite"))
            .unwrap()
            .list()
            .unwrap(),
        vec![original_row]
    );
}

#[test]
fn an_offline_registration_does_not_prevent_other_projects_being_added() {
    let dir = tempfile::tempdir().unwrap();
    let first = project(&dir, "first");
    let second = project(&dir, "second");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(database).unwrap();
    let offline = library
        .register(EngineKind::Unreal, "offline", &first)
        .unwrap();
    fs::rename(first, dir.path().join("detached")).unwrap();
    let online = library
        .register(EngineKind::Unreal, "online", second)
        .unwrap();
    assert_eq!(library.list().unwrap(), vec![offline.clone(), online]);
    assert!(library.remove(offline.id).unwrap());
}

#[test]
fn matching_version_with_an_unknown_trigger_is_rejected_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    library.register(EngineKind::Unreal, "keep", &root).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER unexpected AFTER DELETE ON projects BEGIN DELETE FROM projects; END;",
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(library.remove(1).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn unexpected_wal_sidecar_is_rejected_without_touching_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let library = ProjectLibrary::create_new(&database).unwrap();
    let before = fs::read(&database).unwrap();
    let wal = dir.path().join("library.sqlite-wal");
    fs::write(&wal, b"unknown WAL").unwrap();
    assert_eq!(library.list().unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read(wal).unwrap(), b"unknown WAL");
}

#[test]
fn invalid_stored_relative_path_is_reported_and_not_treated_as_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let first = project(&dir, "first");
    let next = project(&dir, "next");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    library.register(EngineKind::Unreal, "keep", first).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("UPDATE projects SET path = 'relative'", [])
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert_eq!(library.list().unwrap_err().kind, ErrorKind::Corrupt);
    assert_eq!(
        library
            .register(EngineKind::Unreal, "next", next)
            .unwrap_err()
            .kind,
        ErrorKind::Corrupt
    );
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn corrupt_page_is_reported_without_initializing_or_clearing_the_database() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    library.register(EngineKind::Unreal, "keep", root).unwrap();
    drop(library);
    let connection = Connection::open(&database).unwrap();
    let page: u64 = connection
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name='projects'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let size: u64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .unwrap();
    drop(connection);
    let mut file = fs::OpenOptions::new().write(true).open(&database).unwrap();
    file.seek(SeekFrom::Start((page - 1) * size)).unwrap();
    file.write_all(&[0xff; 16]).unwrap();
    drop(file);
    let before = fs::read(&database).unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn index_key_corruption_is_rejected_before_opening_or_registering() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "game");
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    library.register(EngineKind::Unreal, "keep", &root).unwrap();

    let connection = Connection::open(&database).unwrap();
    let page: usize = connection
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name='sqlite_autoindex_projects_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let page_size: usize = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .unwrap();
    drop(connection);

    // Alter the indexed path without changing the table row or the index layout.
    let mut bytes = fs::read(&database).unwrap();
    let index = &mut bytes[(page - 1) * page_size..page * page_size];
    let key = root.to_str().unwrap().as_bytes();
    let offset = index
        .windows(key.len())
        .position(|part| part == key)
        .unwrap();
    index[offset + key.len() - 1] = b'f';
    fs::write(&database, &bytes).unwrap();

    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    assert_eq!(
        library
            .register(EngineKind::Unreal, "duplicate", &root)
            .unwrap_err()
            .kind,
        ErrorKind::Corrupt
    );
    assert_eq!(fs::read(database).unwrap(), bytes);
}

#[test]
fn existing_destination_sidecars_are_never_touched() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let wal = dir.path().join("library.sqlite-wal");
    fs::write(&wal, b"keep sidecar").unwrap();
    assert_eq!(
        ProjectLibrary::create_new(&database).unwrap_err().kind,
        ErrorKind::AlreadyExists
    );
    assert!(!database.exists());
    assert_eq!(fs::read(wal).unwrap(), b"keep sidecar");
}

#[cfg(unix)]
#[test]
fn database_hardlink_alias_is_rejected_without_changing_either_name() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let root = project(&dir, "game");
    library.register(EngineKind::Unreal, "keep", root).unwrap();
    let before = fs::read(&database).unwrap();
    let alias = dir.path().join("alias.sqlite");
    fs::hard_link(&database, &alias).unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&alias).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(library.list().unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(fs::read(database).unwrap(), before);
    assert_eq!(fs::read(alias).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn non_utf8_project_path_is_rejected_without_lossy_persistence() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let project = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"bad-\xff".to_vec()));
    let mut library = ProjectLibrary::create_new(dir.path().join("library.sqlite")).unwrap();
    assert_eq!(
        library
            .register(EngineKind::Godot, "keep", project)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert!(library.list().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn readonly_library_can_be_listed_but_rejects_removal() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let root = project(&dir, "game");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let record = library.register(EngineKind::Unreal, "keep", root).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o400)).unwrap();
    if fs::OpenOptions::new().write(true).open(&database).is_ok() {
        // A privileged runner can bypass file mode bits.
        return;
    }
    assert_eq!(library.list().unwrap(), vec![record.clone()]);
    assert_eq!(library.remove(record.id).unwrap_err().kind, ErrorKind::Io);
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(library.list().unwrap(), vec![record]);
}

#[cfg(windows)]
#[test]
fn windows_case_alias_of_an_online_directory_is_not_duplicated() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir, "MiXeDProject");
    let alias = dir.path().join("mixedproject");
    if !alias.exists() {
        return;
    }
    let mut library = ProjectLibrary::create_new(dir.path().join("library.sqlite")).unwrap();
    library.register(EngineKind::Unity, "first", root).unwrap();
    assert_eq!(
        library
            .register(EngineKind::Unity, "duplicate", alias)
            .unwrap_err()
            .kind,
        ErrorKind::Duplicate
    );
}
