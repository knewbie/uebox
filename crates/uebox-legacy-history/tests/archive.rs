use std::{fs, path::Path};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uebox_legacy_history::{
    AttachmentKind, Availability, HistoryError, ImageMediaType, ImportSummary, LegacyHistory,
    ManifestSource, MessageRole, MessageStatus,
};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+WZ1sAAAAASUVORK5CYII=";

fn embedded(bytes: &[u8], media_type: &str) -> Value {
    json!({"kind":"image","availability":"embedded","mediaType":media_type,
        "base64":STANDARD.encode(bytes),"sha256":Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>()})
}

fn chat(sid: &str, text: &str) -> Value {
    json!({"id":sid,"title":"Recovered","createdAt":123,"updatedAt":456,
        "messages":[{"role":"user","text":text,"status":"completed"},
        {"role":"assistant","text":"","status":"interrupted"}]})
}

fn manifest(chats: Vec<Value>) -> Value {
    json!({"format":"uebox-visible-chat-history","version":1,"chats":chats})
}

fn import(archive: &mut LegacyHistory, document: &Value) -> Result<ImportSummary, HistoryError> {
    let bytes = serde_json::to_vec(document).unwrap();
    archive.import_manifest(ManifestSource::Bytes(&bytes))
}

#[test]
fn validated_fixture_round_trips_with_separate_attachment_lookup() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("archive.sqlite");
    let source = directory.path().join("export.json");
    let png = STANDARD.decode(PNG).unwrap();
    let mut item = chat("../opaque/session", "原始可见内容");
    item["messages"][0]["attachments"] = json!([
        {"kind":"document","availability":"unavailable"},
        embedded(&png, "image/png")
    ]);
    fs::write(&source, serde_json::to_vec(&manifest(vec![item])).unwrap()).unwrap();
    let mut archive = LegacyHistory::new(&db).unwrap();
    assert!(archive.list_chats(10, 0).unwrap().is_empty());
    assert!(!db.exists());
    assert_eq!(
        archive
            .import_manifest(ManifestSource::Path(&source))
            .unwrap(),
        ImportSummary {
            inserted: 1,
            already_present: 0,
            embedded_attachments: 1,
            unavailable_attachments: 1,
        }
    );
    drop(archive);

    let mut archive = LegacyHistory::new(&db).unwrap();
    let summaries = archive.list_chats(10, 0).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].sid, "../opaque/session");
    assert_eq!(summaries[0].message_count, 2);
    let detail = archive.get_chat(&summaries[0].id).unwrap().unwrap();
    assert_eq!(detail.summary, summaries[0]);
    assert_eq!(detail.messages[0].role, MessageRole::User);
    assert_eq!(detail.messages[0].text, "原始可见内容");
    assert_eq!(detail.messages[1].status, MessageStatus::Interrupted);
    let [unavailable, image] = detail.messages[0].attachments.as_slice() else {
        panic!("expected two attachments")
    };
    assert_eq!(unavailable.kind, AttachmentKind::Document);
    assert_eq!(unavailable.availability, Availability::Unavailable);
    assert_eq!(unavailable.id, None);
    assert_eq!(unavailable.media_type, None);
    assert_eq!(
        serde_json::to_value(unavailable).unwrap(),
        json!({"kind": "document", "availability": "unavailable"})
    );
    assert_eq!(image.kind, AttachmentKind::Image);
    assert_eq!(image.availability, Availability::Embedded);
    assert_eq!(image.media_type, Some(ImageMediaType::Png));
    assert_eq!(
        serde_json::to_value(image).unwrap(),
        json!({"kind": "image", "availability": "embedded", "id": image.id, "mediaType": "image/png"})
    );
    let downloaded = archive
        .get_attachment(image.id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(downloaded.bytes, png);
    assert_eq!(downloaded.media_type, ImageMediaType::Png);
    assert_eq!(archive.get_attachment("a-9999").unwrap(), None);
}

#[test]
fn exact_duplicate_is_idempotent_but_same_sid_can_have_multiple_snapshots() {
    let temporary = tempfile::tempdir().unwrap();
    let db = temporary.path().join("archive.sqlite");
    let mut archive = LegacyHistory::new(&db).unwrap();
    let mut original = chat("session", "first");
    original["messages"][0]["attachments"] = json!([
        {"kind":"video","availability":"unavailable"},
        embedded(&STANDARD.decode(PNG).unwrap(), "image/png")
    ]);
    let original = manifest(vec![original]);
    assert_eq!(import(&mut archive, &original).unwrap().inserted, 1);
    let pretty = serde_json::to_vec_pretty(&original).unwrap();
    assert_eq!(
        archive
            .import_manifest(ManifestSource::Bytes(&pretty))
            .unwrap(),
        ImportSummary {
            inserted: 0,
            already_present: 1,
            embedded_attachments: 1,
            unavailable_attachments: 1,
        }
    );
    assert_eq!(
        import(&mut archive, &manifest(vec![chat("session", "changed")]))
            .unwrap()
            .inserted,
        1
    );
    let all = archive.list_chats(10, 0).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(archive.list_chats(1, 1).unwrap(), &all[1..2]);
    assert_ne!(all[0].id, all[1].id);
    assert!(all.iter().all(|item| item.sid == "session"));
    let mut texts = all
        .iter()
        .map(|item| {
            archive
                .get_chat(&item.id)
                .unwrap()
                .unwrap()
                .messages
                .remove(0)
                .text
        })
        .collect::<Vec<_>>();
    texts.sort();
    assert_eq!(texts, ["changed", "first"]);
}

#[test]
fn an_invalid_later_chat_preserves_existing_database_and_does_not_publish_a_new_one() {
    let temporary = tempfile::tempdir().unwrap();
    let db = temporary.path().join("archive.sqlite");
    let bad_db = temporary.path().join("bad.sqlite");
    let mut archive = LegacyHistory::new(&db).unwrap();
    import(&mut archive, &manifest(vec![chat("committed", "keep")])).unwrap();
    drop(archive);
    let before = fs::read(&db).unwrap();

    let mut bad = chat("rejected", "data");
    bad["messages"][0]["credentials"] = json!("fixture must never be accepted");
    let invalid = manifest(vec![chat("not inserted", "second"), bad]);
    let mut archive = LegacyHistory::new(&db).unwrap();
    assert_eq!(
        import(&mut archive, &invalid),
        Err(HistoryError::InvalidManifest)
    );
    drop(archive);
    assert_eq!(fs::read(&db).unwrap(), before);

    let mut fresh = LegacyHistory::new(&bad_db).unwrap();
    assert_eq!(
        import(&mut fresh, &invalid),
        Err(HistoryError::InvalidManifest)
    );
    assert!(!bad_db.exists());
    assert_eq!(
        import(&mut fresh, &manifest(vec![])).unwrap(),
        ImportSummary::default()
    );
    assert!(!bad_db.exists());
}

#[test]
fn invalid_wire_shapes_enums_hashes_and_image_encodings_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let db = temporary.path().join("archive.sqlite");
    let mut archive = LegacyHistory::new(&db).unwrap();
    let png = STANDARD.decode(PNG).unwrap();
    let good = embedded(&png, "image/png");
    let mut attachments = vec![
        json!({"kind":"image","availability":"unavailable","path":"/fixture/do-not-read"}),
        json!({"kind":"file","availability":"embedded"}),
        embedded(&png, "image/jpeg"),
    ];
    for (field, value) in [
        ("sha256", json!("0".repeat(64))),
        ("sha256", json!("A".repeat(64))),
        ("base64", json!(PNG.trim_end_matches('='))),
        ("base64", json!(PNG.replace('/', "_"))),
    ] {
        let mut attachment = good.clone();
        attachment[field] = value;
        attachments.push(attachment);
    }
    for attachment in attachments {
        let mut item = chat("session", "visible");
        item["messages"][0]["attachments"] = json!([attachment]);
        assert_eq!(
            import(&mut archive, &manifest(vec![item])),
            Err(HistoryError::InvalidManifest)
        );
    }
    for (field, value) in [
        ("role", json!("tool")),
        ("status", json!("pending")),
        ("attachments", Value::Null),
    ] {
        let mut item = chat("session", "visible");
        item["messages"][0][field] = value;
        assert_eq!(
            import(&mut archive, &manifest(vec![item])),
            Err(HistoryError::InvalidManifest)
        );
    }
    assert_eq!(
        import(
            &mut archive,
            &manifest(vec![chat("duplicate", "one"), chat("duplicate", "two")])
        ),
        Err(HistoryError::InvalidManifest)
    );
    assert_eq!(
        archive.import_manifest(ManifestSource::Bytes(
            b"{\"format\":\"uebox-visible-chat-history\",\"version\":1,\"version\":1,\"chats\":[]}"
        )),
        Err(HistoryError::InvalidManifest)
    );
    assert_eq!(
        archive.import_manifest(ManifestSource::Bytes(b"{\"format\":\"\xff\"}")),
        Err(HistoryError::InvalidManifest)
    );
    assert!(!db.exists());
}

#[test]
fn input_and_decoded_attachment_limits_are_enforced_before_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let db = temporary.path().join("archive.sqlite");
    let mut archive = LegacyHistory::new(&db).unwrap();
    let too_large = vec![b' '; 16 * 1024 * 1024 + 1];
    assert_eq!(
        archive.import_manifest(ManifestSource::Bytes(&too_large)),
        Err(HistoryError::LimitExceeded)
    );
    assert_eq!(
        import(
            &mut archive,
            &manifest((0..4097).map(|id| chat(&id.to_string(), "x")).collect())
        ),
        Err(HistoryError::LimitExceeded)
    );
    let mut item = chat("message-limit", "visible");
    item["messages"] = Value::Array(vec![
        json!({"role":"user","text":"x","status":"completed"});
        100001
    ]);
    assert_eq!(
        import(&mut archive, &manifest(vec![item])),
        Err(HistoryError::LimitExceeded)
    );

    let mut image = vec![0; 2 * 1024 * 1024];
    image[..3].copy_from_slice(&[0xff, 0xd8, 0xff]);
    let attachment = embedded(&image, "image/jpeg");
    image.push(0);
    let mut item = chat("image-limit", "visible");
    item["messages"][0]["attachments"] = json!([embedded(&image, "image/jpeg")]);
    assert_eq!(
        import(&mut archive, &manifest(vec![item.clone()])),
        Err(HistoryError::LimitExceeded)
    );
    item["messages"][0]["attachments"] = json!([
        attachment.clone(),
        attachment.clone(),
        attachment.clone(),
        attachment.clone(),
        embedded(&[0xff, 0xd8, 0xff], "image/jpeg")
    ]);
    assert_eq!(
        import(&mut archive, &manifest(vec![item.clone()])),
        Err(HistoryError::LimitExceeded)
    );
    assert!(!db.exists());
    item["messages"][0]["attachments"] = json!([
        attachment.clone(),
        attachment.clone(),
        attachment.clone(),
        attachment
    ]);
    let report = import(&mut archive, &manifest(vec![item])).unwrap();
    assert_eq!(report.embedded_attachments, 4);
}

fn assert_rejects_db(path: &Path, expected: HistoryError) {
    let before = fs::read(path).unwrap();
    let mut archive = LegacyHistory::new(path).unwrap();
    assert_eq!(archive.list_chats(10, 0), Err(expected));
    assert_eq!(
        import(&mut archive, &manifest(vec![chat("new", "rejected")])),
        Err(expected)
    );
    drop(archive);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn incompatible_or_modified_archive_is_never_replaced() {
    let temporary = tempfile::tempdir().unwrap();
    let other = temporary.path().join("other.sqlite");
    let connection = rusqlite::Connection::open(&other).unwrap();
    connection
        .execute_batch("CREATE TABLE unrelated(k TEXT)")
        .unwrap();
    drop(connection);
    assert_rejects_db(&other, HistoryError::Unsupported);

    let db = temporary.path().join("other-version.sqlite");
    let mut archive = LegacyHistory::new(&db).unwrap();
    import(&mut archive, &manifest(vec![chat("committed", "keep")])).unwrap();
    drop(archive);
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch("CREATE TABLE unrelated(k TEXT)")
        .unwrap();
    drop(connection);
    assert_rejects_db(&db, HistoryError::Unsupported);
}

#[test]
fn positional_json_arrays_are_not_wire_objects() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("archive.sqlite");
    let mut archive = LegacyHistory::new(&database).unwrap();
    for document in [
        json!(["uebox-visible-chat-history", 1, []]),
        manifest(vec![json!(["sid", "title", 1, 1, []])]),
        manifest(vec![json!({
            "id": "sid", "title": "", "createdAt": 1, "updatedAt": 1,
            "messages": [["user", "visible", "completed", []]]
        })]),
    ] {
        assert_eq!(
            import(&mut archive, &document),
            Err(HistoryError::InvalidManifest)
        );
        assert!(!database.exists());
    }
}

#[test]
fn existing_sidecars_are_rejected_without_recovery_or_publication() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("archive.sqlite");
    let journal = temp.path().join("archive.sqlite-journal");
    let wal = temp.path().join("archive.sqlite-wal");
    std::fs::write(&journal, b"fixture journal").unwrap();
    let mut archive = LegacyHistory::new(&database).unwrap();
    assert_eq!(
        archive.list_chats(10, 0),
        Err(HistoryError::RecoveryRequired)
    );
    assert!(!database.exists());
    std::fs::write(&wal, b"fixture wal").unwrap();
    assert_eq!(archive.list_chats(10, 0), Err(HistoryError::Unsupported));
    assert_eq!(std::fs::read(&journal).unwrap(), b"fixture journal");
    assert_eq!(std::fs::read(&wal).unwrap(), b"fixture wal");
}

#[test]
fn a_conflicting_snapshot_rolls_back_earlier_writes_in_the_same_import() {
    let temporary = tempfile::tempdir().unwrap();
    let db = temporary.path().join("archive.sqlite");
    let original = chat("original", "first");
    let mut archive = LegacyHistory::new(&db).unwrap();
    import(&mut archive, &manifest(vec![original.clone()])).unwrap();
    drop(archive);

    // Simulate a schema-valid row whose stored content disagrees with its snapshot ID.
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute(
            "UPDATE messages SET text = 'mismatching fixture' WHERE position = 0",
            [],
        )
        .unwrap();
    drop(connection);
    let bytes_before = fs::read(&db).unwrap();

    let mut archive = LegacyHistory::new(&db).unwrap();
    assert_eq!(
        import(
            &mut archive,
            &manifest(vec![chat("earlier", "write"), original])
        ),
        Err(HistoryError::Conflict)
    );
    let chats = archive.list_chats(10, 0).unwrap();
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].sid, "original");
    drop(archive);
    assert_eq!(fs::read(&db).unwrap(), bytes_before);
}
