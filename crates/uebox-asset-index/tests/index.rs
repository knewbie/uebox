use serde_json::{json, Value};
use uebox_asset_index::{Archive, AssetIndex, Error};

fn source(id: &str, name: &str, root: &str, indexed: bool) -> Value {
    let (folders, assets) = if indexed {
        (
            json!([
                {"identity":"trees","parentIdentity":null,"name":"Trees"},
                {"identity":"models","parentIdentity":"trees","name":"Models"}
            ]),
            json!([
                {"identity":"item","folderIdentity":"models","name":"Oak",
                 "relativePath":"Scanned/tall.uasset","size":123,"extension":"uasset",
                 "modifiedTime":null,"tags":["fixture"]},
                {"identity":"project","folderIdentity":null,"name":"Other",
                 "relativePath":null,"size":null,"extension":null,"modifiedTime":null,"tags":[]}
            ]),
        )
    } else {
        (json!([]), json!([]))
    };
    json!({
        "format":"uebox-local-asset-index", "version":1, "source":"electron-local-vault",
        "vault":{"identity":id,"name":name,"rootPath":root,"indexAvailable":indexed},
        "folders":folders, "assets":assets
    })
}

#[test]
fn imports_are_isolated_per_vault_and_offline_metadata_preserves_prior_index() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("assets.sqlite");
    let mut store = AssetIndex::create(&path).unwrap();
    let import_a = source("a", "Shared", "/mnt/absent/dataset/a", true);
    let import_b = source("b", "Shared", r"C:\Workspace\Other", true);
    for archive in [&import_a, &import_b, &import_a] {
        let report = store
            .import(Archive::parse(&archive.to_string()).unwrap())
            .unwrap();
        assert_eq!(report.assets, 2);
        assert_eq!(report.folders, 2);
    }
    assert_eq!(store.vaults(None, 1).unwrap()[0].identity, "a");
    assert_eq!(store.vaults(Some("a"), 1).unwrap()[0].identity, "b");
    assert_eq!(store.folders("a", None, None, 20).unwrap()[0].name, "Trees");
    assert_eq!(
        store.folders("a", Some("trees"), None, 20).unwrap()[0].name,
        "Models"
    );
    assert_eq!(
        store.assets("b", None, None, 20, None).unwrap()[0].relative_path,
        None
    );

    let offline = source("a", "New name", "/mnt/absent/dataset/a", false);
    assert!(
        store
            .import(Archive::parse(&offline.to_string()).unwrap())
            .unwrap()
            .retained_index
    );
    let bad_root = source("a", "Wrong name", "/other-root", false);
    assert!(matches!(
        store.import(Archive::parse(&bad_root.to_string()).unwrap()),
        Err(Error::Conflict)
    ));

    drop(store);
    let mut restarted = AssetIndex::open_existing(&path).unwrap();
    let first = restarted.vaults(None, 1).unwrap().remove(0);
    assert_eq!(first.name, "New name");
    assert_eq!(first.root_path, "/mnt/absent/dataset/a");
    let nested = restarted
        .assets("a", Some("models"), None, 20, None)
        .unwrap();
    assert_eq!(
        nested[0].relative_path.as_deref(),
        Some("Scanned/tall.uasset")
    );
    assert_eq!(nested[0].tags, ["fixture"]);
    let search = restarted.assets("a", None, None, 20, Some("oak")).unwrap();
    assert_eq!(search[0].identity, "item");

    let registration = source("c", "Offline", "/mnt/disconnected", false);
    assert!(
        !restarted
            .import(Archive::parse(&registration.to_string()).unwrap())
            .unwrap()
            .retained_index
    );
    assert!(!restarted.vaults(Some("b"), 20).unwrap()[0].index_available);
}

#[test]
fn archive_rejects_cross_vault_fields_and_retains_unrecognized_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("assets.sqlite");
    let unexpected = rusqlite::Connection::open(&path).unwrap();
    unexpected
        .execute_batch("CREATE TABLE keep_me(fixture TEXT)")
        .unwrap();
    drop(unexpected);
    let original = std::fs::read(&path).unwrap();
    assert!(matches!(
        AssetIndex::open_existing(&path),
        Err(Error::Unavailable)
    ));
    assert!(AssetIndex::create(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let mut invalid = source("a", "Fixture", "/mnt/unknown", true);
    invalid["assets"][0]["relativePath"] = json!("../outside.secret");
    assert!(matches!(
        Archive::parse(&invalid.to_string()),
        Err(Error::Invalid)
    ));
    invalid["assets"][0]["relativePath"] = Value::Null;
    invalid["assets"][0]["folderIdentity"] = json!("another-vault");
    assert!(matches!(
        Archive::parse(&invalid.to_string()),
        Err(Error::Invalid)
    ));
}
