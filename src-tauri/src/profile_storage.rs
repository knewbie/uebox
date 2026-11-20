//! Local application data ownership and interprocess locking.
use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions, TryLockError},
    io::ErrorKind,
    path::Path,
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum ProfileError {
    Invalid,
    Locked,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct ProfileLock(File);

impl Drop for ProfileLock {
    fn drop(&mut self) {
        // A child forked on a different thread may temporarily inherit this
        // descriptor. Unlock explicitly so it cannot keep the operation busy.
        let _ = self.0.unlock();
    }
}

/// Prepare one private feature directory in the host-resolved local profile.
/// The profile lock only coordinates cooperating instances of UEBox.
pub(crate) fn private_directory(
    app_local_data: &Path,
    name: &str,
    create: bool,
) -> Result<Option<std::path::PathBuf>, ProfileError> {
    if !app_local_data.is_absolute() {
        return Err(ProfileError::Invalid);
    }
    if !ensure_directory(app_local_data, create, 0o022)? {
        return Ok(None);
    }
    let directory = app_local_data.join(name);
    Ok(ensure_directory(&directory, create, 0o077)?.then_some(directory))
}

fn metadata_at(path: &Path) -> Result<Option<Metadata>, ProfileError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ProfileError::Unavailable),
    }
}

fn ensure_directory(path: &Path, create: bool, forbidden_mode: u32) -> Result<bool, ProfileError> {
    let metadata = match metadata_at(path)? {
        Some(metadata) => metadata,
        None if !create => return Ok(false),
        None => {
            let mut builder = DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(path)
                .map_err(|_| ProfileError::Unavailable)?;
            metadata_at(path)?.ok_or(ProfileError::Unavailable)?
        }
    };
    if !metadata.is_dir() || is_link(&metadata) || !mode_allows(&metadata, forbidden_mode) {
        return Err(ProfileError::Invalid);
    }
    Ok(true)
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

fn mode_allows(_metadata: &Metadata, _forbidden_mode: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        _metadata.mode() & _forbidden_mode == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn check_file(metadata: &Metadata) -> Result<(), ProfileError> {
    if !metadata.is_file() || is_link(metadata) || !mode_allows(metadata, 0o077) {
        return Err(ProfileError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(ProfileError::Invalid);
        }
    }
    Ok(())
}

pub(crate) fn checked_file_exists(path: &Path) -> Result<bool, ProfileError> {
    let Some(metadata) = metadata_at(path)? else {
        return Ok(false);
    };
    check_file(&metadata)?;
    Ok(true)
}

pub(crate) fn acquire_lock(path: &Path) -> Result<ProfileLock, ProfileError> {
    checked_file_exists(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let lock = options.open(path).map_err(|_| ProfileError::Unavailable)?;
    check_file(&lock.metadata().map_err(|_| ProfileError::Unavailable)?)?;
    match lock.try_lock() {
        Ok(()) => Ok(ProfileLock(lock)),
        Err(TryLockError::WouldBlock) => Err(ProfileError::Locked),
        Err(TryLockError::Error(_)) => Err(ProfileError::Unavailable),
    }
}
