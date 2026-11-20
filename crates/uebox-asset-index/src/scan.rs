//! Bounded, explicit metadata collection. No asset is opened or read.
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    manifest::{relative_path, valid_text},
    Asset, Error, Folder,
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "scan/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "scan/windows.rs"]
mod platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
#[path = "scan/unsupported.rs"]
mod platform;
pub(crate) use platform::Directory;

const MAX_SAFE_SIZE: u64 = 9_007_199_254_740_991;
const SCAN_DEADLINE: Duration = Duration::from_secs(30);

/// Callers may lower these limits, but cannot exceed the hard ceilings.
/// Depth zero permits root files only; ignored entries still consume max_entries.
#[derive(Clone, Copy, Debug)]
pub struct ScanLimits {
    pub max_depth: usize,
    pub max_folders: usize,
    pub max_assets: usize,
    pub max_entries: usize,
    pub max_metadata_bytes: usize,
    pub max_file_bytes: u64,
    pub max_total_file_bytes: u64,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_folders: 8192,
            max_assets: 32768,
            max_entries: 65536,
            max_metadata_bytes: 16 * 1024 * 1024,
            max_file_bytes: 1024 * 1024 * 1024 * 1024,
            max_total_file_bytes: 16 * 1024 * 1024 * 1024 * 1024,
        }
    }
}

impl ScanLimits {
    pub(crate) fn validate(self) -> Result<Self, Error> {
        let hard = Self::default();
        if self.max_depth > hard.max_depth
            || self.max_folders > hard.max_folders
            || self.max_assets > hard.max_assets
            || self.max_entries > hard.max_entries
            || self.max_metadata_bytes > hard.max_metadata_bytes
            || self.max_file_bytes > MAX_SAFE_SIZE
            || self.max_total_file_bytes > MAX_SAFE_SIZE
        {
            return Err(Error::Invalid);
        }
        Ok(self)
    }
}

pub(crate) enum Kind {
    File,
    Directory,
    Ignore,
}
pub(crate) struct Metadata {
    pub kind: Kind,
    pub size: u64,
    pub modified: Option<String>,
    pub key: String,
    pub volume: u64,
}

impl Metadata {
    pub(crate) fn ignored() -> Self {
        Self {
            kind: Kind::Ignore,
            size: 0,
            modified: None,
            key: String::new(),
            volume: 0,
        }
    }
}

pub(crate) struct Snapshot {
    pub folders: Vec<Folder>,
    pub assets: Vec<Asset>,
    pub skipped: usize,
    pub bytes: usize,
}

struct Walker<'a> {
    vault: &'a str,
    limits: ScanLimits,
    started: Instant,
    volume: u64,
    entries: usize,
    total_file_bytes: u64,
    identities: HashSet<String>,
    directories: HashSet<String>,
    snapshot: Snapshot,
}

/// A canonical spelling without following symlinks. Parent aliases such as macOS
/// /var are resolved, but the final component is kept literal: the platform opener
/// then checks every component without following, so the root cannot be a link.
pub(crate) fn root_path(root: &Path) -> Result<PathBuf, Error> {
    let text = root.to_str().ok_or(Error::Invalid)?;
    if !root.is_absolute()
        || !valid_text(text, 8192)
        || (text.starts_with("//") || text.starts_with("\\\\"))
    {
        return Err(Error::Invalid);
    }
    #[cfg(windows)]
    if !matches!(root.components().next(), Some(Component::Prefix(p))
        if matches!(p.kind(), std::path::Prefix::Disk(_)))
    {
        return Err(Error::Invalid);
    }
    let file_name = root.file_name().ok_or(Error::Invalid)?;
    let parent = root.parent().ok_or(Error::Invalid)?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::Missing
        } else {
            Error::Unavailable
        }
    })?;
    Ok(parent.join(file_name))
}

/// Versioned FNV-1a-128 labels are identifiers, never an authorization/hash-of-content check.
/// Duplicate labels are rejected both in the candidate and against the stored snapshot.
pub(crate) fn stable_id(kind: &str, parts: &[&str]) -> String {
    let mut hash: u128 = 0x6c62272e07bb014262b821756295c58d;
    for part in std::iter::once(kind).chain(parts.iter().copied()) {
        for byte in part.bytes().chain(std::iter::once(0)) {
            hash = (hash ^ u128::from(byte)).wrapping_mul(0x0000000001000000000000000000013b);
        }
    }
    format!("local-v1-{kind}-{hash:032x}")
}

pub(crate) fn collect(
    root: &Directory,
    vault: &str,
    limits: ScanLimits,
) -> Result<Snapshot, Error> {
    let metadata = root.metadata()?;
    let mut walker = Walker {
        vault,
        limits,
        started: Instant::now(),
        volume: metadata.volume,
        entries: 0,
        total_file_bytes: 0,
        identities: HashSet::new(),
        directories: HashSet::from([metadata.key]),
        snapshot: Snapshot {
            folders: vec![],
            assets: vec![],
            skipped: 0,
            bytes: 0,
        },
    };
    walker.visit(root, "", None, 0)?;
    walker.deadline()?;
    Ok(walker.snapshot)
}

impl Walker<'_> {
    fn deadline(&self) -> Result<(), Error> {
        if self.started.elapsed() > SCAN_DEADLINE {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }

    fn visit(
        &mut self,
        directory: &Directory,
        prefix: &str,
        parent: Option<&str>,
        depth: usize,
    ) -> Result<(), Error> {
        let mut entries = directory.entries()?;
        loop {
            self.deadline()?;
            let Some(name) = entries.next() else {
                return Ok(());
            };
            let name = name?;
            self.entries += 1;
            if self.entries > self.limits.max_entries {
                return Err(Error::Limit);
            }
            let Some(text) = name
                .to_str()
                .filter(|name| !name.is_empty() && valid_text(name, 1024))
            else {
                self.snapshot.skipped += 1;
                continue;
            };
            let path = if prefix.is_empty() {
                text.to_owned()
            } else {
                format!("{prefix}/{text}")
            };
            if !relative_path(&path) {
                self.snapshot.skipped += 1;
                continue;
            }
            let metadata = directory.entry(&name)?;
            if matches!(metadata.kind, Kind::Ignore) || metadata.volume != self.volume {
                self.snapshot.skipped += 1;
                continue;
            }
            match metadata.kind {
                Kind::Directory => self.folder(directory, &name, &path, parent, depth, metadata)?,
                Kind::File => self.asset(text, &path, parent, metadata)?,
                Kind::Ignore => unreachable!(),
            }
        }
    }

    fn folder(
        &mut self,
        directory: &Directory,
        name: &std::ffi::OsStr,
        path: &str,
        parent: Option<&str>,
        depth: usize,
        metadata: Metadata,
    ) -> Result<(), Error> {
        if depth >= self.limits.max_depth || self.snapshot.folders.len() >= self.limits.max_folders
        {
            return Err(Error::Limit);
        }
        if !self.directories.insert(metadata.key.clone()) {
            self.snapshot.skipped += 1;
            return Ok(());
        }
        let child = directory.child(name, &metadata.key)?;
        let identity = stable_id("folder", &[self.vault, path]);
        self.unique(&identity)?;
        let folder = Folder {
            identity: identity.clone(),
            parent_identity: parent.map(str::to_owned),
            name: name.to_str().ok_or(Error::Invalid)?.to_owned(),
        };
        self.budget(&folder)?;
        self.snapshot.folders.push(folder);
        self.visit(&child, path, Some(&identity), depth + 1)
    }

    fn asset(
        &mut self,
        name: &str,
        path: &str,
        parent: Option<&str>,
        metadata: Metadata,
    ) -> Result<(), Error> {
        if self.snapshot.assets.len() >= self.limits.max_assets
            || metadata.size > self.limits.max_file_bytes
        {
            return Err(Error::Limit);
        }
        self.total_file_bytes = self
            .total_file_bytes
            .checked_add(metadata.size)
            .ok_or(Error::Limit)?;
        if self.total_file_bytes > self.limits.max_total_file_bytes {
            return Err(Error::Limit);
        }
        let identity = stable_id("asset", &[self.vault, path]);
        self.unique(&identity)?;
        let extension = Path::new(name)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| valid_text(value, 64))
            .map(str::to_owned);
        let asset = Asset {
            identity,
            folder_identity: parent.map(str::to_owned),
            name: name.to_owned(),
            relative_path: Some(path.to_owned()),
            size: Some(metadata.size),
            extension,
            modified_time: metadata.modified,
            tags: vec![],
        };
        self.budget(&asset)?;
        self.snapshot.assets.push(asset);
        Ok(())
    }

    fn unique(&mut self, identity: &str) -> Result<(), Error> {
        if self.identities.insert(identity.to_owned()) {
            Ok(())
        } else {
            Err(Error::Conflict)
        }
    }

    fn budget(&mut self, row: &impl serde::Serialize) -> Result<(), Error> {
        let bytes = serde_json::to_vec(row).map_err(|_| Error::Invalid)?.len();
        self.snapshot.bytes = self.snapshot.bytes.checked_add(bytes).ok_or(Error::Limit)?;
        if self.snapshot.bytes > self.limits.max_metadata_bytes {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }
}
