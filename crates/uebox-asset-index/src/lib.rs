//! Offline asset metadata imported from explicitly selected legacy vault archives.

mod manifest;
mod store;

pub use manifest::{Archive, Asset, Folder, Vault};
pub use store::{AssetIndex, ImportReport};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Invalid,
    Limit,
    Version,
    Missing,
    Conflict,
    RecoveryRequired,
    Unavailable,
}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Unavailable
    }
}
