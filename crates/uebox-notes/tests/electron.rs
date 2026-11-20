use std::path::Path;

use rusqlite::Connection;
use uebox_notes::{ElectronNotes, ElectronSourceKind, NoteFormat, NotesError};

fn full_schema(connection: &Connection) {
    connection
        .execute_batch(
            r#"
        CREATE TABLE notebooks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            notebook_id TEXT NOT NULL UNIQUE,
            title TEXT NOT NULL DEFAULT '新知识库',
            description TEXT DEFAULT NULL,
            cover_style TEXT DEFAULT NULL,
            cover_image TEXT DEFAULT NULL,
            source_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL DEFAULT '未命名笔记',
            content TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT '',
            remote_id TEXT DEFAULT NULL,
            sync_status TEXT NOT NULL DEFAULT 'local-only',
            is_shared INTEGER NOT NULL DEFAULT 0,
            tags TEXT DEFAULT NULL
        );
        CREATE TABLE notebook_sources (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id TEXT NOT NULL UNIQUE,
            notebook_id TEXT NOT NULL,
            title TEXT NOT NULL,
            type TEXT NOT NULL,
            content TEXT NOT NULL DEFAULT '',
            source_url TEXT DEFAULT NULL,
            file_name TEXT DEFAULT NULL,
            file_path TEXT DEFAULT NULL,
            media_url TEXT DEFAULT NULL,
            raw_content TEXT DEFAULT NULL,
            context_level TEXT DEFAULT NULL
        );
        "#,
        )
        .unwrap();
}

#[test]
fn reads_full_electron_model_without_flattening_content() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("app-data.db");
    let connection = Connection::open(&path).unwrap();
    full_schema(&connection);
    connection
        .execute(
            "INSERT INTO notebooks(notebook_id, title, description, cover_image)
             VALUES (?1, ?2, ?3, ?4)",
            ["nb-1", "Legacy KB", "desc", "res://cover.png"],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notes(id, title, content)
             VALUES (1, 'Hello', '<p>Hi <img src=\"data:image/png;base64,AA\"> there</p>')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notebook_sources(source_id, notebook_id, title, type, content)
             VALUES ('src-note', 'nb-1', 'Src note', 'note', '<h1>T</h1><img src=\"file:///x.png\">')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notebook_sources(source_id, notebook_id, title, type, content)
             VALUES ('src-text', 'nb-1', 'Src text', 'text', 'plain')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notebook_sources(source_id, notebook_id, title, type, content, raw_content, file_name, file_path)
             VALUES ('src-file', 'nb-1', 'Report', 'file', 'pdf body', 'raw pdf', 'report.pdf', '/tmp/report.pdf')",
            [],
        )
        .unwrap();
    drop(connection);

    let reader = ElectronNotes::open_read_only(&path).unwrap();
    let snapshot = reader.snapshot().unwrap();

    assert_eq!(snapshot.notebooks.len(), 1);
    let notebook = &snapshot.notebooks[0];
    assert_eq!(notebook.identity, "nb-1");
    assert_eq!(notebook.title, "Legacy KB");
    assert_eq!(notebook.description.as_deref(), Some("desc"));
    assert_eq!(notebook.cover_image.as_deref(), Some("res://cover.png"));

    assert_eq!(snapshot.notes.len(), 3);
    let independent = snapshot
        .notes
        .iter()
        .find(|note| note.identity == "note:1")
        .unwrap();
    assert!(independent.rich_text);
    assert_eq!(independent.inline_attachments, 1);
    assert_eq!(independent.notebook_identity, None);
    assert_eq!(
        independent.content,
        "<p>Hi <img src=\"data:image/png;base64,AA\"> there</p>"
    );
    let note_source = snapshot
        .notes
        .iter()
        .find(|note| note.identity == "source:src-note")
        .unwrap();
    assert!(note_source.rich_text);
    assert_eq!(note_source.inline_attachments, 1);
    assert_eq!(note_source.notebook_identity.as_deref(), Some("nb-1"));
    let text_source = snapshot
        .notes
        .iter()
        .find(|note| note.identity == "source:src-text")
        .unwrap();
    assert!(!text_source.rich_text);
    assert_eq!(text_source.inline_attachments, 0);
    assert_eq!(text_source.content, "plain");

    assert_eq!(snapshot.original_files.len(), 1);
    let file = &snapshot.original_files[0];
    assert_eq!(file.identity, "src-file");
    assert_eq!(file.notebook_identity, "nb-1");
    assert_eq!(file.kind, ElectronSourceKind::File);
    assert_eq!(file.title, "Report");
    assert_eq!(file.content, "pdf body");
    assert_eq!(file.raw_content.as_deref(), Some("raw pdf"));
    assert_eq!(file.file_name.as_deref(), Some("report.pdf"));
    assert_eq!(file.file_path.as_deref(), Some("/tmp/report.pdf"));
    assert_eq!(file.source_url, None);
    assert_eq!(file.media_url, None);
}

#[test]
fn absent_optional_columns_read_as_null() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("app-data.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            r#"
        CREATE TABLE notebooks (notebook_id TEXT NOT NULL, title TEXT NOT NULL);
        CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL DEFAULT '');
        CREATE TABLE notebook_sources (
            source_id TEXT NOT NULL, notebook_id TEXT NOT NULL, title TEXT NOT NULL,
            type TEXT NOT NULL, content TEXT NOT NULL DEFAULT ''
        );
        "#,
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notebooks(notebook_id, title) VALUES ('nb-1', 'KB')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notebook_sources(source_id, notebook_id, title, type, content)
             VALUES ('src-file', 'nb-1', 'R', 'file', 'body')",
            [],
        )
        .unwrap();
    drop(connection);

    let snapshot = ElectronNotes::open_read_only(&path)
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(snapshot.notebooks[0].description, None);
    assert_eq!(snapshot.notebooks[0].cover_image, None);
    assert_eq!(snapshot.original_files[0].raw_content, None);
    assert_eq!(snapshot.original_files[0].file_name, None);
    assert_eq!(snapshot.original_files[0].file_path, None);
}

#[test]
fn snapshot_wires_into_v2_handoff() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("app-data.db");
    let connection = Connection::open(&path).unwrap();
    full_schema(&connection);
    connection
        .execute(
            "INSERT INTO notebooks(notebook_id, title) VALUES ('nb-1', 'KB')",
            [],
        )
        .unwrap();
    connection
        .execute(
            r#"INSERT INTO notes(id, title, content) VALUES (1, 'Rich', '<p>hi <img src="data:image/png;base64,AA"></p>')"#,
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notebook_sources(source_id, notebook_id, title, type, content) VALUES ('src-text', 'nb-1', 'Plain', 'text', 'plain')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO notebook_sources(source_id, notebook_id, title, type, content, raw_content, file_name, file_path) VALUES ('src-file', 'nb-1', 'Report', 'file', 'pdf', 'raw', 'report.pdf', '/tmp/report.pdf')",
            [],
        )
        .unwrap();
    drop(connection);

    let reader = ElectronNotes::open_read_only(&path).unwrap();
    let handoff = reader.snapshot().unwrap().into_v2("electron-public-notes");
    assert_eq!(handoff.format, "uebox-notes");
    assert_eq!(handoff.version, 2);
    assert_eq!(handoff.source, "electron-public-notes");
    assert_eq!(handoff.notebooks.len(), 1);
    assert_eq!(handoff.notes.len(), 2);
    assert_eq!(handoff.original_files.len(), 1);

    let rich = handoff
        .notes
        .iter()
        .find(|n| n.identity == "note:1")
        .unwrap();
    assert_eq!(rich.format, NoteFormat::Html);
    assert!(rich.content.contains("data:image/png"));
    assert!(rich.attachments.is_empty());
    let plain = handoff
        .notes
        .iter()
        .find(|n| n.identity == "source:src-text")
        .unwrap();
    assert_eq!(plain.format, NoteFormat::Plain);
    assert_eq!(handoff.original_files[0].identity, "src-file");
    assert_eq!(handoff.original_files[0].kind, ElectronSourceKind::File);
}

#[test]
fn rejects_missing_wrong_or_incomplete_databases() {
    let temporary = tempfile::tempdir().unwrap();

    let missing = temporary.path().join("missing.db");
    assert!(matches!(
        ElectronNotes::open_read_only(&missing),
        Err(NotesError::Io)
    ));

    let text = temporary.path().join("not-sqlite.db");
    std::fs::write(&text, b"just text").unwrap();
    assert!(matches!(
        ElectronNotes::open_read_only(&text),
        Err(NotesError::Corrupt)
    ));

    let incomplete = temporary.path().join("incomplete.db");
    let connection = Connection::open(&incomplete).unwrap();
    connection
        .execute_batch("CREATE TABLE unrelated (x INTEGER)")
        .unwrap();
    drop(connection);
    assert!(matches!(
        ElectronNotes::open_read_only(&incomplete),
        Err(NotesError::Unsupported)
    ));

    assert!(matches!(
        ElectronNotes::open_read_only(Path::new("relative/app-data.db")),
        Err(NotesError::InvalidInput)
    ));
}
