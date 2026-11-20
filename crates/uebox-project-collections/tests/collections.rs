use std::{
    path::Path,
    sync::{Arc, Barrier},
};

use uebox_project_collections::{CollectionError, LegacyCollection, ProjectCollections};
use uebox_project_library::{EngineKind, ProjectLibrary};

fn registration(directory: &Path) -> (ProjectLibrary, i64) {
    let mut library = ProjectLibrary::create_new(directory.join("library.sqlite")).unwrap();
    let project_path = directory.join("offline-project");
    std::fs::create_dir(&project_path).unwrap();
    let project = library
        .register(EngineKind::Unreal, "Fixture", &project_path)
        .unwrap();
    std::fs::remove_dir(project_path).unwrap();
    (library, project.id)
}

#[test]
fn stable_keys_order_and_empty_collections_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let (library, _) = registration(directory.path());
    let path = directory.path().join("collections.sqlite");
    let mut store = ProjectCollections::create_new(&path).unwrap();
    store.create_collection("collection_z", "One", 0).unwrap();
    store.create_collection("collection_a", "One", 0).unwrap();
    store.rename_collection("collection_z", "  教学  ").unwrap();
    assert_eq!(
        store.create_collection("collection_z", "Duplicate", 4),
        Err(CollectionError::Duplicate)
    );
    assert_eq!(
        store.set_order(&[("collection_z", -3), ("missing", 10)]),
        Err(CollectionError::CollectionMissing)
    );
    assert_eq!(
        store.list_collections(&library).unwrap()[0].id,
        "collection_a"
    );
    store
        .set_order(&[("collection_z", -3), ("collection_a", 2)])
        .unwrap();
    drop(store);

    let store = ProjectCollections::open_existing(&path).unwrap();
    let records = store.list_collections(&library).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["collection_z", "collection_a"]
    );
    assert_eq!(records[0].name.as_deref(), Some("教学"));
    assert_eq!(records[0].sort_order, -3);
    assert!(records.iter().all(|row| row.project_ids.is_empty()));
}

#[test]
fn bindings_are_many_to_many_and_dissolving_preserves_registration() {
    let directory = tempfile::tempdir().unwrap();
    let (mut library, project) = registration(directory.path());
    let mut store =
        ProjectCollections::create_new(directory.path().join("collections.sqlite")).unwrap();
    for id in ["versions", "purpose", "other"] {
        store.create_collection(id, id, 0).unwrap();
    }
    assert!(store.bind_project(&library, "versions", project).unwrap());
    assert!(store.bind_project(&library, "purpose", project).unwrap());
    assert!(!store.bind_project(&library, "purpose", project).unwrap());
    assert_eq!(
        store.bind_project(&library, "other", project + 1),
        Err(CollectionError::ProjectMissing)
    );
    assert_eq!(
        store.bind_project(&library, "missing", project),
        Err(CollectionError::CollectionMissing)
    );
    assert!(store.remove_collection("versions").unwrap());
    assert!(!store.remove_collection("versions").unwrap());
    let records = store.list_collections(&library).unwrap();
    assert!(records
        .iter()
        .find(|row| row.id == "purpose")
        .unwrap()
        .project_ids
        .contains(&project));
    assert_eq!(library.list().unwrap()[0].id, project);

    assert!(store.bind_project(&library, "other", project).unwrap());
    assert_eq!(store.unbind_project(Some("other"), project).unwrap(), 1);
    assert_eq!(store.unbind_project(None, project).unwrap(), 1);
    assert_eq!(store.unbind_project(None, project).unwrap(), 0);
    assert_eq!(store.list_collections(&library).unwrap().len(), 2);
    assert!(store.bind_project(&library, "other", project).unwrap());
    assert!(library.remove(project).unwrap());
    assert!(store
        .list_collections(&library)
        .unwrap()
        .iter()
        .all(|row| row.project_ids.is_empty()));
    assert_eq!(store.unbind_project(None, project).unwrap(), 1);
}

#[test]
fn mapped_legacy_import_is_atomic_and_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let (library, project) = registration(directory.path());
    let path = directory.path().join("collections.sqlite");
    let mut store = ProjectCollections::create_new(&path).unwrap();
    let empty = LegacyCollection {
        id: "collection_empty",
        name: None,
        sort_order: -3,
        project_ids: &[],
    };
    let grouped = LegacyCollection {
        id: "collection_existing",
        name: Some("5.6"),
        sort_order: 2,
        project_ids: &[project, project],
    };
    let unknown = LegacyCollection {
        id: "collection_dangling",
        project_ids: &[project + 1],
        ..grouped
    };
    assert_eq!(
        store.import_legacy(&library, &[empty, unknown]),
        Err(CollectionError::ProjectMissing)
    );
    assert!(store.list_collections(&library).unwrap().is_empty());
    let report = store.import_legacy(&library, &[empty, grouped]).unwrap();
    assert_eq!((report.inserted, report.already_present), (2, 0));
    drop(store);
    let mut store = ProjectCollections::open_existing(&path).unwrap();
    let report = store.import_legacy(&library, &[empty, grouped]).unwrap();
    assert_eq!((report.inserted, report.already_present), (0, 2));
    let rows = store.list_collections(&library).unwrap();
    assert_eq!(rows[0].name, None);
    assert_eq!(rows[1].project_ids, [project]);
    store
        .rename_collection(grouped.id, "Changed locally")
        .unwrap();
    assert_eq!(
        store.import_legacy(
            &library,
            &[LegacyCollection { id: "new", ..empty }, grouped]
        ),
        Err(CollectionError::Conflict)
    );
    assert_eq!(store.list_collections(&library).unwrap().len(), 2);
}

#[test]
fn concurrent_handles_serialize_duplicate_keys_and_links() {
    let directory = tempfile::tempdir().unwrap();
    let (library, project) = registration(directory.path());
    let path = directory.path().join("collections.sqlite");
    let lib_path = directory.path().join("library.sqlite");
    let mut first = ProjectCollections::create_new(&path).unwrap();
    let mut second = ProjectCollections::open_existing(&path).unwrap();
    let second_library = ProjectLibrary::open_existing(&lib_path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let result = std::thread::scope(|scope| {
        let other_barrier = Arc::clone(&barrier);
        let other = scope.spawn(move || {
            other_barrier.wait();
            let created = second.create_collection("same", "Same", 0);
            let bound = second
                .bind_project(&second_library, "same", project)
                .unwrap();
            (created, bound)
        });
        barrier.wait();
        let created = first.create_collection("same", "Same", 0);
        let bound = first.bind_project(&library, "same", project).unwrap();
        ((created, bound), other.join().unwrap())
    });
    assert_eq!(
        usize::from(result.0 .0.is_ok()) + usize::from(result.1 .0.is_ok()),
        1
    );
    assert!(matches!(
        result.0 .0.and(result.1 .0),
        Err(CollectionError::Duplicate)
    ));
    assert_ne!(result.0 .1, result.1 .1);
    assert_eq!(
        first.list_collections(&library).unwrap()[0].project_ids,
        [project]
    );
}

#[test]
fn invalid_inputs_and_foreign_databases_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let (library, _) = registration(directory.path());
    let existing = directory.path().join("library.sqlite");
    let prior = std::fs::read(&existing).unwrap();
    assert!(matches!(
        ProjectCollections::open_existing(&existing),
        Err(CollectionError::Unsupported)
    ));
    assert!(matches!(
        ProjectCollections::create_new(&existing),
        Err(CollectionError::AlreadyExists)
    ));
    assert_eq!(std::fs::read(existing).unwrap(), prior);
    let path = directory.path().join("collections.sqlite");
    assert!(matches!(
        ProjectCollections::open_existing(&path),
        Err(CollectionError::Missing)
    ));
    let mut store = ProjectCollections::create_new(&path).unwrap();
    assert_eq!(
        store.create_collection("\0", "A", 0),
        Err(CollectionError::InvalidInput)
    );
    assert_eq!(
        store.create_collection("one", "   ", 0),
        Err(CollectionError::InvalidInput)
    );
    assert_eq!(
        store.unbind_project(None, 0),
        Err(CollectionError::InvalidInput)
    );
    assert!(store.list_collections(&library).unwrap().is_empty());
}

#[test]
fn changed_schema_and_rollback_sidecars_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("collections.sqlite");
    drop(ProjectCollections::create_new(&path).unwrap());
    let journal = directory.path().join("collections.sqlite-journal");
    std::fs::write(&journal, b"journal fixture").unwrap();
    assert!(matches!(
        ProjectCollections::open_existing(&path),
        Err(CollectionError::RecoveryRequired)
    ));
    assert_eq!(std::fs::read(&journal).unwrap(), b"journal fixture");
    std::fs::remove_file(journal).unwrap();

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("CREATE TABLE extra (value TEXT)", [])
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        ProjectCollections::open_existing(&path),
        Err(CollectionError::Unsupported)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn concurrent_initialization_publishes_only_one_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("collections.sqlite");
    let barrier = Barrier::new(2);
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            barrier.wait();
            ProjectCollections::create_new(&path)
        });
        barrier.wait();
        let right = ProjectCollections::create_new(&path);
        (left.join().unwrap(), right)
    });
    assert!(matches!(
        (left, right),
        (Ok(_), Err(CollectionError::AlreadyExists)) | (Err(CollectionError::AlreadyExists), Ok(_))
    ));
    assert!(ProjectCollections::open_existing(&path).is_ok());
}
