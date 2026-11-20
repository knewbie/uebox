use super::*;
use std::{collections::HashMap, fs, sync::Mutex};
use uebox_credentials::{CredentialBackend, CredentialError, CredentialRef};

#[derive(Clone, Default)]
struct FixtureVault(Arc<Mutex<HashMap<CredentialRef, Vec<u8>>>>);

impl CredentialBackend for FixtureVault {
    fn set(&self, key: &CredentialRef, value: &Secret) -> Result<(), CredentialError> {
        self.0
            .lock()
            .unwrap()
            .insert(key.clone(), value.expose().to_vec());
        Ok(())
    }

    fn get(&self, key: &CredentialRef) -> Result<Option<Secret>, CredentialError> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .map(Secret::new)
            .transpose()
    }

    fn delete(&self, key: &CredentialRef) -> Result<bool, CredentialError> {
        Ok(self.0.lock().unwrap().remove(key).is_some())
    }

    fn has(&self, key: &CredentialRef) -> Result<bool, CredentialError> {
        Ok(self.0.lock().unwrap().contains_key(key))
    }
}

#[test]
fn secret_stays_in_the_system_store_and_is_bound_to_the_complete_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let backend = FixtureVault::default();
    let store = CredentialStore::with_backend(backend.clone());
    let catalog = Catalog::open(root.path(), true).unwrap().unwrap();
    let change = add_to_catalog(
        &catalog,
        &store,
        "Project tools".into(),
        "https://engine.example.test:443/one/mcp".into(),
        Some("test-private-bearer".into()),
    )
    .unwrap();
    let id = change.endpoints[0].id.clone();
    let url = &change.endpoints[0].url;
    assert_eq!(url, "https://engine.example.test/one/mcp");
    assert_eq!(backend.0.lock().unwrap().len(), 1);
    let response = serde_json::to_string(&change).unwrap();
    assert!(!response.contains("test-private-bearer"));
    assert!(response.contains("\"authenticated\":true"));
    assert_eq!(
        add_to_catalog(&catalog, &store, "duplicate".into(), url.to_owned(), None).err(),
        Some("mcp_endpoint_duplicate")
    );
    let disk = root.path().join("mcp/endpoints.json");
    let original = fs::read_to_string(&disk).unwrap();
    assert!(!original.contains("test-private-bearer"));
    assert!(registered_client(catalog, &store, &id).is_ok());

    fs::write(&disk, original.replace("/one/mcp", "/different/mcp")).unwrap();
    let catalog = Catalog::open(root.path(), false).unwrap().unwrap();
    assert_eq!(
        registered_client(catalog, &store, &id).err(),
        Some("credential_unavailable")
    );

    fs::write(&disk, original).unwrap();
    let catalog = Catalog::open(root.path(), false).unwrap().unwrap();
    let removed = remove_from_catalog(&catalog, &store, &id).unwrap();
    assert!(removed.endpoints.is_empty());
    assert!(!removed.credential_cleanup_pending);
    assert!(backend.0.lock().unwrap().is_empty());
}

#[test]
fn invalid_url_and_bearer_never_stage_a_secret() {
    let root = tempfile::tempdir().unwrap();
    let backend = FixtureVault::default();
    let store = CredentialStore::with_backend(backend.clone());
    let catalog = Catalog::open(root.path(), true).unwrap().unwrap();
    for url in [
        "http://non-loopback.example/mcp",
        "https://user:pass@host/mcp",
        "https://host/mcp?token=p",
    ] {
        assert_eq!(
            add_to_catalog(
                &catalog,
                &store,
                "Fixture".into(),
                url.into(),
                Some("fixture".into())
            )
            .err(),
            Some("mcp_invalid_endpoint")
        );
    }
    assert_eq!(
        add_to_catalog(
            &catalog,
            &store,
            "Fixture".into(),
            "https://host/mcp".into(),
            Some("fixture\n".into())
        )
        .err(),
        Some("credential_invalid_secret")
    );
    assert!(backend.0.lock().unwrap().is_empty());
    assert!(catalog.read().unwrap().is_empty());
}
