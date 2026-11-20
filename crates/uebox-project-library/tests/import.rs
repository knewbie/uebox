use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use rusqlite::Connection;
use uebox_project_library::{EngineKind, ErrorKind, ImportRecord, ImportSummary, ProjectLibrary};

fn unreal(name: &str, path: impl Into<PathBuf>) -> ImportRecord {
    ImportRecord {
        engine: EngineKind::Unreal,
        name: name.into(),
        path: path.into(),
    }
}

#[test]
fn reimport_preserves_existing_rows_and_skips_without_allocating_ids() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let existing_path = dir.path().join("existing");
    fs::create_dir(&existing_path).unwrap();
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let existing = library
        .register(EngineKind::Unreal, "Local name", &existing_path)
        .unwrap();

    let offline = dir.path().join("offline");
    let rows = [
        unreal("Imported name", &existing_path),
        unreal("Repeated existing path", &existing_path),
        unreal("  首条保留  ", &offline),
        unreal("Later duplicate", &offline),
        ImportRecord {
            engine: EngineKind::Unity,
            name: "Different engine".into(),
            path: offline.clone(),
        },
    ];
    assert_eq!(
        library.import_batch(&rows).unwrap(),
        ImportSummary {
            inserted: 2,
            already_registered: 1,
            duplicate_in_file: 2,
        }
    );
    let saved = library.list().unwrap();
    assert_eq!(saved[0], existing);
    assert_eq!(saved[1].id, existing.id + 1);
    assert_eq!(saved[1].name, "  首条保留  ");
    assert_eq!(saved[1].path, offline);
    assert_eq!(saved[2].id, existing.id + 2);
    assert_eq!(saved[2].engine, EngineKind::Unity);
    assert_eq!(
        library.import_batch(&rows).unwrap(),
        ImportSummary {
            inserted: 0,
            already_registered: 3,
            duplicate_in_file: 2,
        }
    );
    assert_eq!(library.list().unwrap(), saved);

    let manual_path = dir.path().join("manual");
    fs::create_dir(&manual_path).unwrap();
    let later = library
        .register(EngineKind::Godot, "After import", manual_path)
        .unwrap();
    assert_eq!(later.id, existing.id + 3);
    assert!(!offline.exists());
}

#[test]
fn first_import_publishes_all_rows_and_preserves_exact_path_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let path = dir.path().join("offline");
    let alias = path.join(".");
    assert_ne!(path.as_os_str(), alias.as_os_str());
    let rows = [
        unreal("First", &path),
        unreal("Exact duplicate", &path),
        unreal("Distinct spelling", &alias),
    ];
    let (library, summary) = ProjectLibrary::create_with_import(&database, &rows).unwrap();
    assert_eq!(
        summary,
        ImportSummary {
            inserted: 2,
            already_registered: 0,
            duplicate_in_file: 1,
        }
    );
    let expected = library.list().unwrap();
    assert_eq!(expected[0].id, 1);
    assert_eq!(expected[0].name, "First");
    assert_eq!(expected[1].id, 2);
    assert_eq!(expected[1].path.as_os_str(), alias.as_os_str());
    drop(library);
    let mut reopened = ProjectLibrary::open_existing(&database).unwrap();
    assert_eq!(reopened.list().unwrap(), expected);
    assert!(reopened.remove(expected[0].id).unwrap());
    assert_eq!(reopened.list().unwrap(), expected[1..]);
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn import_never_resolves_an_absolute_project_path() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loop");
    symlink("loop", &path).unwrap();
    let (library, summary) = ProjectLibrary::create_with_import(
        dir.path().join("library.sqlite"),
        &[unreal("Retain this path", &path)],
    )
    .unwrap();
    assert_eq!(summary.inserted, 1);
    assert_eq!(library.list().unwrap()[0].path, path);
    assert_eq!(fs::read_link(path).unwrap(), PathBuf::from("loop"));
}

#[test]
fn a_bad_last_record_leaves_both_existing_and_unpublished_libraries_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.sqlite");
    let missing = dir.path().join("unpublished.sqlite");
    let (mut library, _) =
        ProjectLibrary::create_with_import(&existing, &[unreal("Initial", dir.path().join("a"))])
            .unwrap();
    let before = fs::read(&existing).unwrap();
    let good = unreal("Valid", dir.path().join("not-yet-inserted"));
    let mut invalid_rows = vec![
        unreal("\u{2003}", &good.path),
        unreal("NUL\0name", &good.path),
        unreal("Relative", "relative/directory"),
        unreal("NUL path", dir.path().join("bad\0path")),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        invalid_rows.push(unreal("Windows path", r"C:\offline"));
        invalid_rows.push(unreal(
            "Non UTF-8 path",
            dir.path().join(std::ffi::OsString::from_vec(vec![0xff])),
        ));
    }
    #[cfg(windows)]
    invalid_rows.push(unreal("POSIX path", "/offline"));

    for invalid in invalid_rows {
        let rows = [good.clone(), invalid];
        assert_eq!(
            library.import_batch(&rows).unwrap_err().kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            ProjectLibrary::create_with_import(&missing, &rows)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidInput
        );
    }
    assert_eq!(fs::read(&existing).unwrap(), before);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn a_sqlite_failure_after_the_first_insert_rolls_back_the_batch_and_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let (mut library, _) =
        ProjectLibrary::create_with_import(&database, &[unreal("Initial", dir.path().join("a"))])
            .unwrap();
    let initial = library.list().unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("UPDATE sqlite_sequence SET seq = ?1", [i64::MAX - 1])
        .unwrap();
    drop(connection);

    let rows = [
        unreal("First insert", dir.path().join("b")),
        unreal("Overflow", dir.path().join("c")),
    ];
    assert!(library.import_batch(&rows).is_err());
    assert_eq!(library.list().unwrap(), initial);
    assert_eq!(library.import_batch(&rows[..1]).unwrap().inserted, 1);
    assert_eq!(library.list().unwrap()[1].id, i64::MAX);
}

#[test]
fn import_refuses_an_unknown_database_schema_and_creation_preserves_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let mut library = ProjectLibrary::create_new(&database).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA user_version=99").unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    let rows = [unreal("Valid", dir.path().join("project"))];
    assert_eq!(
        library.import_batch(&rows).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        ProjectLibrary::create_with_import(&database, &rows)
            .unwrap_err()
            .kind,
        ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&database).unwrap(), before);
}

#[test]
fn concurrent_imports_and_manual_registration_serialize_on_the_same_project() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let shared = dir.path().join("shared");
    fs::create_dir(&shared).unwrap();
    let mut manual = ProjectLibrary::create_new(&database).unwrap();
    let start = Arc::new(Barrier::new(3));
    let imports: Vec<_> = (0..2)
        .map(|index| {
            let mut library = ProjectLibrary::open_existing(&database).unwrap();
            let rows = [
                unreal(&format!("Shared from import {index}"), &shared),
                unreal(
                    &format!("Offline {index}"),
                    dir.path().join(format!("offline-{index}")),
                ),
            ];
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                library.import_batch(&rows).unwrap()
            })
        })
        .collect();
    start.wait();
    let manual_result = manual.register(EngineKind::Unreal, "Manual name", &shared);
    let imported: usize = imports
        .into_iter()
        .map(|task| task.join().unwrap().inserted)
        .sum();
    let saved = manual.list().unwrap();
    assert_eq!(saved.len(), 3);
    let shared_record = saved.iter().find(|record| record.path == shared).unwrap();
    match manual_result {
        Ok(record) => {
            assert_eq!(imported, 2);
            assert_eq!(shared_record, &record);
        }
        Err(error) => {
            assert_eq!(error.kind, ErrorKind::Duplicate);
            assert_eq!(imported, 3);
            assert!(shared_record.name.starts_with("Shared from import "));
        }
    }
    assert_eq!(
        saved.iter().map(|record| record.id).collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn concurrent_first_imports_publish_only_the_complete_winning_batch() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("library.sqlite");
    let start = Arc::new(Barrier::new(2));
    let tasks: Vec<_> = (0..2)
        .map(|index| {
            let database = database.clone();
            let start = Arc::clone(&start);
            let rows = [
                unreal(
                    &format!("Batch {index} first"),
                    dir.path().join(format!("first-{index}")),
                ),
                unreal(
                    &format!("Batch {index} second"),
                    dir.path().join(format!("second-{index}")),
                ),
            ];
            std::thread::spawn(move || {
                start.wait();
                ProjectLibrary::create_with_import(database, &rows)
            })
        })
        .collect();
    let results: Vec<_> = tasks.into_iter().map(|task| task.join().unwrap()).collect();
    let winner = results.iter().position(Result::is_ok).unwrap();
    let (_, summary) = results[winner].as_ref().unwrap();
    assert_eq!(summary.inserted, 2);
    assert_eq!(
        results[1 - winner].as_ref().unwrap_err().kind,
        ErrorKind::AlreadyExists
    );
    let library = ProjectLibrary::open_existing(database).unwrap();
    let rows = library.list().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, format!("Batch {winner} first"));
    assert_eq!(rows[1].name, format!("Batch {winner} second"));
}
