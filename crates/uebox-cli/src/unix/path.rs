use std::{
    fs::{self, Metadata},
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use tokio::net::{UnixListener, UnixStream};

use super::SOCKET_NAME;
use crate::HostError;

pub(super) fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments or ownership requirements.
    unsafe { libc::geteuid() }
}

pub(super) fn authenticate(stream: &UnixStream, uid: u32) -> Result<(), HostError> {
    if peer_uid(stream).is_ok_and(|peer| peer == uid) {
        Ok(())
    } else {
        Err(HostError::Authentication)
    }
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;

    let (mut uid, mut gid) = (0, 0);
    // SAFETY: the borrowed descriptor stays open and both outputs are writable uid_t/gid_t values.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    // Tokio obtains these credentials with SO_PEERCRED on Linux.
    stream.peer_cred().map(|peer| peer.uid())
}

pub(super) fn io_error(error: io::Error) -> HostError {
    match error.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => {
            HostError::EndpointUnreadable
        }
        io::ErrorKind::AddrInUse => HostError::EndpointOccupied,
        _ => HostError::Unavailable,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct Identity(u64, u64);

fn identity(meta: &Metadata) -> Identity {
    Identity(meta.dev(), meta.ino())
}

pub(super) struct Endpoint {
    pub(super) socket: PathBuf,
    pub(super) uid: u32,
    directory: PathBuf,
    directory_identity: Identity,
}

impl Endpoint {
    pub(super) fn checked(directory: &Path) -> Result<Self, HostError> {
        let socket = directory.join(SOCKET_NAME);
        if !directory.is_absolute()
            || directory.parent().is_none()
            || directory
                .components()
                .any(|component| component == Component::ParentDir)
            || socket.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(HostError::EndpointUnreadable);
        }
        let uid = effective_uid();
        Ok(Self {
            socket,
            uid,
            directory_identity: validate_directory(directory, uid)?,
            directory: directory.to_owned(),
        })
    }

    pub(super) fn socket_identity(&self) -> Result<Identity, HostError> {
        self.recheck_directory()?;
        let socket = self.socket_metadata()?;
        if socket.mode() & 0o7777 != 0o600 {
            return Err(HostError::EndpointUnreadable);
        }
        Ok(identity(&socket))
    }

    fn recheck_directory(&self) -> Result<(), HostError> {
        if validate_directory(&self.directory, self.uid)? == self.directory_identity {
            Ok(())
        } else {
            Err(HostError::EndpointUnreadable)
        }
    }

    fn socket_metadata(&self) -> Result<Metadata, HostError> {
        let socket = fs::symlink_metadata(&self.socket).map_err(io_error)?;
        if !socket.file_type().is_socket() || socket.uid() != self.uid {
            return Err(HostError::EndpointUnreadable);
        }
        Ok(socket)
    }

    pub(super) fn bind(self) -> Result<(UnixListener, SocketGuard), HostError> {
        match fs::symlink_metadata(&self.socket) {
            Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == self.uid => {
                return Err(HostError::EndpointOccupied);
            }
            Ok(_) => return Err(HostError::EndpointUnreadable),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        // bind never overwrites a pre-existing entry. Do not unlink stale sockets.
        let listener = UnixListener::bind(&self.socket).map_err(io_error)?;
        let identity = identity(&self.socket_metadata()?);
        let guard = SocketGuard {
            endpoint: self,
            identity,
        };
        guard.endpoint.recheck_directory()?;
        fs::set_permissions(&guard.endpoint.socket, fs::Permissions::from_mode(0o600))
            .map_err(io_error)?;
        if guard.endpoint.socket_identity()? != guard.identity {
            return Err(HostError::EndpointUnreadable);
        }
        Ok((listener, guard))
    }
}

fn validate_directory(directory: &Path, uid: u32) -> Result<Identity, HostError> {
    // Inspect ancestors before the leaf, so an intermediate symlink is never followed.
    // A root-owned sticky directory (such as /tmp) cannot replace a user-owned entry.
    let ancestors: Vec<_> = directory.ancestors().collect();
    let mut result = None;
    for component in ancestors.into_iter().rev() {
        let metadata = fs::symlink_metadata(component).map_err(io_error)?;
        let owner = metadata.uid();
        let mode = metadata.mode();
        let root_sticky = owner == 0 && mode & 0o1000 != 0;
        if !metadata.is_dir() || (owner != uid && owner != 0) || (mode & 0o022 != 0 && !root_sticky)
        {
            return Err(HostError::EndpointUnreadable);
        }
        if component == directory {
            if owner != uid || mode & 0o7777 != 0o700 {
                return Err(HostError::EndpointUnreadable);
            }
            result = Some(identity(&metadata));
        }
    }
    result.ok_or(HostError::EndpointUnreadable)
}

pub(super) struct SocketGuard {
    pub(super) endpoint: Endpoint,
    identity: Identity,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.endpoint.recheck_directory().is_ok()
            && self
                .endpoint
                .socket_metadata()
                .is_ok_and(|meta| identity(&meta) == self.identity)
        {
            let _ = fs::remove_file(&self.endpoint.socket);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kernel_peer_uid_must_match_on_either_side() {
        let (first, second) = UnixStream::pair().unwrap();
        let uid = effective_uid();
        for stream in [first, second] {
            assert_eq!(authenticate(&stream, uid), Ok(()));
            assert_eq!(
                authenticate(&stream, uid.wrapping_add(1)),
                Err(HostError::Authentication)
            );
        }
    }
}
