//! Native credential storage for Unreal Box.

use std::{fmt, str::FromStr};
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod native;
#[cfg(windows)]
#[path = "windows.rs"]
mod native;
#[cfg(not(any(target_os = "macos", windows)))]
#[path = "unsupported.rs"]
mod native;

/// Common byte limit, matching Windows `CRED_MAX_CREDENTIAL_BLOB_SIZE`.
pub const MAX_SECRET_BYTES: usize = 2560;

/// Stable namespace used for entries in the native application only.
pub const APPLICATION_NAMESPACE: &str = "com.uebox.tauri-preview.credentials.v1";

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CredentialPurpose {
    ModelBearer,
    McpBearer,
}

impl CredentialPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelBearer => "model-bearer",
            Self::McpBearer => "mcp-bearer",
        }
    }
}

impl FromStr for CredentialPurpose {
    type Err = CredentialError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "model-bearer" => Ok(Self::ModelBearer),
            "mcp-bearer" => Ok(Self::McpBearer),
            _ => Err(CredentialError::InvalidReference),
        }
    }
}

/// A non-secret application-generated identifier and its purpose. Never use a
/// token, endpoint, or path as the identifier because the OS may display it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CredentialRef {
    purpose: CredentialPurpose,
    id: String,
}

impl CredentialRef {
    /// IDs are 1–64 ASCII bytes, beginning with a lowercase letter or digit.
    /// Remaining bytes may also be `-` or `_`. This avoids Windows case aliases.
    pub fn new(purpose: CredentialPurpose, id: &str) -> Result<Self, CredentialError> {
        let mut bytes = id.bytes();
        let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
        if id.len() > 64
            || !bytes.next().is_some_and(alphanumeric)
            || !bytes.all(|byte| alphanumeric(byte) || matches!(byte, b'-' | b'_'))
        {
            return Err(CredentialError::InvalidReference);
        }
        Ok(Self {
            purpose,
            id: id.to_owned(),
        })
    }

    pub fn purpose(&self) -> CredentialPurpose {
        self.purpose
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    #[cfg(any(target_os = "macos", windows, test))]
    fn service(&self) -> String {
        format!("{APPLICATION_NAMESPACE}.{}", self.purpose.as_str())
    }
}

/// Owned secret bytes. The Rust-owned allocation is zeroed on drop. Native APIs
/// and any copies an application makes have their own memory lifetimes.
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Result<Self, CredentialError> {
        let bytes = Zeroizing::new(bytes);
        if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
            return Err(CredentialError::InvalidSecret);
        }
        Ok(Self(bytes))
    }

    pub fn expose(&self) -> &[u8] {
        self.0.as_slice()
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialError {
    InvalidReference,
    InvalidSecret,
    Unavailable,
    AccessDenied,
    BackendFailure,
}

impl CredentialError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidReference => "credential_invalid_reference",
            Self::InvalidSecret => "credential_invalid_secret",
            Self::Unavailable => "credential_unavailable",
            Self::AccessDenied => "credential_access_denied",
            Self::BackendFailure => "credential_backend_failure",
        }
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for CredentialError {}

/// Injection boundary for hosts and tests. Native storage is selected by [`CredentialStore::native`].
pub trait CredentialBackend: Send + Sync {
    fn set(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), CredentialError>;
    fn get(&self, reference: &CredentialRef) -> Result<Option<Secret>, CredentialError>;
    fn delete(&self, reference: &CredentialRef) -> Result<bool, CredentialError>;
    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError>;
}

pub struct CredentialStore {
    backend: Box<dyn CredentialBackend>,
}

impl CredentialStore {
    /// Selects the platform backend without opening the OS store. Operations can
    /// block on the OS and should run outside an async executor's core threads.
    pub fn native() -> Self {
        Self::with_backend(native::NativeBackend)
    }

    pub fn with_backend(backend: impl CredentialBackend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
        }
    }

    /// Creates or replaces this entry. Uses the OS store when constructed with `native()`.
    pub fn set(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), CredentialError> {
        self.backend.set(reference, secret)
    }

    /// `None` indicates that the entry is absent. OS failures propagate as errors.
    pub fn get(&self, reference: &CredentialRef) -> Result<Option<Secret>, CredentialError> {
        self.backend.get(reference)
    }

    /// Returns whether an entry was removed.
    pub fn delete(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        self.backend.delete(reference)
    }

    /// Tests presence without yielding the secret to the caller.
    pub fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        self.backend.has(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_stable_namespace_and_purpose() {
        let purpose = "model-bearer".parse().unwrap();
        let reference = CredentialRef::new(purpose, "fixture-1").unwrap();
        assert_eq!(
            reference.service(),
            "com.uebox.tauri-preview.credentials.v1.model-bearer"
        );
        assert_eq!(
            "unknown".parse::<CredentialPurpose>(),
            Err(CredentialError::InvalidReference)
        );
    }
}
