use std::collections::HashMap;
use std::sync::Mutex;
use uebox_credentials::{
    CredentialBackend, CredentialError, CredentialPurpose, CredentialRef, CredentialStore, Secret,
    MAX_SECRET_BYTES,
};

#[derive(Default)]
struct MemoryBackend(Mutex<HashMap<CredentialRef, Secret>>);

impl CredentialBackend for MemoryBackend {
    fn set(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), CredentialError> {
        self.0
            .lock()
            .unwrap()
            .insert(reference.clone(), Secret::new(secret.expose().to_vec())?);
        Ok(())
    }

    fn get(&self, reference: &CredentialRef) -> Result<Option<Secret>, CredentialError> {
        self.0
            .lock()
            .unwrap()
            .get(reference)
            .map(|secret| Secret::new(secret.expose().to_vec()))
            .transpose()
    }

    fn delete(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        Ok(self.0.lock().unwrap().remove(reference).is_some())
    }

    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        Ok(self.0.lock().unwrap().contains_key(reference))
    }
}

#[test]
fn store_overwrites_one_reference_and_distinguishes_absence() {
    let store = CredentialStore::with_backend(MemoryBackend::default());
    let model = CredentialRef::new(CredentialPurpose::ModelBearer, "provider-01").unwrap();
    let other = CredentialRef::new(CredentialPurpose::ModelBearer, "provider-02").unwrap();
    assert_eq!(model.id(), "provider-01");
    assert_eq!(model.purpose(), CredentialPurpose::ModelBearer);
    assert!(!store.has(&model).unwrap());
    assert!(store.get(&model).unwrap().is_none());
    assert!(!store.delete(&model).unwrap());

    store
        .set(&model, &Secret::new(b"fixture first".to_vec()).unwrap())
        .unwrap();
    store
        .set(&other, &Secret::new(b"fixture other".to_vec()).unwrap())
        .unwrap();
    store
        .set(
            &model,
            &Secret::new(b"fixture replacement".to_vec()).unwrap(),
        )
        .unwrap();
    assert!(store.has(&model).unwrap());
    assert_eq!(
        store.get(&model).unwrap().unwrap().expose(),
        b"fixture replacement"
    );
    assert!(store.delete(&model).unwrap());
    assert!(!store.delete(&model).unwrap());
    assert_eq!(
        store.get(&other).unwrap().unwrap().expose(),
        b"fixture other"
    );
}

#[test]
fn references_are_canonical_and_secrets_are_bounded_bytes() {
    for id in [
        "", "A", "-leading", "a/b", "a.b", "a:b", " a", "a\0", "测试",
    ] {
        assert_eq!(
            CredentialRef::new(CredentialPurpose::ModelBearer, id).unwrap_err(),
            CredentialError::InvalidReference
        );
    }
    assert!(CredentialRef::new(CredentialPurpose::ModelBearer, &"a".repeat(64)).is_ok());
    assert!(CredentialRef::new(CredentialPurpose::ModelBearer, &"a".repeat(65)).is_err());
    assert!(Secret::new(vec![0xff; MAX_SECRET_BYTES]).is_ok());
    assert!(matches!(
        Secret::new(Vec::new()),
        Err(CredentialError::InvalidSecret)
    ));
    assert!(matches!(
        Secret::new(vec![b'a'; MAX_SECRET_BYTES + 1]),
        Err(CredentialError::InvalidSecret)
    ));
}

struct UnavailableBackend;

impl CredentialBackend for UnavailableBackend {
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

fn assert_unavailable(store: CredentialStore) {
    let reference = CredentialRef::new(CredentialPurpose::ModelBearer, "fixture").unwrap();
    let secret = Secret::new(b"fixture private bytes".to_vec()).unwrap();
    assert_eq!(
        store.set(&reference, &secret),
        Err(CredentialError::Unavailable)
    );
    assert!(matches!(
        store.get(&reference),
        Err(CredentialError::Unavailable)
    ));
    assert_eq!(store.has(&reference), Err(CredentialError::Unavailable));
    assert_eq!(store.delete(&reference), Err(CredentialError::Unavailable));
    assert_eq!(
        CredentialError::Unavailable.code(),
        "credential_unavailable"
    );
    assert_eq!(
        CredentialError::Unavailable.to_string(),
        "credential_unavailable"
    );
}

#[test]
fn backend_failure_is_not_converted_to_absence_or_a_fallback() {
    assert_unavailable(CredentialStore::with_backend(UnavailableBackend));
}

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn unsupported_platform_has_no_native_backend() {
    assert_unavailable(CredentialStore::native());
}
