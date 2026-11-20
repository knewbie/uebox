use std::{ptr, ptr::NonNull, slice};

use windows_sys::Win32::{
    Foundation::{
        GetLastError, ERROR_ACCESS_DENIED, ERROR_CANCELLED, ERROR_NOT_FOUND, ERROR_NOT_SUPPORTED,
        ERROR_NO_SUCH_LOGON_SESSION, ERROR_SERVICE_NOT_ACTIVE, WIN32_ERROR,
    },
    Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE,
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    },
};
use zeroize::{Zeroize, Zeroizing};

use crate::{CredentialBackend, CredentialError, CredentialRef, Secret, MAX_SECRET_BYTES};

const _: () = assert!(MAX_SECRET_BYTES == CRED_MAX_CREDENTIAL_BLOB_SIZE as usize);

pub(super) struct NativeBackend;

fn target(reference: &CredentialRef) -> Vec<u16> {
    format!("{}/{}", reference.service(), reference.id())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn error(status: WIN32_ERROR) -> CredentialError {
    match status {
        ERROR_ACCESS_DENIED | ERROR_CANCELLED => CredentialError::AccessDenied,
        ERROR_NO_SUCH_LOGON_SESSION | ERROR_NOT_SUPPORTED | ERROR_SERVICE_NOT_ACTIVE => {
            CredentialError::Unavailable
        }
        _ => CredentialError::BackendFailure,
    }
}

fn failed_read_or_delete(status: WIN32_ERROR) -> Result<bool, CredentialError> {
    match status {
        ERROR_NOT_FOUND => Ok(false),
        _ => Err(error(status)),
    }
}

/// Owns the allocation returned by CredReadW and releases it through CredFree.
struct Credential(NonNull<CREDENTIALW>);

impl Credential {
    fn read(reference: &CredentialRef) -> Result<Option<Self>, CredentialError> {
        let target = target(reference);
        let mut result = ptr::null_mut();
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut result) } == 0 {
            return failed_read_or_delete(unsafe { GetLastError() }).map(|_| None);
        }
        NonNull::new(result)
            .map(|result| Some(Self(result)))
            .ok_or(CredentialError::BackendFailure)
    }

    fn blob(&self) -> Option<(*mut u8, usize)> {
        // SAFETY: the successful CredReadW allocation is alive while self is borrowed.
        let credential = unsafe { self.0.as_ref() };
        let size = credential.CredentialBlobSize as usize;
        let blob = credential.CredentialBlob;
        ((1..=MAX_SECRET_BYTES).contains(&size) && !blob.is_null()).then_some((blob, size))
    }

    fn secret(&self) -> Result<Secret, CredentialError> {
        let (blob, size) = self.blob().ok_or(CredentialError::BackendFailure)?;
        // SAFETY: CredReadW owns a buffer with the documented CredentialBlobSize.
        let bytes = unsafe { slice::from_raw_parts(blob, size) };
        Secret::new(bytes.to_vec()).map_err(|_| CredentialError::BackendFailure)
    }
}

impl Drop for Credential {
    fn drop(&mut self) {
        if let Some((blob, size)) = self.blob() {
            // SAFETY: this mutable system allocation has no outstanding Rust borrows.
            unsafe { slice::from_raw_parts_mut(blob, size) }.zeroize();
        }
        // SAFETY: the sole owner must release the entire CredReadW allocation once.
        unsafe { CredFree(self.0.as_ptr().cast()) };
    }
}

impl CredentialBackend for NativeBackend {
    fn set(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), CredentialError> {
        let mut target = target(reference);
        let mut blob = Zeroizing::new(secret.expose().to_vec());
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..CREDENTIALW::default()
        };
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(error(unsafe { GetLastError() }));
        }
        Ok(())
    }

    fn get(&self, reference: &CredentialRef) -> Result<Option<Secret>, CredentialError> {
        Credential::read(reference)?
            .map(|credential| credential.secret())
            .transpose()
    }

    fn delete(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        let target = target(reference);
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            return failed_read_or_delete(unsafe { GetLastError() });
        }
        Ok(true)
    }

    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        Ok(Credential::read(reference)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CredentialPurpose;

    #[test]
    fn builds_a_namespaced_target_and_maps_errors_without_an_os_call() {
        let reference = CredentialRef::new(CredentialPurpose::ModelBearer, "fixture-1").unwrap();
        let target = target(&reference);
        assert_eq!(target.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&target[..target.len() - 1]).unwrap(),
            "com.uebox.tauri-preview.credentials.v1.model-bearer/fixture-1"
        );
        assert_eq!(failed_read_or_delete(ERROR_NOT_FOUND), Ok(false));
        assert_eq!(
            failed_read_or_delete(ERROR_NO_SUCH_LOGON_SESSION),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(
            failed_read_or_delete(ERROR_ACCESS_DENIED),
            Err(CredentialError::AccessDenied)
        );
    }
}
