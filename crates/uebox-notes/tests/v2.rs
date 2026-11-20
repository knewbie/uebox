use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};
use uebox_notes::{
    AttachmentKind, ElectronSourceKind, LegacyNote, LegacyNotebook, NoteFormat, NotesError,
    NotesStore, V2Attachment, V2Handoff, V2Note, V2Notebook, V2OriginalFile,
};

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn attachment(
    kind: AttachmentKind,
    bytes: &[u8],
    media_type: Option<&str>,
    file_name: Option<&str>,
) -> V2Attachment {
    V2Attachment {
        kind,
        media_type: media_type.map(str::to_owned),
        file_name: file_name.map(str::to_owned),
        bytes: STANDARD.encode(bytes),
        sha256: sha256_hex(bytes),
    }
}

fn handoff() -> V2Handoff {
    V2Handoff {
        format: "uebox-notes".to_owned(),
        version: 2,
        source: "electron-public-notes".to_owned(),
        notebooks: vec![V2Notebook {
            identity: "nb-1".to_owned(),
            title: "Legacy KB".to_owned(),
        }],
        notes: vec![
            V2Note {
                identity: "note:1".to_owned(),
                notebook_identity: Some("nb-1".to_owned()),
                title: "Rich".to_owned(),
                content: "<p>hi</p>".to_owned(),
                format: NoteFormat::Html,
                attachments: vec![
                    attachment(
                        AttachmentKind::Binary,
                        b"hello",
                        Some("application/pdf"),
                        Some("report.pdf"),
                    ),
                    attachment(AttachmentKind::Html, b"<b>frag</b>", None, None),
                ],
            },
            V2Note {
                identity: "note:2".to_owned(),
                notebook_identity: None,
                title: "Markdown".to_owned(),
                content: "# title".to_owned(),
                format: NoteFormat::Markdown,
                attachments: Vec::new(),
            },
            V2Note {
                identity: "source:src-text".to_owned(),
                notebook_identity: Some("nb-1".to_owned()),
                title: "Plain".to_owned(),
                content: "plain text".to_owned(),
                format: NoteFormat::Plain,
                attachments: Vec::new(),
            },
        ],
        original_files: vec![V2OriginalFile {
            identity: "src-file".to_owned(),
            notebook_identity: "nb-1".to_owned(),
            kind: ElectronSourceKind::File,
            title: "Report".to_owned(),
            content: "pdf body".to_owned(),
            raw_content: Some("raw pdf".to_owned()),
            source_url: None,
            file_name: Some("report.pdf".to_owned()),
            file_path: Some("/tmp/report.pdf".to_owned()),
            media_url: None,
        }],
    }
}

#[test]
fn v1_database_is_migrated_to_v2_on_open() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("notes.sqlite");
    create_v1_database(&path);

    let store = NotesStore::open_existing(&path).unwrap();
    assert_eq!(store.get_note(1).unwrap().unwrap().content, "body");
    assert_eq!(store.note_format(1).unwrap(), None);
    assert!(store.note_attachments(1).unwrap().is_empty());
    assert!(store.list_original_files(1, None, 10).unwrap().is_empty());
    drop(store);

    // Reopening now sees the migrated v2 schema.
    let mut store = NotesStore::open_existing(&path).unwrap();
    assert!(store.create_notebook("After migration").unwrap() > 0);
}

fn create_v1_database(path: &Path) {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .pragma_update(None, "application_id", 0x5542_4e54_u32)
        .unwrap();
    connection
        .pragma_update(None, "user_version", 1_u32)
        .unwrap();
    for sql in [
        "CREATE TABLE notebooks (\n    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),\n    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) <= 1024 AND instr(title, char(0)) = 0)\n) STRICT",
        "CREATE TABLE notes (\n    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),\n    notebook_id INTEGER REFERENCES notebooks(id) ON DELETE CASCADE,\n    title TEXT NOT NULL CHECK (length(CAST(title AS BLOB)) <= 1024 AND instr(title, char(0)) = 0),\n    content TEXT NOT NULL CHECK (length(CAST(content AS BLOB)) <= 4194304 AND instr(content, char(0)) = 0)\n) STRICT",
        "CREATE TABLE legacy_identities (source TEXT NOT NULL, identity TEXT NOT NULL, record_id INTEGER NOT NULL, fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32), PRIMARY KEY (source, identity)) STRICT, WITHOUT ROWID",
        "CREATE INDEX notes_by_notebook ON notes(notebook_id, id)",
    ] {
        connection.execute_batch(sql).unwrap();
    }
    connection
        .execute("INSERT INTO notebooks(title) VALUES ('Old')", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO notes(notebook_id, title, content) VALUES (1, 'Note', 'body')",
            [],
        )
        .unwrap();
    drop(connection);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn v2_import_stores_format_attachments_and_original_files() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();

    let summary = store.import_v2(&handoff()).unwrap();
    assert_eq!(summary.notebooks_inserted, 1);
    assert_eq!(summary.notes_inserted, 3);
    assert_eq!(summary.attachments_inserted, 2);
    assert_eq!(summary.original_files_inserted, 1);

    let rich = store.get_note(1).unwrap().unwrap();
    assert_eq!(rich.content, "<p>hi</p>");
    assert_eq!(store.note_format(rich.id).unwrap(), Some(NoteFormat::Html));

    let attachments = store.note_attachments(rich.id).unwrap();
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0].kind, AttachmentKind::Binary);
    assert_eq!(
        attachments[0].media_type.as_deref(),
        Some("application/pdf")
    );
    assert_eq!(attachments[0].file_name.as_deref(), Some("report.pdf"));
    assert_eq!(attachments[0].position, 0);
    assert_eq!(attachments[1].kind, AttachmentKind::Html);
    assert_eq!(attachments[1].position, 1);

    let stored = store.get_attachment(attachments[0].id).unwrap().unwrap();
    assert_eq!(stored.bytes, b"hello");
    assert_eq!(
        stored.summary.sha256.as_slice(),
        Sha256::digest(b"hello").as_slice()
    );

    let standalone = store.get_note(2).unwrap().unwrap();
    assert_eq!(
        store.note_format(standalone.id).unwrap(),
        Some(NoteFormat::Markdown)
    );
    let plain = store.get_note(3).unwrap().unwrap();
    assert_eq!(
        store.note_format(plain.id).unwrap(),
        Some(NoteFormat::Plain)
    );

    let notebook_id = store.list_notebooks(None, 10).unwrap()[0].id;
    let files = store.list_original_files(notebook_id, None, 10).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].identity, "src-file");
    assert_eq!(files[0].kind, ElectronSourceKind::File);
    assert_eq!(files[0].title, "Report");
    assert_eq!(files[0].file_name.as_deref(), Some("report.pdf"));

    assert!(store
        .get_original_file(notebook_id + 1, files[0].id)
        .unwrap()
        .is_none());
    let body = store
        .get_original_file(notebook_id, files[0].id)
        .unwrap()
        .unwrap();
    assert_eq!(body.identity, "src-file");
    assert_eq!(body.kind, ElectronSourceKind::File);
    assert_eq!(body.title, "Report");
    assert_eq!(body.content, "pdf body");
    assert_eq!(body.raw_content.as_deref(), Some("raw pdf"));
    assert_eq!(body.file_name.as_deref(), Some("report.pdf"));
}

#[test]
fn v2_import_is_idempotent_and_conflicts_on_change() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();

    let first = store.import_v2(&handoff()).unwrap();
    let second = store.import_v2(&handoff()).unwrap();
    assert_eq!(second.notebooks_existing, 1);
    assert_eq!(second.notes_existing, 3);
    assert_eq!(second.original_files_existing, 1);
    assert_eq!(second.attachments_inserted, 0);
    assert_eq!(second.notes_inserted, 0);
    assert_ne!(first.notes_inserted, 0);

    let mut changed = handoff();
    changed.notes[0].content = "different".to_owned();
    assert_eq!(
        store.import_v2(&changed).unwrap_err(),
        NotesError::ImportConflict
    );
}

#[test]
fn v2_json_round_trips_through_import() {
    let document = handoff();
    let json = serde_json::to_string(&document).unwrap();
    let parsed: V2Handoff = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, document);

    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();
    let summary = store.import_v2_json(&json).unwrap();
    assert_eq!(summary.notes_inserted, 3);
    assert_eq!(summary.attachments_inserted, 2);

    let mut wrong_version = document;
    wrong_version.version = 1;
    let wrong = serde_json::to_string(&wrong_version).unwrap();
    assert_eq!(
        store.import_v2_json(&wrong).unwrap_err(),
        NotesError::InvalidInput
    );

    let mut tampered = handoff();
    tampered.notes[0].attachments[0].bytes = "not-base64!".to_owned();
    let tampered = serde_json::to_string(&tampered).unwrap();
    assert_eq!(
        store.import_v2_json(&tampered).unwrap_err(),
        NotesError::InvalidInput
    );
}

#[test]
fn attachment_change_conflicts_whole_batch() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();
    store.import_v2(&handoff()).unwrap();

    let mut changed_bytes = handoff();
    changed_bytes.notes[0].attachments[0].bytes = STANDARD.encode(b"different");
    changed_bytes.notes[0].attachments[0].sha256 = sha256_hex(b"different");
    assert_eq!(
        store.import_v2(&changed_bytes).unwrap_err(),
        NotesError::ImportConflict
    );

    let mut changed_media = handoff();
    changed_media.notes[0].attachments[0].media_type = Some("image/png".to_owned());
    assert_eq!(
        store.import_v2(&changed_media).unwrap_err(),
        NotesError::ImportConflict
    );
}

#[test]
fn v1_import_then_v2_same_content_enriches_without_id_change() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();

    store
        .import_legacy_batch(
            "electron-public-notes",
            &[LegacyNotebook {
                identity: "nb-1",
                title: "Legacy KB",
            }],
            &[LegacyNote {
                identity: "note:1",
                notebook_identity: Some("nb-1"),
                title: "Rich",
                content: "<p>hi</p>",
            }],
        )
        .unwrap();
    let notebook_id = store.list_notebooks(None, 10).unwrap()[0].id;
    let note_id = store.list_notes(Some(notebook_id), None, 10).unwrap()[0].id;
    assert_eq!(store.note_format(note_id).unwrap(), None);
    assert!(store.note_attachments(note_id).unwrap().is_empty());

    let summary = store.import_v2(&handoff()).unwrap();
    assert_eq!(summary.notebooks_existing, 1);
    assert_eq!(summary.notes_existing, 1);
    assert_eq!(summary.notes_inserted, 2);
    assert_eq!(summary.attachments_inserted, 2);
    assert_eq!(summary.original_files_inserted, 1);

    assert_eq!(store.note_format(note_id).unwrap(), Some(NoteFormat::Html));
    let attachments = store.note_attachments(note_id).unwrap();
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0].kind, AttachmentKind::Binary);
    assert_eq!(attachments[1].kind, AttachmentKind::Html);
}

#[test]
fn v1_import_then_v2_changed_content_conflicts() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = NotesStore::create_new(temporary.path().join("notes.sqlite")).unwrap();

    store
        .import_legacy_batch(
            "electron-public-notes",
            &[LegacyNotebook {
                identity: "nb-1",
                title: "Legacy KB",
            }],
            &[LegacyNote {
                identity: "note:1",
                notebook_identity: Some("nb-1"),
                title: "Rich",
                content: "old content",
            }],
        )
        .unwrap();

    let mut changed = handoff();
    changed.notes[0].content = "new content".to_owned();
    assert_eq!(
        store.import_v2(&changed).unwrap_err(),
        NotesError::ImportConflict
    );
}
