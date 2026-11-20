use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;
use uebox_project_library::{EngineKind, ErrorKind, ProjectLibrary, ProjectMetadata};

fn project(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::create_dir(&path).unwrap();
    path
}

fn registered(dir: &TempDir, name: &str) -> (ProjectLibrary, i64) {
    let mut library = ProjectLibrary::create_new(dir.path().join("library.sqlite")).unwrap();
    let record = library
        .register(EngineKind::Unreal, name, project(dir, name))
        .unwrap();
    (library, record.id)
}

#[test]
fn metadata_round_trips_and_appears_in_the_ordered_list() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = registered(&dir, "first");
    let second = library
        .register(EngineKind::Unity, "second", project(&dir, "second"))
        .unwrap();

    assert_eq!(library.get_metadata(id).unwrap(), None);

    library
        .set_metadata(id, Some("备注"), Some("描述"), true, 2.5)
        .unwrap();
    assert_eq!(
        library.get_metadata(id).unwrap(),
        Some(ProjectMetadata {
            project_id: id,
            note: Some("备注".to_string()),
            description: Some("描述".to_string()),
            is_pinned: true,
            sort_order: 2.5,
        })
    );

    drop(library);
    let library = ProjectLibrary::open_existing(dir.path().join("library.sqlite")).unwrap();
    let rows = library.list_with_metadata().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].project.id, id, "pinned project is listed first");
    assert_eq!(rows[0].metadata.note.as_deref(), Some("备注"));
    assert_eq!(rows[0].metadata.description.as_deref(), Some("描述"));
    assert!(rows[0].metadata.is_pinned);
    assert_eq!(rows[0].metadata.sort_order, 2.5);
    assert_eq!(rows[0].cover, None);
    // The unpinned registration follows with all-default metadata.
    assert_eq!(rows[1].project.id, second.id);
    assert_eq!(
        rows[1].metadata,
        ProjectMetadata {
            project_id: second.id,
            note: None,
            description: None,
            is_pinned: false,
            sort_order: 0.0,
        }
    );
}

#[test]
fn empty_text_clears_and_invalid_values_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = registered(&dir, "game");
    // Full-set semantics: `None` and an empty string both clear a field, and
    // siblings are exactly what this call sets them to.
    library
        .set_metadata(id, Some("note"), Some("description"), false, 0.0)
        .unwrap();
    library
        .set_metadata(id, Some(""), Some("description"), false, 0.0)
        .unwrap();
    let metadata = library.get_metadata(id).unwrap().unwrap();
    assert_eq!(metadata.note, None);
    assert_eq!(metadata.description.as_deref(), Some("description"));

    for note in [Some("\0"), Some(&"x".repeat(1_048_577))] {
        assert_eq!(
            library
                .set_metadata(id, note, None, false, 0.0)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        library
            .set_metadata(id, None, None, false, f64::NAN)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        library
            .set_metadata(id, None, None, false, f64::INFINITY)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        library.get_metadata(0).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        library
            .set_metadata(0, None, None, false, 0.0)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn metadata_for_an_unknown_project_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = registered(&dir, "game");
    assert_eq!(
        library
            .set_metadata(id + 1, Some("note"), None, true, 1.0)
            .unwrap_err()
            .kind,
        ErrorKind::Missing
    );
    assert!(library.get_metadata(id).unwrap().is_none());
}

#[test]
fn remove_cascades_metadata_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut library, id) = registered(&dir, "game");
    library
        .set_metadata(id, Some("note"), Some("description"), true, 3.0)
        .unwrap();
    assert!(library.remove(id).unwrap());
    assert!(library.list_with_metadata().unwrap().is_empty());
    drop(library);
    assert!(
        ProjectLibrary::open_existing(dir.path().join("library.sqlite"))
            .unwrap()
            .list_with_metadata()
            .unwrap()
            .is_empty()
    );
}
