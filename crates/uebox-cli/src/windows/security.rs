//! The Windows security half of the transport: who may open the pipe, and how each end proves the
//! other is the same local user.
//!
//! Everything here is Win32 against the current process's own identity. Two rules hold throughout:
//!
//! * **Nothing is trusted because it was requested.** The descriptor is handed to
//!   `CreateNamedPipe` at creation time — a pipe created with a NULL descriptor inherits the
//!   default security descriptor, which grants read access to Everyone and to the anonymous
//!   account — and is then read back from the object with `GetSecurityInfo`. The verdict is the
//!   read-back, not the request.
//! * **A check that cannot complete is a refusal.** Every failure path returns an error to the
//!   caller that is closing the connection; there is no code path here that proceeds without an
//!   authenticated peer.

use std::{
    ffi::c_void,
    io,
    mem::{size_of, size_of_val},
    os::windows::io::{AsHandle, AsRawHandle, OwnedHandle},
    ptr::{self, addr_of},
    slice,
};

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SetEntriesInAclW, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE,
    SE_KERNEL_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetLengthSid, GetTokenInformation, InitializeSecurityDescriptor,
    RevertToSelf, SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TokenUser,
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, NO_INHERITANCE,
    OWNER_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, SECURITY_EFFECTIVE_ONLY, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

use super::{MAX_INSTANCES, MAX_REQUEST, OUT_BUFFER};
use crate::HostError;

pub(super) const SID_MAX: usize = SECURITY_MAX_SID_SIZE as usize;

/// A process user identity, owned. Every SID the transport compares was copied out of the token or
/// the security descriptor it came from, so no comparison can read memory the kernel has already
/// released, and none of them aliases an impersonated client's buffer.
pub(super) struct Sid(RawSid);

/// A SID copied on the stack. It exists so the impersonated window can capture the client's
/// identity without allocating; the owned [`Sid`] is built after the thread reverted.
#[repr(C, align(4))]
struct RawSid {
    bytes: [u8; SID_MAX],
    len: usize,
}

impl RawSid {
    fn owned(self) -> Sid {
        Sid(self)
    }
}

impl Sid {
    /// The user this process runs as. Both ends compare everything to it: the server checks the
    /// connected client's token against it, the client checks the pipe object's owner and DACL.
    /// Elevation does not change it — a filtered and a full token for one account carry the same
    /// user SID — which is what keeps this check equivalent to the Unix UID comparison.
    pub(super) fn current_user() -> Result<Self, HostError> {
        let mut token = ptr::null_mut();
        // SAFETY: GetCurrentProcess is a pseudo handle that must not be closed, and the token
        // handle filled in here is owned by `Token` from this point on.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if opened == 0 {
            return Err(HostError::Authentication);
        }
        Token(token).user_sid().map(RawSid::owned)
    }

    /// Lowercase hex of the SID, used only as the endpoint's per-account label. It is not a
    /// credential: access is decided by the descriptor, and knowing the name proves nothing.
    pub(super) fn hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(self.0.len * 2);
        for byte in self.0.bytes[..self.0.len].iter() {
            text.push(DIGITS[(byte >> 4) as usize] as char);
            text.push(DIGITS[(byte & 0x0f) as usize] as char);
        }
        text
    }

    fn as_ptr(&self) -> PSID {
        self.0.bytes.as_ptr() as PSID
    }

    fn equals(&self, other: PSID) -> bool {
        !other.is_null() && unsafe { EqualSid(other, self.as_ptr()) } != 0
    }
}

#[cfg(test)]
impl Sid {
    /// A SID from raw bytes, so a test can hold an identity that is not this user's.
    pub(super) fn from_bytes(bytes: [u8; 12]) -> Self {
        let mut raw = RawSid {
            bytes: [0; SID_MAX],
            len: bytes.len(),
        };
        raw.bytes[..raw.len].copy_from_slice(&bytes);
        Self(raw)
    }
}

/// A real token handle. The current-process and current-thread pseudo handles are deliberately
/// never wrapped here, so they can never be closed by accident.
struct Token(HANDLE);

impl Token {
    fn thread() -> Result<Self, HostError> {
        let mut token = ptr::null_mut();
        // SAFETY: GetCurrentThread is a pseudo handle. Check access as the process, since an
        // identification-level token cannot open kernel objects on its own. The returned token
        // is still the thread impersonation token.
        let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
        if opened == 0 {
            return Err(HostError::Authentication);
        }
        Ok(Self(token))
    }

    /// Reads `TokenUser` into a stack buffer: no allocation, no I/O, nothing that can outlive the
    /// window it is called in.
    fn user_sid(&self) -> Result<RawSid, HostError> {
        let mut storage = [0u64; 16];
        let mut used = 0u32;
        // SAFETY: `storage` is 128 writable bytes at 8-alignment, enough for a TOKEN_USER header
        // plus the largest SID the kernel defines, and `used` receives the length written.
        let ok = unsafe {
            GetTokenInformation(
                self.0,
                TokenUser,
                storage.as_mut_ptr().cast(),
                size_of_val(&storage) as u32,
                &mut used,
            )
        };
        if ok == 0 {
            return Err(HostError::Authentication);
        }
        // SAFETY: the kernel wrote a TOKEN_USER whose `Sid` points inside `storage`, which is
        // alive and unmodified here.
        let sid = unsafe { (&*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        if sid.is_null() {
            return Err(HostError::Authentication);
        }
        // SAFETY: GetLengthSid reports the SID living inside the buffer just written.
        let len = unsafe { GetLengthSid(sid) } as usize;
        if len == 0 || len > SID_MAX {
            return Err(HostError::Authentication);
        }
        let mut bytes = [0u8; SID_MAX];
        bytes[..len].copy_from_slice(unsafe { slice::from_raw_parts(sid as *const u8, len) });
        Ok(RawSid { bytes, len })
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: this handle came from OpenProcessToken/OpenThreadToken and is not used again.
        unsafe { CloseHandle(self.0) };
    }
}

/// Own a duplicate of the exact instance while the authentication thread is running.
pub(super) struct InstanceHandle(OwnedHandle);

impl InstanceHandle {
    pub(super) fn of(pipe: &NamedPipeServer) -> Result<Self, HostError> {
        pipe.as_handle()
            .try_clone_to_owned()
            .map(Self)
            .map_err(|_| HostError::Authentication)
    }
}

/// The descriptor for a pipe that only the current user may open: one allow ACE, one trustee, full
/// control, nothing inherited and nothing else.
pub(super) struct PipeSecurity {
    // One allocation holding a SECURITY_DESCRIPTOR followed by the ACL, so the DACL pointer lives
    // inside a buffer whose address cannot move when this struct is moved.
    buffer: Box<[usize]>,
}

impl PipeSecurity {
    pub(super) fn private(owner: &Sid) -> Result<Self, HostError> {
        let acl = allow_only_acl(owner)?;
        let header_size = size_of::<SECURITY_DESCRIPTOR>();
        let dacl_end = header_size + acl.len();
        let sid_offset = dacl_end.next_multiple_of(4);
        let words = (sid_offset + owner.0.len).div_ceil(size_of::<usize>());
        let mut buffer = vec![0usize; words].into_boxed_slice();
        // SAFETY: the allocation is aligned for the descriptor. The DACL and SID are
        // DWORD-aligned and both fit in the allocation with the descriptor.
        unsafe {
            let base = buffer.as_mut_ptr().cast::<u8>();
            let descriptor = base.cast::<c_void>();
            let dacl = base.add(header_size);
            let sid = base.add(sid_offset);
            ptr::copy_nonoverlapping(acl.as_ptr(), dacl, acl.len());
            ptr::copy_nonoverlapping(owner.0.bytes.as_ptr(), sid, owner.0.len);
            if InitializeSecurityDescriptor(descriptor, SECURITY_DESCRIPTOR_REVISION) == 0
                || SetSecurityDescriptorOwner(descriptor, sid.cast(), 0) == 0
                || SetSecurityDescriptorDacl(descriptor, 1, dacl.cast(), 0) == 0
            {
                return Err(HostError::EndpointUnreadable);
            }
        }
        Ok(Self { buffer })
    }

    /// The `lpSecurityAttributes` value for `CreateNamedPipe`. The descriptor it points at is owned
    /// by `self`, so `self` must stay alive across the call — which `create_listener` guarantees by
    /// holding the borrow.
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            // Read-only from here: CreateNamedPipe copies the descriptor and never writes back.
            lpSecurityDescriptor: self.buffer.as_ptr() as *mut c_void,
            bInheritHandle: 0,
        }
    }
}

fn allow_only_acl(owner: &Sid) -> Result<Box<[u8]>, HostError> {
    let explicit = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: owner.as_ptr() as windows_sys::core::PWSTR,
        },
    };
    let mut acl = ptr::null_mut();
    // SAFETY: the trustee borrows the SID owned by `owner`, which outlives this call, and the
    // returned ACL is copied into an owned buffer before it is released.
    let error = unsafe { SetEntriesInAclW(1, &explicit, ptr::null(), &mut acl) };
    if error != ERROR_SUCCESS || acl.is_null() {
        return Err(HostError::EndpointUnreadable);
    }
    // SAFETY: AclSize is the allocated length of the ACL SetEntriesInAclW produced, and LocalFree
    // releases exactly that allocation.
    let bytes = unsafe {
        let copied = slice::from_raw_parts(acl as *const u8, (*acl).AclSize as usize)
            .to_vec()
            .into_boxed_slice();
        LocalFree(acl as HLOCAL);
        copied
    };
    Ok(bytes)
}

/// Creates a listening instance. `first` is true only for the instance that establishes the name:
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` makes a second owner fail instead of letting it inherit or
/// rewrite the descriptor of the one already running, which is the counterpart of the Unix rule
/// that an existing socket is never unlinked.
pub(super) fn create_listener(
    name: &str,
    first: bool,
    security: &PipeSecurity,
) -> io::Result<NamedPipeServer> {
    // The same descriptor bytes are handed to every instance: for a later instance the kernel keeps
    // the first one's, and `verify_listener` reads back which one that actually was.
    let mut attributes = security.attributes();
    let mut options = ServerOptions::new();
    options
        .access_inbound(true)
        .access_outbound(true)
        .pipe_mode(PipeMode::Byte)
        // Remote clients are refused by the kernel, on top of the client refusing to name a
        // remote pipe at all.
        .reject_remote_clients(true)
        .max_instances(MAX_INSTANCES)
        .in_buffer_size(MAX_REQUEST as u32)
        .out_buffer_size(OUT_BUFFER)
        .first_pipe_instance(first);
    // SAFETY: `attributes` and the descriptor and ACL it points into are alive for this
    // synchronous call, which is the only use CreateNamedPipeW makes of them.
    unsafe {
        options.create_with_security_attributes_raw(name, &mut attributes as *mut _ as *mut c_void)
    }
}

/// Opens the pipe as a client.
///
/// The quality of service asks for the **identification** level and nothing more. That is the level
/// the server needs to read the client's user SID, and the only one a server without
/// `SE_IMPERSONATE_PRIVILEGE` may use — a desktop host runs as an ordinary, possibly filtered,
/// account. It also means the server cannot reach any object as this client while impersonated.
pub(super) fn open_client(name: &str) -> io::Result<NamedPipeClient> {
    ClientOptions::new()
        .security_qos_flags(
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | SECURITY_EFFECTIVE_ONLY,
        )
        .open(name)
}

/// The client's proof that it reached the host it meant to reach: the pipe object's owner must be
/// this user, and its DACL must be exactly the one the host asked for. Checked before any frame is
/// written, so a request never leaves for an unverified peer.
pub(super) fn verify_server(pipe: &NamedPipeClient, owner: &Sid) -> Result<(), HostError> {
    verify_descriptor(pipe.as_raw_handle() as HANDLE, owner)
}

/// The server's read-back after each instance is created: the same descriptor check, on the
/// listening half. A later instance inherits the first one's descriptor, so this is also what
/// detects somebody else having taken the name in between.
pub(super) fn verify_listener(pipe: &NamedPipeServer, owner: &Sid) -> Result<(), HostError> {
    verify_descriptor(pipe.as_raw_handle() as HANDLE, owner)
}

fn verify_descriptor(pipe: HANDLE, owner: &Sid) -> Result<(), HostError> {
    let mut stored_owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: *mut c_void = ptr::null_mut();
    // SAFETY: the handle is a live pipe object and every output is a writable slot. The pointers
    // returned point into the descriptor copy, which is released only after they have been read.
    let error = unsafe {
        GetSecurityInfo(
            pipe,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut stored_owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    let result = if error == ERROR_SUCCESS {
        check_descriptor(stored_owner, dacl, owner)
    } else {
        // Not being able to read the descriptor of the object one is about to talk to is a refusal,
        // not a reason to continue without it.
        Err(HostError::Authentication)
    };
    if !descriptor.is_null() {
        // SAFETY: this is the LocalAlloc'd copy GetSecurityInfo returned, and nothing points into
        // it after this point.
        unsafe { LocalFree(descriptor as HLOCAL) };
    }
    result
}

fn check_descriptor(stored_owner: PSID, dacl: *mut ACL, owner: &Sid) -> Result<(), HostError> {
    if !owner.equals(stored_owner) || dacl.is_null() {
        return Err(HostError::Authentication);
    }
    // SAFETY: the ACL pointer is valid for as long as the descriptor copy it came from.
    let header = unsafe { &*dacl };
    if u32::from(header.AclRevision) != ACL_REVISION
        || header.Sbz1 != 0
        || header.AceCount != 1
        || usize::from(header.AclSize) < size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Err(HostError::Authentication);
    }
    let mut entry: *mut c_void = ptr::null_mut();
    // SAFETY: index 0 is within the single ACE the count above requires, and GetAce hands back a
    // pointer into the same ACL.
    if unsafe { GetAce(dacl, 0, &mut entry) } == 0 || entry.is_null() {
        return Err(HostError::Authentication);
    }
    // SAFETY: the type below was just checked, and an ACCESS_ALLOWED_ACE_TYPE entry has the
    // ACCESS_ALLOWED_ACE layout.
    let ace = unsafe { &*entry.cast::<ACCESS_ALLOWED_ACE>() };
    if u32::from(ace.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE
        || ace.Header.AceFlags != 0
        || ace.Mask != FILE_ALL_ACCESS
        // SidStart is the ULONG placeholder the trailing SID begins at, not a value to read.
        || !owner.equals(addr_of!(ace.SidStart) as PSID)
    {
        return Err(HostError::Authentication);
    }
    Ok(())
}

/// Proves the client of the first received frame runs as `owner`.
///
/// The caller runs this on a dedicated thread (its use of `crate::blocking`), so the impersonation
/// window cannot outlive the thread even if something unwinds inside it.
pub(super) fn authenticate_client(pipe: InstanceHandle, owner: &Sid) -> Result<(), HostError> {
    if client_sid(pipe.0.as_raw_handle())?
        .owned()
        .equals(owner.as_ptr())
    {
        Ok(())
    } else {
        Err(HostError::Authentication)
    }
}

fn client_sid(pipe: HANDLE) -> Result<RawSid, HostError> {
    // SAFETY: the handle is a connected pipe server instance. Because the client asked for the
    // identification level, this succeeds without SE_IMPERSONATE_PRIVILEGE and cannot be used to
    // reach the client's objects.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(HostError::Authentication);
    }
    let result = Token::thread().and_then(|token| token.user_sid());
    // Every path reverts before returning: an error is no reason to leave this thread answering to
    // the client's token. A failed revert is reported as a refusal, never downgraded.
    match unsafe { RevertToSelf() } {
        0 => Err(HostError::Authentication),
        _ => result,
    }
}
