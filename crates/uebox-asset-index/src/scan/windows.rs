//! Windows directory access: reparse points are opened without following, and every
//! entry is pinned to the root volume. All handles use read access plus full sharing.
//! Declared directly against the documented Win32 ABI instead of adding a dependency.
//! NOTE: this module has not been compile-checked on a Windows toolchain.
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
    },
    path::PathBuf,
    ptr,
};

use super::{Kind, Metadata};
use crate::Error;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
const FIND_FIRST_EX_BASIC: u32 = 1;
const FIND_SEARCH_NAME_MATCH: u32 = 0;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_NO_MORE_FILES: u32 = 18;
const INVALID_HANDLE_VALUE: isize = -1;

type Handle = *mut core::ffi::c_void;

#[repr(C)]
struct Filetime {
    low: u32,
    high: u32,
}

#[repr(C)]
struct ByHandleFileInformation {
    attributes: u32,
    creation: Filetime,
    last_access: Filetime,
    last_write: Filetime,
    volume_serial: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[repr(C)]
struct FindData {
    attributes: u32,
    creation: Filetime,
    last_access: Filetime,
    last_write: Filetime,
    file_size_high: u32,
    file_size_low: u32,
    reserved: [u32; 2],
    file_name: [u16; 260],
    alternate_file_name: [u16; 14],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(file: Handle, information: *mut ByHandleFileInformation) -> i32;
    fn GetLastError() -> u32;
    fn FindFirstFileExW(
        name: *const u16,
        level: u32,
        data: *mut FindData,
        search: u32,
        filter: *mut core::ffi::c_void,
        flags: u32,
    ) -> Handle;
    fn FindNextFileW(search: Handle, data: *mut FindData) -> i32;
    fn FindClose(search: Handle) -> i32;
}

pub(crate) struct Directory {
    file: File,
    path: PathBuf,
}

pub(crate) struct Entries {
    search: Handle,
    pending: Option<OsString>,
}

impl Directory {
    pub(crate) fn open_root(path: &std::path::Path) -> Result<Self, Error> {
        let directory = Self {
            file: open(path)?,
            path: path.to_owned(),
        };
        directory.metadata()?;
        Ok(directory)
    }

    pub(crate) fn metadata(&self) -> Result<Metadata, Error> {
        let information = information(&self.file)?;
        if information.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || information.attributes & FILE_ATTRIBUTE_DIRECTORY == 0
        {
            return Err(Error::Invalid);
        }
        let (volume, index) = identity(&information);
        Ok(Metadata {
            kind: Kind::Directory,
            size: 0,
            modified: None,
            key: key(volume, index),
            volume,
        })
    }

    pub(crate) fn child(&self, name: &OsStr, expected: &str) -> Result<Self, Error> {
        let child = Self {
            file: open(&self.path.join(name))?,
            path: self.path.join(name),
        };
        if child.metadata()?.key != expected {
            return Err(Error::Conflict);
        }
        Ok(child)
    }

    pub(crate) fn entry(&self, name: &OsStr) -> Result<Metadata, Error> {
        let Ok(file) = open(&self.path.join(name)) else {
            return Ok(Metadata::ignored());
        };
        let Ok(information) = information(&file) else {
            return Ok(Metadata::ignored());
        };
        if information.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(Metadata::ignored());
        }
        let kind = if information.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            Kind::Directory
        } else {
            Kind::File
        };
        let size =
            (u64::from(information.file_size_high) << 32) | u64::from(information.file_size_low);
        let (volume, index) = identity(&information);
        Ok(Metadata {
            kind,
            size,
            modified: modified(&information.last_write),
            key: key(volume, index),
            volume,
        })
    }

    pub(crate) fn entries(&self) -> Result<Entries, Error> {
        let mut pattern = self.path.as_os_str().encode_wide().collect::<Vec<_>>();
        pattern.extend([b'\\' as u16, b'*' as u16, 0]);
        let mut data = FindData::default();
        let search = unsafe {
            FindFirstFileExW(
                pattern.as_ptr(),
                FIND_FIRST_EX_BASIC,
                &mut data,
                FIND_SEARCH_NAME_MATCH,
                ptr::null_mut(),
                0,
            )
        };
        if search == INVALID_HANDLE_VALUE as Handle {
            return Err(map_error());
        }
        Ok(Entries {
            search,
            pending: first_name(&data),
        })
    }
}

impl Iterator for Entries {
    type Item = Result<OsString, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        let name = self.pending.take()?;
        let mut data = FindData::default();
        loop {
            if unsafe { FindNextFileW(self.search, &mut data) } == 0 {
                let error = unsafe { GetLastError() };
                return if error == ERROR_NO_MORE_FILES {
                    None
                } else {
                    Some(Err(map_error()))
                };
            }
            if let Some(next) = first_name(&data) {
                self.pending = Some(next);
                break;
            }
        }
        Some(Ok(name))
    }
}

impl Drop for Entries {
    fn drop(&mut self) {
        unsafe { FindClose(self.search) };
    }
}

fn open(path: &std::path::Path) -> Result<File, Error> {
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(code)
                if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PATH_NOT_FOUND as i32 =>
            {
                Error::Missing
            }
            _ => Error::Unavailable,
        })
}

fn information(file: &File) -> Result<ByHandleFileInformation, Error> {
    let mut information = ByHandleFileInformation::default();
    let handle = std::os::windows::io::AsRawHandle::as_raw_handle(file) as Handle;
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(Error::Unavailable);
    }
    Ok(information)
}

fn identity(information: &ByHandleFileInformation) -> (u64, u64) {
    let index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    (u64::from(information.volume_serial), index)
}

fn key(volume: u64, index: u64) -> String {
    format!("win:{volume:x}:{index:x}")
}

fn modified(time: &Filetime) -> Option<String> {
    let ticks = (u64::from(time.high) << 32) | u64::from(time.low);
    if ticks == 0 {
        return None;
    }
    let epoch = 116_444_736_000_000_000u64;
    let since = ticks.checked_sub(epoch).unwrap_or(0);
    let seconds = since / 10_000_000;
    let nanos = (since % 10_000_000) * 100;
    Some(format!("{seconds}.{nanos:09}"))
}

fn first_name(data: &FindData) -> Option<OsString> {
    let end = data
        .file_name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(data.file_name.len());
    if end == 0 {
        return None;
    }
    let name = OsString::from_wide(&data.file_name[..end]);
    if name == OsStr::new(".") || name == OsStr::new("..") {
        None
    } else {
        Some(name)
    }
}

fn map_error() -> Error {
    match io::Error::last_os_error().raw_os_error() {
        Some(code)
            if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PATH_NOT_FOUND as i32 =>
        {
            Error::Missing
        }
        _ => Error::Unavailable,
    }
}

impl Default for FindData {
    fn default() -> Self {
        Self {
            attributes: 0,
            creation: Filetime { low: 0, high: 0 },
            last_access: Filetime { low: 0, high: 0 },
            last_write: Filetime { low: 0, high: 0 },
            file_size_high: 0,
            file_size_low: 0,
            reserved: [0; 2],
            file_name: [0; 260],
            alternate_file_name: [0; 14],
        }
    }
}

impl Default for ByHandleFileInformation {
    fn default() -> Self {
        Self {
            attributes: 0,
            creation: Filetime { low: 0, high: 0 },
            last_access: Filetime { low: 0, high: 0 },
            last_write: Filetime { low: 0, high: 0 },
            volume_serial: 0,
            file_size_high: 0,
            file_size_low: 0,
            number_of_links: 0,
            file_index_high: 0,
            file_index_low: 0,
        }
    }
}
