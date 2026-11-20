//! Explicit local scans are not implemented on this platform: registration itself
//! fails with Unavailable rather than pretending the root was verified.
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use super::Metadata;
use crate::Error;

pub(crate) struct Directory;
pub(crate) struct Entries;

impl Directory {
    pub(crate) fn open_root(_: &Path) -> Result<Self, Error> {
        Err(Error::Unavailable)
    }
    pub(crate) fn metadata(&self) -> Result<Metadata, Error> {
        Err(Error::Unavailable)
    }
    pub(crate) fn child(&self, _: &OsStr, _: &str) -> Result<Self, Error> {
        Err(Error::Unavailable)
    }
    pub(crate) fn entry(&self, _: &OsStr) -> Result<Metadata, Error> {
        Err(Error::Unavailable)
    }
    pub(crate) fn entries(&self) -> Result<Entries, Error> {
        Err(Error::Unavailable)
    }
}

impl Iterator for Entries {
    type Item = Result<OsString, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        None
    }
}
