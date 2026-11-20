use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::Error;

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_FOLDERS: usize = 8192;
const MAX_ASSETS: usize = 32768;
const MAX_LOCAL_PATH: usize = 8192;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Archive {
    format: String,
    version: u32,
    source: String,
    pub(crate) vault: Vault,
    pub(crate) folders: Vec<Folder>,
    pub(crate) assets: Vec<Asset>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Vault {
    pub identity: String,
    pub name: String,
    pub root_path: String,
    pub index_available: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Folder {
    pub identity: String,
    pub parent_identity: Option<String>,
    pub name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub identity: String,
    pub folder_identity: Option<String>,
    pub name: String,
    pub relative_path: Option<String>,
    pub size: Option<u64>,
    pub extension: Option<String>,
    pub modified_time: Option<String>,
    pub tags: Vec<String>,
}

pub(crate) fn identity(value: &str) -> bool {
    !value.is_empty() && valid_text(value, 256)
}

pub(crate) fn valid_text(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}

fn rooted_path(path: &str) -> bool {
    if !valid_text(path, MAX_LOCAL_PATH) {
        return false;
    }
    if path.starts_with('/') && !path.starts_with("//") {
        return true;
    }
    let drive = matches!(
        path.as_bytes(),
        [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic()
    );
    if drive {
        return true;
    }
    let mut parts = path.split(['\\', '/']);
    if parts.next() != Some("") || parts.next() != Some("") {
        return false;
    }
    let valid_anchor = |part: &str| !matches!(part, "" | "." | ".." | "?") && !part.contains(':');
    parts.next().is_some_and(valid_anchor) && parts.next().is_some_and(valid_anchor)
}

pub(crate) fn relative_path(path: &str) -> bool {
    valid_text(path, MAX_LOCAL_PATH)
        && !path.contains(['\\', ':'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

impl Archive {
    pub fn parse(document: &str) -> Result<Self, Error> {
        if document.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Limit);
        }
        let archive: Self = serde_json::from_str(document).map_err(|_| Error::Invalid)?;
        if archive.version != 1 || archive.format != "uebox-local-asset-index" {
            return Err(Error::Version);
        }
        if archive.source != "electron-local-vault"
            || !identity(&archive.vault.identity)
            || !valid_text(&archive.vault.name, 1024)
            || !rooted_path(&archive.vault.root_path)
        {
            return Err(Error::Invalid);
        }
        if archive.folders.len() > MAX_FOLDERS || archive.assets.len() > MAX_ASSETS {
            return Err(Error::Limit);
        }
        if !archive.vault.index_available
            && (!archive.folders.is_empty() || !archive.assets.is_empty())
        {
            return Err(Error::Invalid);
        }

        let mut parents = HashMap::with_capacity(archive.folders.len());
        for folder in &archive.folders {
            if !identity(&folder.identity)
                || !valid_text(&folder.name, 1024)
                || parents
                    .insert(folder.identity.as_str(), folder.parent_identity.as_deref())
                    .is_some()
            {
                return Err(Error::Invalid);
            }
        }
        validate_parents(&parents)?;
        let mut assets = HashSet::with_capacity(archive.assets.len());
        for asset in &archive.assets {
            if !identity(&asset.identity)
                || !assets.insert(asset.identity.as_str())
                || !valid_text(&asset.name, 1024)
                || asset
                    .size
                    .is_some_and(|bytes| bytes > 9_007_199_254_740_991)
                || asset
                    .extension
                    .as_deref()
                    .is_some_and(|extension| !valid_text(extension, 64))
                || asset
                    .modified_time
                    .as_deref()
                    .is_some_and(|time| !valid_text(time, 64))
                || asset
                    .relative_path
                    .as_deref()
                    .is_some_and(|path| !relative_path(path))
                || asset
                    .folder_identity
                    .as_deref()
                    .is_some_and(|folder| !parents.contains_key(folder))
                || asset.tags.len() > 64
                || asset.tags.iter().any(|tag| !valid_text(tag, 256))
            {
                return Err(Error::Invalid);
            }
        }
        Ok(archive)
    }
}

fn validate_parents(parents: &HashMap<&str, Option<&str>>) -> Result<(), Error> {
    let mut validated = HashSet::with_capacity(parents.len());
    for &folder in parents.keys() {
        let mut rootward = HashSet::new();
        let mut current = Some(folder);
        while let Some(next) = current.filter(|next| !validated.contains(next)) {
            if !rootward.insert(next) {
                return Err(Error::Invalid);
            }
            current = *parents.get(next).ok_or(Error::Invalid)?;
        }
        validated.extend(rootward);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_accepts_cross_platform_roots_and_rejects_path_escape() {
        assert!(rooted_path("/folder/root"));
        assert!(rooted_path(r"D:\Library"));
        assert!(rooted_path(r"\\shared\library"));
        assert!(!rooted_path(r"\\?\C:\fixture"));
        assert!(!rooted_path(r"D:Library"));
        assert!(relative_path("characters/hero.uasset"));
        for invalid in [
            "",
            "../escape",
            "asset//nested",
            "C:/outside",
            r"a\b",
            "/root",
            "./root",
        ] {
            assert!(!relative_path(invalid), "{invalid}");
        }
    }

    #[test]
    fn folder_tree_rejects_cycles_and_unrecognized_parents() {
        assert!(validate_parents(&HashMap::from([("a", Some("b")), ("b", None)])).is_ok());
        assert!(validate_parents(&HashMap::from([("a", Some("b"))])).is_err());
        assert!(validate_parents(&HashMap::from([("a", Some("b")), ("b", Some("a"))])).is_err());
    }
}
