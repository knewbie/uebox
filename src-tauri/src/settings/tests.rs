use std::{collections::HashMap, sync::Mutex};

use uebox_credentials::{CredentialBackend, CredentialError};

use super::*;

#[derive(Default)]
struct MemoryVault(Mutex<HashMap<CredentialRef, Vec<u8>>>);

impl CredentialBackend for MemoryVault {
    fn set(&self, reference: &CredentialRef, secret: &Secret) -> Result<(), CredentialError> {
        self.0
            .lock()
            .unwrap()
            .insert(reference.clone(), secret.expose().to_vec());
        Ok(())
    }

    fn get(&self, reference: &CredentialRef) -> Result<Option<Secret>, CredentialError> {
        self.0
            .lock()
            .unwrap()
            .get(reference)
            .cloned()
            .map(Secret::new)
            .transpose()
    }

    fn delete(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        Ok(self.0.lock().unwrap().remove(reference).is_some())
    }

    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
        Ok(self.0.lock().unwrap().contains_key(reference))
    }
}

fn update(id: Option<String>, endpoint: &str, authorization: AuthorizationUpdate) -> ProfileUpdate {
    ProfileUpdate {
        id,
        title: "Fixture".into(),
        endpoint: endpoint.into(),
        model: "test-model".into(),
        authorization,
    }
}

#[test]
fn catalog_uses_private_atomic_snapshots_and_never_serializes_the_bearer() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile");
    let settings = CatalogFile::open(&path, true).unwrap().unwrap();
    let vault = CredentialStore::with_backend(MemoryVault::default());
    let saved = save(
        &settings,
        &vault,
        update(
            None,
            "https://one.example/v1/chat/completions",
            AuthorizationUpdate::Keychain {
                bearer: "synthetic-bearer-for-fixture".into(),
            },
        ),
    )
    .unwrap();
    let id = saved.catalog.profiles[0].id.clone();
    let reply = serde_json::to_string(&saved).unwrap();
    assert!(!reply.contains("synthetic-bearer-for-fixture"));
    let body = std::fs::read_to_string(path.join("settings/models.json")).unwrap();
    assert!(!body.contains("synthetic-bearer-for-fixture"));
    let profile = settings.read().unwrap().profiles.pop().unwrap();
    let Authorization::Keychain { reference } = profile.authorization else {
        panic!("expected a reference");
    };
    let reference = credential_ref(&reference).unwrap();
    assert!(vault.has(&reference).unwrap());
    assert_eq!(
        save(
            &settings,
            &vault,
            update(
                Some(id.clone()),
                "https://two.example/v1/chat/completions",
                AuthorizationUpdate::Keep
            )
        )
        .err(),
        Some("credential_reauthorization_required")
    );
    assert!(vault.has(&reference).unwrap());
    assert_eq!(
        std::fs::read_to_string(path.join("settings/models.json")).unwrap(),
        body
    );
    save(
        &settings,
        &vault,
        update(
            Some(id),
            "https://two.example/v1/chat/completions",
            AuthorizationUpdate::None,
        ),
    )
    .unwrap();
    assert!(!vault.has(&reference).unwrap());
    drop(settings);
    let reopened = CatalogFile::open(&path, false).unwrap().unwrap();
    assert_eq!(
        reopened.read().unwrap().profiles[0].endpoint,
        "https://two.example/v1/chat/completions"
    );
}

#[test]
fn invalid_catalog_is_preserved_instead_of_resetting_credentials() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile");
    let settings = CatalogFile::open(&path, true).unwrap().unwrap();
    let vault = CredentialStore::with_backend(MemoryVault::default());
    settings.write(&Catalog::default()).unwrap();
    let target = path.join("settings/models.json");
    std::fs::write(&target, r#"{"version":99,"profiles":[],"bindings":{}}"#).unwrap();
    assert_eq!(
        save(
            &settings,
            &vault,
            update(
                None,
                "https://example.test/v1/chat/completions",
                AuthorizationUpdate::None
            )
        )
        .err(),
        Some("settings_invalid")
    );
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        r#"{"version":99,"profiles":[],"bindings":{}}"#
    );
}

#[cfg(any(feature = "unreal", feature = "unity", feature = "godot"))]
#[tokio::test]
async fn saved_default_loads_only_its_bound_credential_and_revocation_stops_next_request() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        time::Duration,
    };
    use tauri::test::{mock_builder, mock_context, noop_assets};
    use uebox_agent_core::{Cancellation, Error, Message};

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("profile");
    let credentials = Arc::new(CredentialStore::with_backend(MemoryVault::default()));
    let state = ModelSettings {
        credentials: Arc::clone(&credentials),
        test_data_dir: Arc::new(std::sync::OnceLock::from(root.clone())),
        active_credentials: Default::default(),
    };
    let app = mock_builder()
        .manage(state.clone())
        .build(mock_context(noop_assets()))
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let file = CatalogFile::open(&root, true).unwrap().unwrap();
    let saved = save(
        &file,
        &credentials,
        update(
            None,
            &origin,
            AuthorizationUpdate::Keychain {
                bearer: "synthetic-local-fixture".into(),
            },
        ),
    )
    .unwrap();
    let id = saved.catalog.profiles[0].id.clone();
    let mut catalog = file.read().unwrap();
    catalog.bindings.insert(Purpose::Agent, id.clone());
    file.write(&catalog).unwrap();
    drop(file);

    let model = agent_model::resolve_saved_model(app.handle(), None).unwrap();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(&socket);
        let mut auth = false;
        let mut incoming_bytes = 0;
        for line in reader.by_ref().lines() {
            let line = line.unwrap().to_ascii_lowercase();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("authorization: ") {
                auth = value == "bearer synthetic-local-fixture";
            }
            if let Some(value) = line.strip_prefix("content-length: ") {
                incoming_bytes = value.parse::<usize>().unwrap();
            }
        }
        let mut request = vec![0; incoming_bytes];
        reader.read_exact(&mut request).unwrap();
        assert!(
            auth,
            "model request used the synthetic credential from its selected profile"
        );
        assert!(!String::from_utf8(request)
            .unwrap()
            .contains("synthetic-local-fixture"));
        let events = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Fixture answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            events.len(),
            events,
        ).unwrap();
    });
    assert!(model.credential.lock().unwrap().is_some());
    let query = [Message::user("fixture message")];
    let cancel = Cancellation::new();
    let reply = model
        .provider
        .complete(&query, &[], &cancel, |_| {})
        .await
        .unwrap();
    assert_eq!(reply.text, "Fixture answer");
    worker.join().unwrap();
    state.revoke_active_credentials(&id);
    assert!(model.credential.lock().unwrap().is_none());
    assert_eq!(
        model
            .provider
            .complete(&query, &[], &cancel, |_| {})
            .await
            .unwrap_err(),
        Error::CredentialUnavailable
    );
}
