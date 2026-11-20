use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use uebox_project_library::{EngineKind, ErrorKind, ProjectLibrary};

const APPLICATION_ID: i64 = 0x5542_504c; // UBPL

const V1_PROJECTS: &str = "CREATE TABLE projects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engine TEXT NOT NULL CHECK(engine IN ('unreal', 'unity', 'godot')),
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    path TEXT NOT NULL,
    UNIQUE(engine, path)
)";

fn create_v1(database: &Path, rows: &[(&str, &str, &Path)]) {
    let connection = Connection::open(database).unwrap();
    connection.execute_batch(V1_PROJECTS).unwrap();
    connection
        .pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    for (engine, name, path) in rows {
        connection
            .execute(
                "INSERT INTO projects (engine, name, path) VALUES (?1, ?2, ?3)",
                params![engine, name, path.to_str().unwrap()],
            )
            .unwrap();
    }
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "delete");
}

fn offline(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

#[test]
fn v1_library_is_upgraded_in_place_preserving_offline_registrations_ids_and_content() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let online = dir.path().join("online-project");
    fs::create_dir(&online).unwrap();
    let offline_a = offline(&dir, "offline-a");
    let offline_b = offline(&dir, "offline-b");
    create_v1(
        &database,
        &[
            ("unreal", "虚幻项目", offline_a.as_path()),
            ("unity", "Unity online", online.as_path()),
            ("godot", "Godot", offline_b.as_path()),
        ],
    );

    let mut library = ProjectLibrary::open_existing(&database).unwrap();
    let rows = library.list().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[0].id, rows[0].engine), (1, EngineKind::Unreal));
    assert_eq!(rows[0].name, "虚幻项目");
    assert_eq!(rows[0].path, offline_a);
    assert_eq!((rows[1].id, rows[1].engine), (2, EngineKind::Unity));
    assert_eq!(rows[1].path, online);
    assert_eq!((rows[2].id, rows[2].engine), (3, EngineKind::Godot));
    // Offline registrations survive migration without touching the disk.
    assert!(!offline_a.exists());
    assert!(!offline_b.exists());

    // The migrated library is a fully working v2: metadata defaults are empty
    // and a new registration continues from the preserved ID sequence.
    let with_metadata = library.list_with_metadata().unwrap();
    assert_eq!(with_metadata.len(), 3);
    assert!(with_metadata.iter().all(|row| row.cover.is_none()));
    assert!(with_metadata.iter().all(|row| row.metadata.note.is_none()));

    let extra = dir.path().join("new-project");
    fs::create_dir(&extra).unwrap();
    let new = library
        .register(EngineKind::Unreal, "After migration", &extra)
        .unwrap();
    assert_eq!(new.id, 4);

    let connection = Connection::open(&database).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let has_metadata: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name = 'project_metadata'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(has_metadata, 1);
}

#[test]
fn nonstandard_v1_schema_is_rejected_without_modifying_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let offline = offline(&dir, "offline");
    create_v1(&database, &[("unreal", "keep", offline.as_path())]);
    // An extra object means the v1 schema no longer matches exactly.
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("CREATE TABLE extra (value TEXT)")
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fs::read(&database).unwrap(), before);
}

#[test]
fn foreign_v1_database_is_not_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE projects (id INTEGER PRIMARY KEY, engine TEXT, name TEXT, path TEXT)",
        )
        .unwrap();
    connection
        .pragma_update(None, "application_id", 0x1234_5678_i64)
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert_eq!(
        ProjectLibrary::open_existing(&database).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fs::read(&database).unwrap(), before);
}
