use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
    },
    path::{Component, Path},
    ptr::NonNull,
};

use super::{Kind, Metadata};
use crate::Error;

pub(crate) struct Directory(File);
pub(crate) struct Entries(NonNull<libc::DIR>);

impl Directory {
    pub(crate) fn open_root(path: &Path) -> Result<Self, Error> {
        // Anchor each lookup to an already-open directory; NOFOLLOW applies to every component.
        let mut directory = Self(open_at(libc::AT_FDCWD, c"/")?);
        for component in path.components() {
            if let Component::Normal(name) = component {
                directory = Self(open_at(directory.0.as_raw_fd(), &c_name(name)?)?);
            }
        }
        Ok(directory)
    }

    pub(crate) fn metadata(&self) -> Result<Metadata, Error> {
        let metadata = self.0.metadata().map_err(|_| Error::Unavailable)?;
        if !metadata.is_dir() || metadata.ino() == 0 {
            return Err(Error::Invalid);
        }
        Ok(Metadata {
            kind: Kind::Directory,
            size: 0,
            modified: None,
            key: key(metadata.dev(), metadata.ino()),
            volume: metadata.dev(),
        })
    }

    pub(crate) fn child(&self, name: &OsStr, expected: &str) -> Result<Self, Error> {
        let child = Self(open_at(self.0.as_raw_fd(), &c_name(name)?).map_err(|_| Error::Conflict)?);
        if child.metadata()?.key != expected {
            return Err(Error::Conflict);
        }
        Ok(child)
    }

    pub(crate) fn entry(&self, name: &OsStr) -> Result<Metadata, Error> {
        let name = c_name(name)?;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // The kernel initializes stat on success, without following even the final
        // symlink. An entry that vanishes or denies a stat mid-scan is skipped, not
        // fatal: it still consumes the entry budget and shows in the report.
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Ok(Metadata::ignored());
        }
        let metadata = unsafe { metadata.assume_init() };
        let kind = match metadata.st_mode & libc::S_IFMT {
            libc::S_IFREG => Kind::File,
            libc::S_IFDIR => Kind::Directory,
            _ => Kind::Ignore,
        };
        let size = if matches!(kind, Kind::File) {
            u64::try_from(metadata.st_size).map_err(|_| Error::Unavailable)?
        } else {
            0
        };
        Ok(Metadata {
            kind,
            size,
            modified: Some(format!(
                "{}.{:09}",
                metadata.st_mtime, metadata.st_mtime_nsec
            )),
            key: key(metadata.st_dev as u64, metadata.st_ino),
            volume: metadata.st_dev as u64,
        })
    }

    pub(crate) fn entries(&self) -> Result<Entries, Error> {
        // Opening '.' gives the stream its own offset; fdopendir takes ownership on success.
        let file = open_at(self.0.as_raw_fd(), c".")?;
        let stream = unsafe { libc::fdopendir(file.as_raw_fd()) };
        let stream = NonNull::new(stream).ok_or(Error::Unavailable)?;
        std::mem::forget(file);
        Ok(Entries(stream))
    }
}

impl Iterator for Entries {
    type Item = Result<OsString, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // readdir reports end-of-directory as null with errno unchanged.
            unsafe {
                *errno() = 0;
            }
            let entry = unsafe { libc::readdir(self.0.as_ptr()) };
            if entry.is_null() {
                return if io::Error::last_os_error().raw_os_error() == Some(0) {
                    None
                } else {
                    Some(Err(Error::Unavailable))
                };
            }
            // d_name is kernel-provided, NUL-terminated and valid until the next readdir.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                return Some(Ok(OsString::from_vec(name.to_owned())));
            }
        }
    }
}

impl Drop for Entries {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0.as_ptr());
        }
    }
}

fn open_at(parent: std::os::fd::RawFd, name: &CStr) -> Result<File, Error> {
    let flags =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    let descriptor = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(libc::ENOENT) => Error::Missing,
            Some(libc::ELOOP | libc::ENOTDIR) => Error::Invalid,
            _ => Error::Unavailable,
        });
    }
    // This descriptor is newly allocated and is owned exclusively by this File.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn c_name(name: &OsStr) -> Result<CString, Error> {
    CString::new(name.as_bytes()).map_err(|_| Error::Invalid)
}

fn key(volume: u64, inode: u64) -> String {
    format!("unix:{volume:x}:{inode:x}")
}

#[cfg(target_os = "macos")]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__error() }
}
#[cfg(target_os = "linux")]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}
