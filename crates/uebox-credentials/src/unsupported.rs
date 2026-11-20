use crate::{CredentialBackend, CredentialError, CredentialRef, Secret};

pub(super) struct NativeBackend;

impl CredentialBackend for NativeBackend {
    fn set(&self, _: &CredentialRef, _: &Secret) -> Result<(), CredentialError> {
        Err(CredentialError::Unavailable)
    }

    fn get(&self, _: &CredentialRef) -> Result<Option<Secret>, CredentialError> {
        Err(CredentialError::Unavailable)
    }

    fn delete(&self, _: &CredentialRef) -> Result<bool, CredentialError> {
        Err(CredentialError::Unavailable)
    }

    fn has(&self, _: &CredentialRef) -> Result<bool, CredentialError> {
        Err(CredentialError::Unavailable)
    }
}
