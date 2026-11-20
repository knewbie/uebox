//! Explicit user-selected project covers stored as private, validated copies.
//!
//! A cover is never discovered from the project on disk. The host supplies the
//! bytes of an image the user explicitly chose, the crate verifies the magic
//! signature (PNG, JPEG, or WebP only, at most 32 MiB) AND its declared pixel
//! dimensions (bounded width/height and total pixels, so a tiny compressed file
//! cannot decode into a huge image), writes a private copy into a host-supplied
//! covers directory, and records the detected format plus the source path as
//! provenance. The source path is stored for display and is never opened.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use super::{
    failure, io_error, open_readonly, require_registered, sqlite_error, verify_database_identity,
    ErrorKind, LibraryError, ProjectLibrary,
};

pub(crate) const CREATE_COVERS: &str = "CREATE TABLE project_covers (
    project_id INTEGER PRIMARY KEY CHECK (project_id > 0),
    format TEXT NOT NULL CHECK (format IN ('png', 'jpeg', 'webp')),
    filename TEXT NOT NULL CHECK (length(CAST(filename AS BLOB)) BETWEEN 1 AND 1024 AND instr(filename, char(0)) = 0),
    source TEXT CHECK (source IS NULL OR (length(CAST(source AS BLOB)) BETWEEN 1 AND 4096 AND instr(source, char(0)) = 0))
) STRICT, WITHOUT ROWID";

const MAX_COVER_BYTES: usize = 32 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 4096;
/// Maximum acceptable cover width or height in pixels.
pub const MAX_COVER_DIMENSION: u32 = 8192;
/// Maximum acceptable total pixels (width × height) for a cover.
pub const MAX_COVER_PIXELS: u64 = 33_554_432;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverFormat {
    Png,
    Jpeg,
    Webp,
}

impl CoverFormat {
    fn as_db(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
        }
    }

    fn from_db(value: &str) -> Result<Self, LibraryError> {
        match value {
            "png" => Ok(Self::Png),
            "jpeg" => Ok(Self::Jpeg),
            "webp" => Ok(Self::Webp),
            _ => Err(failure(
                ErrorKind::Unsupported,
                format!("unknown cover format: {value}"),
            )),
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverRecord {
    pub project_id: i64,
    pub format: CoverFormat,
    /// File name of the private copy inside the covers directory.
    pub filename: String,
    /// The explicitly selected source path, retained only as provenance.
    pub source: Option<PathBuf>,
}

fn cover_filename(project_id: i64, format: CoverFormat) -> String {
    format!("{project_id}.{}", format.extension())
}

fn validate_stored_filename(
    project_id: i64,
    format: CoverFormat,
    filename: &str,
) -> Result<(), LibraryError> {
    if filename != cover_filename(project_id, format) {
        return Err(failure(
            ErrorKind::Corrupt,
            "cover record filename does not match its project and format",
        ));
    }
    Ok(())
}

pub(super) fn cover_from_join(
    project_id: i64,
    format: Option<String>,
    filename: Option<String>,
    source: Option<String>,
) -> Result<Option<CoverRecord>, LibraryError> {
    let Some(format) = format else {
        return Ok(None);
    };
    let filename = filename.ok_or_else(|| {
        failure(
            ErrorKind::Corrupt,
            "cover row is missing its private-copy filename",
        )
    })?;
    let format = CoverFormat::from_db(&format)?;
    validate_stored_filename(project_id, format, &filename)?;
    Ok(Some(CoverRecord {
        project_id,
        format,
        filename,
        source: source.map(PathBuf::from),
    }))
}

impl ProjectLibrary {
    /// Store an explicitly selected cover for a registered project.
    ///
    /// `bytes` must be a complete PNG, JPEG, or WebP file (magic signature and
    /// declared dimensions are verified) at most 32 MiB. The private copy
    /// replaces any previous one for the same project; a stale copy from a
    /// different format is removed. `source` is stored only as provenance and
    /// is never opened.
    pub fn set_cover(
        &mut self,
        covers_dir: &Path,
        project_id: i64,
        source: Option<&Path>,
        bytes: &[u8],
    ) -> Result<CoverRecord, LibraryError> {
        if project_id <= 0 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project ID must be positive",
            ));
        }
        let format = detect_format(bytes)?;
        dimensions(format, bytes)?;
        let source_text = source.map(validate_source).transpose()?;
        if !self.project_registered(project_id)? {
            return Err(failure(
                ErrorKind::Missing,
                "project registration does not exist",
            ));
        }
        let previous = self.get_cover(project_id)?;
        let directory = covers_directory(covers_dir)?;
        let filename = cover_filename(project_id, format);
        write_private_copy(&directory, &filename, bytes)?;

        let record = self.write_transaction("cover", |transaction| {
            require_registered(transaction, project_id)?;
            transaction
                .execute(
                    "INSERT INTO project_covers (project_id, format, filename, source)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(project_id) DO UPDATE SET
                         format = excluded.format,
                         filename = excluded.filename,
                         source = excluded.source",
                    params![project_id, format.as_db(), filename, source_text],
                )
                .map_err(|error| sqlite_error("set project cover", error))?;
            read_cover_row(transaction, project_id)?.ok_or_else(|| {
                failure(
                    ErrorKind::Other,
                    "cover row disappeared after it was written",
                )
            })
        });
        match record {
            Ok(record) => {
                if let Some(previous) = previous {
                    if previous.filename != record.filename {
                        let _ = fs::remove_file(directory.join(&previous.filename));
                    }
                }
                Ok(record)
            }
            Err(error) => {
                // Best-effort rollback of the already-published private copy.
                let _ = fs::remove_file(directory.join(&filename));
                Err(error)
            }
        }
    }

    /// The cover metadata for one project, or `None` when it has none.
    pub fn get_cover(&self, project_id: i64) -> Result<Option<CoverRecord>, LibraryError> {
        if project_id <= 0 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project ID must be positive",
            ));
        }
        let verified = open_readonly(&self.database, Some(self.identity))?;
        let record = read_cover_row(&verified.connection, project_id)?;
        verify_database_identity(&verified.connection, &self.database, self.identity)?;
        Ok(record)
    }

    /// Read the private cover copy back. `None` means there is no cover row or
    /// the private copy file is missing; a missing file is not an error. The
    /// stored file name must match the derived name for the project and format,
    /// and the copy must be a regular, non-symlink file within the size limit.
    pub fn read_cover(
        &self,
        covers_dir: &Path,
        project_id: i64,
    ) -> Result<Option<Vec<u8>>, LibraryError> {
        if project_id <= 0 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project ID must be positive",
            ));
        }
        let Some(directory) = existing_covers_dir(covers_dir)? else {
            return Ok(None);
        };
        let Some(record) = self.get_cover(project_id)? else {
            return Ok(None);
        };
        read_private_copy(&directory.join(&record.filename))
    }

    /// Remove the cover metadata and, when a copy exists, the private file.
    /// Returns `false` when the project had no cover.
    pub fn remove_cover(
        &mut self,
        covers_dir: &Path,
        project_id: i64,
    ) -> Result<bool, LibraryError> {
        if project_id <= 0 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "project ID must be positive",
            ));
        }
        let directory = existing_covers_dir(covers_dir)?;
        let record = self.get_cover(project_id)?;
        let removed = self.write_transaction("cover removal", |transaction| {
            Ok(transaction
                .execute(
                    "DELETE FROM project_covers WHERE project_id = ?1",
                    [project_id],
                )
                .map_err(|error| sqlite_error("remove project cover", error))?
                != 0)
        })?;
        if removed {
            if let (Some(directory), Some(record)) = (directory, record) {
                let _ = fs::remove_file(directory.join(&record.filename));
            }
        }
        Ok(removed)
    }
}

fn detect_format(bytes: &[u8]) -> Result<CoverFormat, LibraryError> {
    if bytes.is_empty() {
        return Err(failure(ErrorKind::InvalidInput, "cover image is empty"));
    }
    if bytes.len() > MAX_COVER_BYTES {
        return Err(failure(
            ErrorKind::InvalidInput,
            "cover image exceeds the 32 MiB limit",
        ));
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(CoverFormat::Png);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok(CoverFormat::Jpeg);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok(CoverFormat::Webp);
    }
    Err(failure(
        ErrorKind::InvalidInput,
        "cover is not a PNG, JPEG, or WebP image",
    ))
}

/// Verify the declared pixel dimensions of a validated cover. Width/height and
/// total pixels are bounded so a small, highly-compressed file cannot decode
/// into an enormous image when the host displays it.
fn dimensions(format: CoverFormat, bytes: &[u8]) -> Result<(u32, u32), LibraryError> {
    let (width, height) = match format {
        CoverFormat::Png => png_dimensions(bytes)?,
        CoverFormat::Jpeg => jpeg_dimensions(bytes)?,
        CoverFormat::Webp => webp_dimensions(bytes)?,
    };
    if width == 0 || height == 0 {
        return Err(failure(
            ErrorKind::InvalidInput,
            "cover declares a zero dimension",
        ));
    }
    if width > MAX_COVER_DIMENSION || height > MAX_COVER_DIMENSION {
        return Err(failure(
            ErrorKind::InvalidInput,
            format!("cover {width}×{height} exceeds the {MAX_COVER_DIMENSION}px dimension limit"),
        ));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_COVER_PIXELS {
        return Err(failure(
            ErrorKind::InvalidInput,
            format!("cover {width}×{height} exceeds the total pixel limit"),
        ));
    }
    Ok((width, height))
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), LibraryError> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return Err(failure(
            ErrorKind::InvalidInput,
            "PNG cover has no IHDR header",
        ));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("PNG width"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("PNG height"));
    Ok((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), LibraryError> {
    let mut offset = 2usize; // skip the SOI marker
    while offset + 2 <= bytes.len() {
        if bytes[offset] != 0xff {
            return Err(failure(
                ErrorKind::InvalidInput,
                "JPEG cover has a malformed marker",
            ));
        }
        let mut marker = offset + 1;
        while marker < bytes.len() && bytes[marker] == 0xff {
            marker += 1; // fill bytes
        }
        if marker >= bytes.len() {
            return Err(failure(ErrorKind::InvalidInput, "JPEG cover is truncated"));
        }
        let id = bytes[marker];
        // Standalone markers carry no length: TEM, RST0-7, SOI, EOI.
        if id == 0x01 || (0xd0..=0xd9).contains(&id) {
            if id == 0xd9 {
                return Err(failure(
                    ErrorKind::InvalidInput,
                    "JPEG cover has no SOF dimensions",
                ));
            }
            offset = marker + 1;
            continue;
        }
        if marker + 3 > bytes.len() {
            return Err(failure(
                ErrorKind::InvalidInput,
                "JPEG cover segment is truncated",
            ));
        }
        let length = u16::from_be_bytes([bytes[marker + 1], bytes[marker + 2]]) as usize;
        if length < 2 {
            return Err(failure(
                ErrorKind::InvalidInput,
                "JPEG cover segment length is invalid",
            ));
        }
        // SOF0-15 except DHT (C4), JPG (C8), DAC (CC).
        if (0xc0..=0xcf).contains(&id) && id != 0xc4 && id != 0xc8 && id != 0xcc {
            if marker + 8 > bytes.len() {
                return Err(failure(
                    ErrorKind::InvalidInput,
                    "JPEG cover SOF is truncated",
                ));
            }
            let height = u16::from_be_bytes([bytes[marker + 4], bytes[marker + 5]]) as u32;
            let width = u16::from_be_bytes([bytes[marker + 6], bytes[marker + 7]]) as u32;
            return Ok((width, height));
        }
        offset = marker + 1 + length;
    }
    Err(failure(
        ErrorKind::InvalidInput,
        "JPEG cover has no dimensions",
    ))
}

fn webp_dimensions(bytes: &[u8]) -> Result<(u32, u32), LibraryError> {
    if bytes.len() < 16 || &bytes[8..12] != b"WEBP" {
        return Err(failure(ErrorKind::InvalidInput, "WebP cover is truncated"));
    }
    match &bytes[12..16] {
        b"VP8 " => {
            if bytes.len() < 30 || bytes[20..23] != [0x9d, 0x01, 0x2a] {
                return Err(failure(
                    ErrorKind::InvalidInput,
                    "lossy WebP cover has no frame header",
                ));
            }
            let width = u32::from(u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff);
            let height = u32::from(u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3fff);
            Ok((width, height))
        }
        b"VP8L" => {
            if bytes.len() < 25 || bytes[20] != 0x2f {
                return Err(failure(
                    ErrorKind::InvalidInput,
                    "lossless WebP cover has no signature",
                ));
            }
            let width = (u32::from(bytes[21]) | ((u32::from(bytes[22]) & 0x3f) << 8)) + 1;
            let height = (((u32::from(bytes[22]) >> 6) & 0x03)
                | (u32::from(bytes[23]) << 2)
                | ((u32::from(bytes[24]) & 0x0f) << 10))
                + 1;
            Ok((width, height))
        }
        b"VP8X" => {
            if bytes.len() < 30 {
                return Err(failure(
                    ErrorKind::InvalidInput,
                    "extended WebP cover is truncated",
                ));
            }
            let width = 1
                + (u32::from(bytes[24])
                    | (u32::from(bytes[25]) << 8)
                    | (u32::from(bytes[26]) << 16));
            let height = 1
                + (u32::from(bytes[27])
                    | (u32::from(bytes[28]) << 8)
                    | (u32::from(bytes[29]) << 16));
            Ok((width, height))
        }
        _ => Err(failure(
            ErrorKind::InvalidInput,
            "WebP cover has an unknown chunk type",
        )),
    }
}

fn validate_source(source: &Path) -> Result<String, LibraryError> {
    let text = source
        .to_str()
        .filter(|text| !text.contains('\0'))
        .ok_or_else(|| {
            failure(
                ErrorKind::InvalidInput,
                "cover source path must be UTF-8 without NUL",
            )
        })?;
    if text.len() > MAX_SOURCE_BYTES {
        return Err(failure(
            ErrorKind::InvalidInput,
            "cover source path exceeds the byte limit",
        ));
    }
    Ok(text.to_string())
}

/// Resolve the covers directory, creating it with owner-only permissions when
/// absent. A symlink or reparse point is rejected.
fn covers_directory(path: &Path) -> Result<PathBuf, LibraryError> {
    if !path.is_absolute() {
        return Err(failure(
            ErrorKind::InvalidInput,
            "covers directory must be absolute",
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_symlink_or_reparse(&metadata) {
                return Err(failure(
                    ErrorKind::Unsupported,
                    "covers directory must not be a symlink or reparse point",
                ));
            }
            if metadata.is_dir() {
                Ok(path.to_path_buf())
            } else {
                Err(failure(
                    ErrorKind::InvalidInput,
                    "covers path is not a directory",
                ))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| io_error("create covers directory", error))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(|error| io_error("secure covers directory", error))?;
            }
            Ok(path.to_path_buf())
        }
        Err(error) => Err(io_error("access covers directory", error)),
    }
}

/// Like `covers_directory`, but never creates: `Ok(None)` when the directory
/// does not exist (there is nothing to read or remove).
fn existing_covers_dir(path: &Path) -> Result<Option<PathBuf>, LibraryError> {
    if !path.is_absolute() {
        return Err(failure(
            ErrorKind::InvalidInput,
            "covers directory must be absolute",
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_symlink_or_reparse(&metadata) {
                return Err(failure(
                    ErrorKind::Unsupported,
                    "covers directory must not be a symlink or reparse point",
                ));
            }
            if metadata.is_dir() {
                Ok(Some(path.to_path_buf()))
            } else {
                Err(failure(
                    ErrorKind::InvalidInput,
                    "covers path is not a directory",
                ))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("access covers directory", error)),
    }
}

fn write_private_copy(directory: &Path, filename: &str, bytes: &[u8]) -> Result<(), LibraryError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".cover-incomplete-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o600));
    }
    let mut staged = builder
        .tempfile_in(directory)
        .map_err(|error| io_error("create cover staging file", error))?;
    staged
        .write_all(bytes)
        .map_err(|error| io_error("write cover staging file", error))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| io_error("sync cover staging file", error))?;
    let staged_path = staged.into_temp_path();
    fs::rename(&staged_path, directory.join(filename))
        .map_err(|error| io_error("publish cover private copy", error))?;
    Ok(())
}

/// Read a private copy back, rejecting symlinks/reparse points and bounding the
/// read so a replaced file cannot exceed the size limit. `Ok(None)` when absent.
fn read_private_copy(path: &Path) -> Result<Option<Vec<u8>>, LibraryError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("stat cover file", error)),
    };
    if is_symlink_or_reparse(&metadata) {
        return Err(failure(
            ErrorKind::Unsupported,
            "cover file must not be a symlink or reparse point",
        ));
    }
    if !metadata.is_file() {
        return Err(failure(
            ErrorKind::InvalidInput,
            "cover path is not a regular file",
        ));
    }
    if metadata.len() > MAX_COVER_BYTES as u64 {
        return Err(failure(
            ErrorKind::InvalidInput,
            "cover file exceeds the size limit",
        ));
    }
    let file = File::open(path).map_err(|error| io_error("open cover file", error))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_COVER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read cover file", error))?;
    if bytes.len() > MAX_COVER_BYTES {
        return Err(failure(
            ErrorKind::InvalidInput,
            "cover file grew beyond the size limit",
        ));
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn is_symlink_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_symlink_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(any(unix, windows)))]
fn is_symlink_or_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

fn read_cover_row(
    connection: &Connection,
    project_id: i64,
) -> Result<Option<CoverRecord>, LibraryError> {
    connection
        .query_row(
            "SELECT format, filename, source FROM project_covers WHERE project_id = ?1",
            [project_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_error("read project cover", error))?
        .map(|(format, filename, source)| {
            let format = CoverFormat::from_db(&format)?;
            validate_stored_filename(project_id, format, &filename)?;
            Ok(CoverRecord {
                project_id,
                format,
                filename,
                source: source.map(PathBuf::from),
            })
        })
        .transpose()
}
