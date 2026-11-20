//! Offline asset metadata imported from explicitly selected legacy vault archives,
//! plus explicitly registered local vaults with bounded, explicit rescans.

mod manifest;
mod scan;
mod store;

pub use manifest::{Archive, Asset, Folder, Vault};
pub use scan::ScanLimits;
pub use store::{AssetIndex, AssetLocation, ImportReport, LocalVault, ScanReport};

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
