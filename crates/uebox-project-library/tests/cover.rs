use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;
use uebox_project_library::{CoverFormat, EngineKind, ErrorKind, ProjectLibrary};

fn project(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::create_dir(&path).unwrap();
    path
}

fn registered(dir: &TempDir, name: &str) -> (ProjectLibrary, i64) {
    let mut library = ProjectLibrary::create_new(dir.path().join("library.sqlite")).unwrap();
    let record = library
        .register(EngineKind::Godot, name, project(dir, name))
        .unwrap();
    (library, record.id)
}

fn png() -> Vec<u8> {
    png_with_dims(100, 100)
}

fn png_with_dims(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

fn jpeg() -> Vec<u8> {
    jpeg_with_dims(100, 100)
}

fn jpeg_with_dims(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8]; // SOI
    bytes.extend_from_slice(&[0xff, 0xc0]); // SOF0
    bytes.extend_from_slice(&7u16.to_be_bytes()); // segment length
    bytes.push(8); // precision
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[0xff, 0xd9]); // EOI
    bytes
}

fn webp() -> Vec<u8> {
    webp_with_dims(100, 100)
}

fn webp_with_dims(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes.extend_from_slice(b"VP8X");
    bytes.extend_from_slice(&10u32.to_le_bytes());
    bytes.push(0); // flags
    bytes.extend_from_slice(&[0, 0, 0]); // reserved
    let width_minus_one = width - 1;
    let height_minus_one = height - 1;
    bytes.extend_from_slice(&[
        (width_minus_one & 0xff) as u8,
        ((width_minus_one >> 8) & 0xff) as u8,
        ((width_minus_one >> 16) & 0xff) as u8,
    ]);
    bytes.extend_from_slice(&[
        (height_minus_one & 0xff) as u8,
        ((height_minus_one >> 8) & 0xff) as u8,
        ((height_minus_one >> 16) & 0xff) as u8,
    ]);
    bytes
}

#[test]
fn cover_round_trips_through_a_private_copy_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let source = dir.path().join("chosen.png");
    let (mut library, id) = registered(&dir, "game");

    let record = library
        .set_cover(&covers, id, Some(&source), &png())
        .unwrap();
    assert_eq!(record.format, CoverFormat::Png);
    assert_eq!(record.filename, format!("{id}.png"));
    assert_eq!(record.source.as_deref(), Some(source.as_path()));

    assert_eq!(library.get_cover(id).unwrap(), Some(record.clone()));
    assert_eq!(library.read_cover(&covers, id).unwrap(), Some(png()));
    assert_eq!(
        fs::read(covers.join(record.filename.clone())).unwrap(),
        png()
    );
    drop(library);

    let library = ProjectLibrary::open_existing(dir.path().join("library.sqlite")).unwrap();
    assert_eq!(library.get_cover(id).unwrap(), Some(record));
    assert_eq!(library.read_cover(&covers, id).unwrap(), Some(png()));
}

#[test]
fn png_jpeg_and_webp_are_accepted_and_other_input_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    for (format, bytes, extension) in [
        (CoverFormat::Png, png(), "png"),
        (CoverFormat::Jpeg, jpeg(), "jpg"),
        (CoverFormat::Webp, webp(), "webp"),
    ] {
        let record = library.set_cover(&covers, id, None, &bytes).unwrap();
        assert_eq!(record.format, format);
        assert_eq!(record.filename, format!("{id}.{extension}"));
        assert_eq!(library.read_cover(&covers, id).unwrap(), Some(bytes));
    }
    for invalid in [
        b"not an image".to_vec(),
        b"\x89PNG".to_vec(),
        vec![0xff, 0xd8],                     // too short for JPEG magic
        b"RIFF\x00\x00\x00\x00WAVE".to_vec(), // RIFF but not WEBP
    ] {
        assert_eq!(
            library
                .set_cover(&covers, id, None, &invalid)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidInput,
            "rejected: {invalid:?}"
        );
    }
    assert_eq!(
        library.set_cover(&covers, id, None, &[]).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        library
            .set_cover(&covers, id, None, &vec![0u8; 32 * 1024 * 1024 + 1])
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn cover_dimensions_are_bounded_against_decompression_bombs() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    // Large but acceptable: 4096×4096 = 16 MiPx within the 33.5 MiPx budget.
    for bytes in [
        png_with_dims(4096, 4096),
        jpeg_with_dims(4096, 4096),
        webp_with_dims(4096, 4096),
    ] {
        library.set_cover(&covers, id, None, &bytes).unwrap();
    }
    // A tiny file declaring a huge canvas is rejected.
    for bytes in [
        png_with_dims(8193, 1),    // width exceeds MAX_COVER_DIMENSION
        png_with_dims(8192, 8192), // 67 MiPx exceeds MAX_COVER_PIXELS
        jpeg_with_dims(9000, 100),
        webp_with_dims(100, 9000),
    ] {
        assert_eq!(
            library
                .set_cover(&covers, id, None, &bytes)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidInput
        );
    }
}

#[test]
fn replacing_a_cover_keeps_one_file_and_updates_format() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    let png_record = library.set_cover(&covers, id, None, &png()).unwrap();
    let record = library.set_cover(&covers, id, None, &webp()).unwrap();
    assert_eq!(record.format, CoverFormat::Webp);
    assert_eq!(record.filename, format!("{id}.webp"));
    assert_eq!(library.read_cover(&covers, id).unwrap(), Some(webp()));
    // The stale copy from the previous format is removed.
    assert!(!covers.join(&png_record.filename).exists());
    assert_eq!(fs::read_dir(&covers).unwrap().count(), 1);
}

#[test]
fn remove_cover_removes_the_row_and_the_private_file() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    let record = library.set_cover(&covers, id, None, &png()).unwrap();
    assert!(fs::read(covers.join(&record.filename)).is_ok());

    assert!(library.remove_cover(&covers, id).unwrap());
    assert!(!library.remove_cover(&covers, id).unwrap());
    assert_eq!(library.get_cover(id).unwrap(), None);
    assert_eq!(library.read_cover(&covers, id).unwrap(), None);
    assert!(fs::read_dir(&covers).unwrap().next().is_none());
}

#[test]
fn set_cover_for_unknown_project_does_not_write_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    assert_eq!(
        library
            .set_cover(&covers, id + 1, None, &png())
            .unwrap_err()
            .kind,
        ErrorKind::Missing
    );
    assert!(!covers.exists());
    assert_eq!(library.get_cover(id + 1).unwrap(), None);
}

#[test]
fn remove_project_cascades_the_cover_row_but_not_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    let record = library.set_cover(&covers, id, None, &png()).unwrap();
    assert!(library.remove(id).unwrap());
    assert_eq!(library.get_cover(id).unwrap(), None);
    // The private copy is the host's responsibility to delete; `remove` only
    // cascades database rows, so the file lingers until `remove_cover` runs.
    assert!(fs::read(covers.join(&record.filename)).is_ok());
}

#[test]
fn read_cover_treats_a_missing_file_as_absent() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    let record = library.set_cover(&covers, id, None, &png()).unwrap();
    fs::remove_file(covers.join(&record.filename)).unwrap();
    assert_eq!(library.read_cover(&covers, id).unwrap(), None);
    assert_eq!(library.get_cover(id).unwrap(), Some(record));
}

#[test]
fn read_cover_rejects_a_tampered_filename_without_reading_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    library.set_cover(&covers, id, None, &png()).unwrap();
    // Plant a decoy file inside the covers dir and repoint the DB row at it.
    fs::write(covers.join("decoy.png"), b"do not read me").unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("library.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE project_covers SET filename = 'decoy.png' WHERE project_id = ?1",
            [id],
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        library.read_cover(&covers, id).unwrap_err().kind,
        ErrorKind::Corrupt
    );
    // The decoy was never returned as cover bytes.
    assert_eq!(
        fs::read(covers.join("decoy.png")).unwrap(),
        b"do not read me"
    );
}

#[cfg(unix)]
#[test]
fn read_cover_rejects_a_symlinked_private_copy() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    let record = library.set_cover(&covers, id, None, &png()).unwrap();
    let copy = covers.join(&record.filename);
    fs::remove_file(&copy).unwrap();
    let decoy = dir.path().join("decoy.png");
    fs::write(&decoy, b"secret").unwrap();
    std::os::unix::fs::symlink(&decoy, &copy).unwrap();

    assert_eq!(
        library.read_cover(&covers, id).unwrap_err().kind,
        ErrorKind::Unsupported
    );
}

#[cfg(unix)]
#[test]
fn read_cover_rejects_a_symlinked_covers_directory() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real-covers");
    let (mut library, id) = registered(&dir, "game");
    library.set_cover(&real, id, None, &png()).unwrap();
    let link = dir.path().join("covers-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert_eq!(
        library.read_cover(&link, id).unwrap_err().kind,
        ErrorKind::Unsupported
    );
}

#[test]
fn read_cover_bounds_the_read_when_the_file_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let covers = dir.path().join("covers");
    let (mut library, id) = registered(&dir, "game");
    let record = library.set_cover(&covers, id, None, &png()).unwrap();
    // Replace the private copy with a file over the size limit.
    fs::write(
        covers.join(&record.filename),
        vec![0u8; 32 * 1024 * 1024 + 1],
    )
    .unwrap();

    assert_eq!(
        library.read_cover(&covers, id).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
}
