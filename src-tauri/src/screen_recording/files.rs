//! Files created by one recording attempt, never a caller-supplied destination.
use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub(super) const UNAVAILABLE: &str = "screen_recording_unavailable";
pub(super) const PUBLISH_FAILED: &str = "screen_recording_publish_failed";

#[derive(Debug)]
pub(super) struct Workspace {
    directory: PathBuf,
    #[cfg(unix)]
    directory_metadata: Metadata,
    output: PathBuf,
    destination: PathBuf,
    name: String,
}

fn private_directory(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return false;
        }
    }
    true
}

impl Workspace {
    /// `root` has already been resolved inside this app's private local profile.
    pub(super) fn prepare(root: &Path, nonce: [u8; 16]) -> Result<Self, &'static str> {
        if !root.is_absolute() || !private_directory(root) {
            return Err(UNAVAILABLE);
        }
        let random: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = format!("recording-{random}.mp4");
        let directory = root.join(format!(".pending-{random}"));
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        // Exclusive mkdir: a collision never adopts or cleans somebody else's directory.
        builder.create(&directory).map_err(|_| UNAVAILABLE)?;
        #[cfg(unix)]
        let directory_metadata = fs::symlink_metadata(&directory).map_err(|_| {
            let _ = fs::remove_dir(&directory);
            UNAVAILABLE
        })?;
        Ok(Self {
            output: directory.join("output.mp4"),
            destination: root.join(&name),
            directory,
            #[cfg(unix)]
            directory_metadata,
            name,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.output
    }

    pub(super) fn final_name(&self) -> &str {
        &self.name
    }

    pub(super) fn publish(&self) -> Result<(), &'static str> {
        let directory = fs::symlink_metadata(&self.directory).map_err(|_| PUBLISH_FAILED)?;
        if !self.owns_directory(&directory) {
            return Err(PUBLISH_FAILED);
        }
        let before = fs::symlink_metadata(&self.output).map_err(|_| PUBLISH_FAILED)?;
        if !before.is_file() || before.is_symlink() || before.len() == 0 {
            return Err(PUBLISH_FAILED);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if before.nlink() != 1 {
                return Err(PUBLISH_FAILED);
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.output)
            .map_err(|_| PUBLISH_FAILED)?;
        check_open_file(&file, &before)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| PUBLISH_FAILED)?;
        }
        file.sync_all().map_err(|_| PUBLISH_FAILED)?;
        // Same filesystem, complete file only, atomic no-clobber publication. In
        // particular, an existing destination symlink is never followed/replaced.
        fs::hard_link(&self.output, &self.destination).map_err(|_| PUBLISH_FAILED)
    }

    /// Remove exactly our expected temporary file and our empty directory.
    /// Unknown files/directories are preserved and surfaced as cleanupPending.
    pub(super) fn cleanup(&self) -> bool {
        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) if self.owns_directory(&metadata) => {}
            Ok(_) => return false,
            Err(error) => return error.kind() == ErrorKind::NotFound,
        }
        let file_removed = match fs::remove_file(&self.output) {
            Ok(()) => true,
            Err(error) => error.kind() == ErrorKind::NotFound,
        };
        let directory_removed = match fs::remove_dir(&self.directory) {
            Ok(()) => true,
            Err(error) => error.kind() == ErrorKind::NotFound,
        };
        file_removed && directory_removed
    }

    fn owns_directory(&self, metadata: &Metadata) -> bool {
        if !metadata.is_dir() || metadata.is_symlink() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            metadata.dev() == self.directory_metadata.dev()
                && metadata.ino() == self.directory_metadata.ino()
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Best effort for setup errors and unwinding; terminal callbacks explicitly
        // report cleanup failure. Never recursively sweep or remove final files.
        let _ = self.cleanup();
    }
}

fn check_open_file(file: &File, before: &Metadata) -> Result<(), &'static str> {
    let opened = file.metadata().map_err(|_| PUBLISH_FAILED)?;
    if !opened.is_file() || opened.len() == 0 {
        return Err(PUBLISH_FAILED);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != before.dev() || opened.ino() != before.ino() || opened.nlink() != 1 {
            return Err(PUBLISH_FAILED);
        }
    }
    #[cfg(not(unix))]
    let _ = before;
    Ok(())
}
