use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::profile_storage::{
    acquire_lock, checked_file_exists, private_directory, ProfileError, ProfileLock,
};

use super::catalog::Catalog;

const MAX_BYTES: u64 = 512 * 1024;
const UNAVAILABLE: &str = "settings_unavailable";
const INVALID: &str = "settings_invalid";

pub(super) struct CatalogFile {
    file: PathBuf,
    _lock: ProfileLock,
}

fn map_storage(error: ProfileError) -> &'static str {
    match error {
        ProfileError::Invalid => INVALID,
        ProfileError::Locked => "settings_locked",
        ProfileError::Unavailable => UNAVAILABLE,
    }
}

impl CatalogFile {
    pub fn open(profile: &Path, create: bool) -> Result<Option<Self>, &'static str> {
        let Some(directory) =
            private_directory(profile, "settings", create).map_err(map_storage)?
        else {
            return Ok(None);
        };
        let lock = acquire_lock(&directory.join("settings.lock")).map_err(map_storage)?;
        Ok(Some(Self {
            file: directory.join("models.json"),
            _lock: lock,
        }))
    }

    pub fn read(&self) -> Result<Catalog, &'static str> {
        if !checked_file_exists(&self.file).map_err(map_storage)? {
            return Ok(Catalog::default());
        }
        let file = File::open(&self.file).map_err(|_| UNAVAILABLE)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| UNAVAILABLE)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(INVALID);
        }
        let catalog: Catalog = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn write(&self, catalog: &Catalog) -> Result<(), &'static str> {
        catalog.validate()?;
        let directory = self.file.parent().ok_or(UNAVAILABLE)?;
        let current = checked_file_exists(&self.file).map_err(map_storage)?;
        let encoded = serde_json::to_vec(catalog).map_err(|_| INVALID)?;
        if encoded.len() as u64 > MAX_BYTES {
            return Err(INVALID);
        }
        let mut staging = tempfile::Builder::new();
        staging.prefix(".models-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            staging.permissions(std::fs::Permissions::from_mode(0o600));
        }
        let mut temporary = staging.tempfile_in(directory).map_err(|_| UNAVAILABLE)?;
        temporary.write_all(&encoded).map_err(|_| UNAVAILABLE)?;
        temporary.as_file().sync_all().map_err(|_| UNAVAILABLE)?;
        if current {
            temporary.persist(&self.file).map_err(|_| UNAVAILABLE)?;
        } else {
            temporary
                .persist_noclobber(&self.file)
                .map_err(|_| UNAVAILABLE)?;
        }
        Ok(())
    }
}
