use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uebox_credentials::{CredentialPurpose, CredentialRef};
use uebox_mcp_client::canonical_endpoint;

use crate::profile_storage::{
    acquire_lock, checked_file_exists, private_directory, ProfileError, ProfileLock,
};

pub(super) const INVALID: &str = "mcp_catalog_invalid";
pub(super) const MISSING: &str = "mcp_endpoint_missing";
pub(super) const UNAVAILABLE: &str = "mcp_catalog_unavailable";
const MAX_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 64;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Endpoint {
    pub(super) id: String,
    title: String,
    pub(super) url: String,
    pub(super) authenticated: bool,
}

impl Endpoint {
    pub(super) fn new(title: &str, url: &str, authenticated: bool) -> Result<Self, &'static str> {
        let title = title.trim();
        if !valid_title(title) {
            return Err(INVALID);
        }
        let url = canonical_endpoint(url.trim()).map_err(|_| "mcp_invalid_endpoint")?;
        let mut bytes = [0; 16];
        getrandom::getrandom(&mut bytes).map_err(|_| UNAVAILABLE)?;
        let id = bytes
            .into_iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            id,
            title: title.to_owned(),
            url,
            authenticated,
        })
    }

    pub(super) fn credential(&self) -> Result<CredentialRef, &'static str> {
        let mut hash = Sha256::new();
        hash.update(self.id.as_bytes());
        hash.update([0]);
        hash.update(self.url.as_bytes());
        let key = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        CredentialRef::new(CredentialPurpose::McpBearer, &key).map_err(|_| INVALID)
    }
}

fn valid_title(title: &str) -> bool {
    !title.is_empty()
        && title.len() <= 256
        && title.trim() == title
        && !title.chars().any(char::is_control)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    endpoints: Vec<Endpoint>,
}

impl Document {
    fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 || self.endpoints.len() > MAX_ENTRIES {
            return Err(INVALID);
        }
        let mut ids = HashSet::new();
        let mut urls = HashSet::new();
        for entry in &self.endpoints {
            if entry.id.len() != 32
                || !entry
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || !valid_title(&entry.title)
                || canonical_endpoint(&entry.url).as_deref() != Ok(entry.url.as_str())
                || !ids.insert(&entry.id)
                || !urls.insert(&entry.url)
            {
                return Err(INVALID);
            }
        }
        Ok(())
    }
}

pub(super) struct Catalog {
    path: PathBuf,
    _lock: ProfileLock,
}

impl Catalog {
    pub(super) fn open(root: &Path, create: bool) -> Result<Option<Self>, &'static str> {
        let Some(directory) = private_directory(root, "mcp", create).map_err(storage_error)? else {
            return Ok(None);
        };
        let lock = acquire_lock(&directory.join("catalog.lock")).map_err(storage_error)?;
        Ok(Some(Self {
            path: directory.join("endpoints.json"),
            _lock: lock,
        }))
    }

    pub(super) fn read(&self) -> Result<Vec<Endpoint>, &'static str> {
        if !checked_file_exists(&self.path).map_err(storage_error)? {
            return Ok(Vec::new());
        }
        let mut encoded = Vec::new();
        File::open(&self.path)
            .map_err(|_| UNAVAILABLE)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut encoded)
            .map_err(|_| UNAVAILABLE)?;
        if encoded.len() as u64 > MAX_BYTES {
            return Err(INVALID);
        }
        let document: Document = serde_json::from_slice(&encoded).map_err(|_| INVALID)?;
        document.validate()?;
        Ok(document.endpoints)
    }

    pub(super) fn write(&self, endpoints: Vec<Endpoint>) -> Result<Vec<Endpoint>, &'static str> {
        let document = Document {
            version: 1,
            endpoints,
        };
        document.validate()?;
        let encoded = serde_json::to_vec(&document).map_err(|_| INVALID)?;
        if encoded.len() as u64 > MAX_BYTES {
            return Err(INVALID);
        }
        let exists = checked_file_exists(&self.path).map_err(storage_error)?;
        let directory = self.path.parent().ok_or(UNAVAILABLE)?;
        let mut builder = tempfile::Builder::new();
        builder.prefix(".endpoints-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o600));
        }
        let mut file = builder.tempfile_in(directory).map_err(|_| UNAVAILABLE)?;
        file.write_all(&encoded).map_err(|_| UNAVAILABLE)?;
        file.as_file().sync_all().map_err(|_| UNAVAILABLE)?;
        if exists {
            file.persist(&self.path).map_err(|_| UNAVAILABLE)?;
        } else {
            file.persist_noclobber(&self.path)
                .map_err(|_| UNAVAILABLE)?;
        }
        Ok(document.endpoints)
    }
}

fn storage_error(error: ProfileError) -> &'static str {
    match error {
        ProfileError::Invalid => INVALID,
        ProfileError::Locked => "mcp_busy",
        ProfileError::Unavailable => UNAVAILABLE,
    }
}
