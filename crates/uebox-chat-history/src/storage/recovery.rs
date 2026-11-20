use std::{
    ffi::{CStr, CString},
    fs::{Metadata, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

use super::{database_metadata, io_error, metadata_at, HistoryError};

const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

pub(super) fn check_files(database: &Path, journal: &Path) -> Result<(), HistoryError> {
    let parent = database.parent().ok_or(HistoryError::InvalidInput)?;
    let parent_metadata = metadata_at(parent)?.ok_or(HistoryError::RecoveryRequired)?;
    let database_metadata = match database_metadata(database) {
        Err(HistoryError::Missing | HistoryError::Unsupported) => {
            return Err(HistoryError::RecoveryRequired)
        }
        result => result?,
    };
    let journal_metadata = metadata_at(journal)?.ok_or(HistoryError::RecoveryRequired)?;
    // geteuid has no parameters and observes the actual owner of these files.
    let owner = unsafe { libc::geteuid() };
    if !parent_metadata.is_dir()
        || !database_metadata.is_file()
        || !journal_metadata.is_file()
        || journal_metadata.nlink() != 1
        || journal_metadata.len() <= 512
        || [&parent_metadata, &database_metadata, &journal_metadata]
            .iter()
            .any(|metadata| metadata.uid() != owner || metadata.mode() & 0o077 != 0)
        || database_metadata.dev() != parent_metadata.dev()
        || journal_metadata.dev() != parent_metadata.dev()
    {
        return Err(HistoryError::RecoveryRequired);
    }

    for (path, expected) in [
        (parent, &parent_metadata),
        (database, &database_metadata),
        (journal, &journal_metadata),
    ] {
        let name =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| HistoryError::InvalidInput)?;
        if !has_no_acl(&name)
            || !metadata_at(path)?.is_some_and(|metadata| same_metadata(&metadata, expected))
        {
            return Err(HistoryError::RecoveryRequired);
        }
    }

    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(journal)
        .map_err(io_error)?;
    let opened = file.metadata().map_err(io_error)?;
    if !same_metadata(&opened, &journal_metadata) {
        return Err(HistoryError::RecoveryRequired);
    }

    let mut magic = [0; 8];
    file.read_exact(&mut magic).map_err(io_error)?;
    if magic != JOURNAL_MAGIC {
        return Err(HistoryError::RecoveryRequired);
    }
    // The bundled pager requires this magic at EOF to follow a super-journal
    // path. Reject such journals without interpreting any embedded path.
    file.seek(SeekFrom::End(-8)).map_err(io_error)?;
    file.read_exact(&mut magic).map_err(io_error)?;
    if magic == JOURNAL_MAGIC || !same_metadata(&file.metadata().map_err(io_error)?, &opened) {
        return Err(HistoryError::RecoveryRequired);
    }
    Ok(())
}

fn same_metadata(actual: &Metadata, expected: &Metadata) -> bool {
    actual.dev() == expected.dev()
        && actual.ino() == expected.ino()
        && actual.uid() == expected.uid()
        && actual.mode() == expected.mode()
        && actual.len() == expected.len()
        && actual.nlink() == expected.nlink()
        && actual.mtime() == expected.mtime()
        && actual.mtime_nsec() == expected.mtime_nsec()
        && actual.ctime() == expected.ctime()
        && actual.ctime_nsec() == expected.ctime_nsec()
}

#[cfg(target_os = "linux")]
fn has_no_acl(path: &CStr) -> bool {
    // A zero-length buffer asks for the length without reading any ACL bytes.
    let result = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            c"system.posix_acl_access".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENODATA)
}

#[cfg(target_os = "macos")]
fn has_no_acl(path: &CStr) -> bool {
    unsafe extern "C" {
        fn acl_get_link_np(path: *const libc::c_char, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_free(value: *mut libc::c_void) -> libc::c_int;
    }
    const ACL_TYPE_EXTENDED: libc::c_int = 0x100;
    // Darwin returns ENOENT for a file without an extended ACL; the caller
    // rechecks identity to distinguish that case from a missing path.
    let acl = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        return io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT);
    }
    unsafe { acl_free(acl) };
    false
}
