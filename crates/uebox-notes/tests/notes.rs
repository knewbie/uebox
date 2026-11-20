use std::{
    fs,
    sync::{Arc, Barrier},
};

use uebox_notes::{NotesError, NotesStore};

#[test]
fn notebook_and_markdown_round_trip_after_reopen() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("notes.sqlite");
    let mut store = NotesStore::create_new(&path).unwrap();
    let notebook_id = store.create_notebook("笔记本").unwrap();
    let content = "# 标题\r\n\r\n[link](file:///offline/fixture)  \n";
    let note_id = store.create_note(Some(notebook_id), "草稿", "").unwrap();
    assert!(store.rename_notebook(notebook_id, "修订").unwrap());
    assert!(store.update_note(note_id, "  正文  ", content).unwrap());
    drop(store);

    let store = NotesStore::open_existing(&path).unwrap();
    let notebook = store.get_notebook(notebook_id).unwrap().unwrap();
    let note = store.get_note(note_id).unwrap().unwrap();
    assert_eq!(
        (notebook.id, notebook.title.as_str()),
        (notebook_id, "修订")
    );
    assert_eq!(note.id, note_id);
    assert_eq!(note.notebook_id, Some(notebook_id));
    assert_eq!(note.title, "  正文  ");
    assert_eq!(note.content, content);
    let rows = store.list_notes(Some(notebook_id), None, 10).unwrap();
    assert_eq!((rows[0].id, rows[0].title.as_str()), (note_id, "  正文  "));
    assert!(store.list_notes(None, None, 10).unwrap().is_empty());
}

#[test]
fn deletes_follow_ownership_without_touching_siblings_or_standalone_notes() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("notes.sqlite");
    let mut store = NotesStore::create_new(&path).unwrap();
    let left = store.create_notebook("Left").unwrap();
    let right = store.create_notebook("Right").unwrap();
    let standalone = store.create_note(None, "Shared text", "same").unwrap();
    let removed = store
        .create_note(Some(left), "Shared text", "same")
        .unwrap();
    let cascaded = store.create_note(Some(left), "Second", "body").unwrap();
    let kept = store.create_note(Some(right), "Sibling", "body").unwrap();

    assert!(store.delete_note(removed).unwrap());
    assert!(!store.delete_note(removed).unwrap());
    assert!(store.get_notebook(left).unwrap().is_some());
    assert!(store.get_note(cascaded).unwrap().is_some());
    assert!(store.delete_notebook(left).unwrap());
    assert!(!store.delete_notebook(left).unwrap());
    assert!(store.get_note(cascaded).unwrap().is_none());
    assert!(store.get_note(kept).unwrap().is_some());
    assert!(store.get_note(standalone).unwrap().is_some());
    assert_eq!(
        store.create_note(Some(left), "missing", ""),
        Err(NotesError::NotebookMissing)
    );
    assert_eq!(
        store.list_notes(Some(left), None, 10),
        Err(NotesError::NotebookMissing)
    );

    drop(store);
    let mut store = NotesStore::open_existing(path).unwrap();
    assert!(store.create_notebook("Again").unwrap() > right);
    assert!(store.delete_note(kept).unwrap());
    assert!(store.create_note(None, "Again", "body").unwrap() > kept);
}

#[test]
fn id_cursor_is_exclusive_and_updates_do_not_reorder_rows() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();
    let first = store.create_notebook("First").unwrap();
    let second = store.create_notebook("Second").unwrap();
    assert_eq!(store.list_notebooks(None, 1).unwrap()[0].id, first);
    assert_eq!(store.list_notebooks(Some(first), 1).unwrap()[0].id, second);
    let a = store.create_note(Some(first), "A", "a").unwrap();
    let b = store.create_note(Some(first), "B", "b").unwrap();
    let c = store.create_note(Some(second), "C", "c").unwrap();
    let d = store.create_note(Some(first), "D", "d").unwrap();
    assert_eq!(store.list_notes(Some(first), None, 1).unwrap()[0].id, a);
    assert!(store.update_note(a, "A revised", "a").unwrap());
    assert!(store.delete_note(a).unwrap());
    let next = store.list_notes(Some(first), Some(a), 2).unwrap();
    assert_eq!(
        next.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [b, d]
    );
    assert_eq!(store.list_notes(Some(second), None, 1).unwrap()[0].id, c);
    assert!(store
        .list_notes(Some(first), Some(d), 1)
        .unwrap()
        .is_empty());
}

#[test]
fn invalid_input_and_missing_mutations_have_stable_results() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();
    let note = store.create_note(None, "", "").unwrap();
    assert_eq!(
        store.update_note(note, "changed", "bad\0body"),
        Err(NotesError::InvalidInput)
    );
    assert_eq!(
        store.create_notebook(&"x".repeat(1025)),
        Err(NotesError::InvalidInput)
    );
    assert_eq!(
        store.create_note(Some(0), "valid", ""),
        Err(NotesError::InvalidInput)
    );
    assert_eq!(store.list_notebooks(None, 0), Err(NotesError::InvalidInput));
    assert_eq!(
        store.list_notes(None, Some(-3), 10),
        Err(NotesError::InvalidInput)
    );
    assert_eq!(
        store.list_notes(None, None, 101),
        Err(NotesError::InvalidInput)
    );
    let too_large = "x".repeat(4 * 1024 * 1024 + 1);
    assert_eq!(
        store.update_note(note, "changed", &too_large),
        Err(NotesError::InvalidInput)
    );
    assert_eq!(store.get_note(note).unwrap().unwrap().title, "");
    assert!(!store.update_note(note + 1, "Missing", "").unwrap());
    assert!(!store.rename_notebook(100, "Missing").unwrap());
}

#[test]
fn concurrent_create_and_parent_delete_leave_no_orphans() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("notes.sqlite");
    let mut creator = NotesStore::create_new(&path).unwrap();
    let parent = creator.create_notebook("Parent").unwrap();
    let mut deleter = NotesStore::open_existing(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let result = std::thread::scope(|scope| {
        let started = Arc::clone(&barrier);
        let writer = scope.spawn(move || {
            started.wait();
            creator.create_note(Some(parent), "Child", "content")
        });
        barrier.wait();
        assert!(deleter.delete_notebook(parent).unwrap());
        writer.join().unwrap()
    });
    if let Ok(note_id) = result {
        assert!(deleter.get_note(note_id).unwrap().is_none());
    } else {
        assert_eq!(result, Err(NotesError::NotebookMissing));
    }
    drop(deleter);
    assert!(NotesStore::open_existing(path).is_ok());
}

#[test]
fn missing_foreign_and_incompatible_storage_is_never_initialized_or_repaired() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("notes.sqlite");
    assert!(matches!(
        NotesStore::open_existing(&path),
        Err(NotesError::Missing)
    ));
    assert!(!path.exists());
    let foreign = rusqlite::Connection::open(&path).unwrap();
    foreign
        .execute("CREATE TABLE foreign_table (label TEXT)", [])
        .unwrap();
    drop(foreign);
    let original = fs::read(&path).unwrap();
    assert!(matches!(
        NotesStore::open_existing(&path),
        Err(NotesError::Unsupported)
    ));
    assert!(matches!(
        NotesStore::create_new(&path),
        Err(NotesError::AlreadyExists)
    ));
    assert_eq!(fs::read(path).unwrap(), original);

    let path = temporary.path().join("real-notes.sqlite");
    drop(NotesStore::create_new(&path).unwrap());
    let journal = temporary.path().join("real-notes.sqlite-journal");
    fs::write(&journal, b"test journal").unwrap();
    assert!(matches!(
        NotesStore::open_existing(&path),
        Err(NotesError::RecoveryRequired)
    ));
    assert_eq!(fs::read(&journal).unwrap(), b"test journal");
    fs::remove_file(journal).unwrap();

    let modified = rusqlite::Connection::open(&path).unwrap();
    modified
        .execute("CREATE TABLE unexpected (label TEXT)", [])
        .unwrap();
    drop(modified);
    let original = fs::read(&path).unwrap();
    assert!(matches!(
        NotesStore::open_existing(&path),
        Err(NotesError::Unsupported)
    ));
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn future_version_and_wal_are_rejected_without_changing_source_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("notes.sqlite");
    drop(NotesStore::create_new(&path).unwrap());
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.pragma_update(None, "user_version", 2).unwrap();
    drop(raw);
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        NotesStore::open_existing(&path),
        Err(NotesError::Unsupported)
    ));
    assert_eq!(fs::read(&path).unwrap(), before);

    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.pragma_update(None, "user_version", 1).unwrap();
    drop(raw);
    drop(NotesStore::open_existing(&path).unwrap());
    let before = fs::read(&path).unwrap();
    let wal = temporary.path().join("notes.sqlite-wal");
    fs::write(&wal, b"test wal").unwrap();
    assert!(matches!(
        NotesStore::open_existing(&path),
        Err(NotesError::Unsupported)
    ));
    assert_eq!(fs::read(&wal).unwrap(), b"test wal");
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn legacy_import_retries_are_idempotent_and_conflicts_roll_back_whole_batch() {
    use uebox_notes::{LegacyNote, LegacyNotebook, NoteImportSummary};

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("notes.sqlite");
    let mut store = NotesStore::create_new(&path).unwrap();
    let notebooks = [LegacyNotebook {
        identity: "nb-old-1",
        title: "Research",
    }];
    let notes = [LegacyNote {
        identity: "source-old-1",
        notebook_identity: Some("nb-old-1"),
        title: "Markdown",
        content: "# Heading\n\nBody",
    }];
    assert_eq!(
        store
            .import_legacy_batch("electron-v1", &notebooks, &notes)
            .unwrap(),
        NoteImportSummary {
            notebooks_inserted: 1,
            notebooks_existing: 0,
            notes_inserted: 1,
            notes_existing: 0
        }
    );
    drop(store);

    let mut store = NotesStore::open_existing(&path).unwrap();
    assert_eq!(
        store
            .import_legacy_batch("electron-v1", &notebooks, &notes)
            .unwrap(),
        NoteImportSummary {
            notebooks_inserted: 0,
            notebooks_existing: 1,
            notes_inserted: 0,
            notes_existing: 1
        }
    );
    assert_eq!(store.list_notebooks(None, 10).unwrap().len(), 1);
    let notebook_id = store.list_notebooks(None, 10).unwrap()[0].id;
    assert_eq!(
        store.list_notes(Some(notebook_id), None, 10).unwrap().len(),
        1
    );

    let mut conflict = notes;
    conflict[0] = LegacyNote {
        content: "changed content",
        ..notes[0]
    };
    let extra_notebook = LegacyNotebook {
        identity: "nb-new",
        title: "Would roll back",
    };
    assert_eq!(
        store.import_legacy_batch("electron-v1", &[extra_notebook], &conflict),
        Err(NotesError::ImportConflict)
    );
    assert_eq!(store.list_notebooks(None, 10).unwrap().len(), 1);
    assert!(store.get_note(2).unwrap().is_none());

    let child_without_parent = [LegacyNote {
        identity: "orphan",
        notebook_identity: Some("missing"),
        title: "Orphan",
        content: "",
    }];
    assert_eq!(
        store.import_legacy_batch("electron-v1", &[], &child_without_parent),
        Err(NotesError::NotebookMissing)
    );
    assert!(store.list_notes(Some(notebook_id), None, 10).unwrap().len() == 1);
}
