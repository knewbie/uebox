use std::{
    fs,
    path::{Path, PathBuf},
};
use uebox_asset_index::{Archive, Asset, AssetIndex, Error, LocalVault, ScanLimits};

struct Fixture {
    _temporary: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    database: PathBuf,
    index: AssetIndex,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temporary.path()).unwrap();
        let root = base.join("library");
        fs::create_dir(&root).unwrap();
        let database = base.join("index.sqlite");
        let index = AssetIndex::create(&database).unwrap();
        Self {
            _temporary: temporary,
            base,
            root,
            database,
            index,
        }
    }

    fn register(&mut self) -> LocalVault {
        self.index.register_local("Fixture", &self.root).unwrap()
    }

    fn assets(&self, vault: &str) -> Vec<Asset> {
        self.index.assets(vault, None, None, 100, Some("")).unwrap()
    }
}

#[test]
fn registration_does_not_scan_and_rescan_keeps_path_ids_across_metadata_changes_and_restart() {
    let mut fixture = Fixture::new();
    fs::create_dir(fixture.root.join("Models")).unwrap();
    fs::write(fixture.root.join("Models/Oak.uasset"), b"metadata only").unwrap();
    let vault = fixture.register();
    assert!(vault.online);
    assert!(!vault.index_available);
    assert!(fixture.assets(&vault.identity).is_empty());
    let duplicate = fixture
        .index
        .register_local("Another name", &fixture.root)
        .unwrap();
    assert_eq!(duplicate.identity, vault.identity);
    assert_eq!(fixture.index.local_vaults(None, 100).unwrap().len(), 1);
    let scanned = fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    assert_eq!((scanned.folders, scanned.assets), (1, 1));
    assert!(!scanned.offline);
    let asset = fixture.assets(&vault.identity).remove(0);
    assert_eq!(asset.relative_path.as_deref(), Some("Models/Oak.uasset"));
    assert_eq!(asset.extension.as_deref(), Some("uasset"));
    assert_eq!(asset.size, Some(13));
    assert!(asset.modified_time.is_some());
    assert!(asset.tags.is_empty());
    let folder = fixture
        .index
        .folders(&vault.identity, None, None, 100)
        .unwrap()
        .remove(0);
    assert_eq!(
        asset.folder_identity.as_deref(),
        Some(folder.identity.as_str())
    );
    fs::write(fixture.root.join("Models/Oak.uasset"), b"changed").unwrap();
    fs::write(fixture.root.join("root.txt"), b"new").unwrap();
    fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    let changed = fixture
        .assets(&vault.identity)
        .into_iter()
        .find(|a| a.name == "Oak.uasset")
        .unwrap();
    assert_eq!(changed.identity, asset.identity);
    assert_eq!(changed.size, Some(7));
    let reopened = AssetIndex::open_existing(&fixture.database).unwrap();
    let registered = reopened.local_vaults(None, 100).unwrap().remove(0);
    assert!(registered.index_available && registered.online);
    assert_eq!(registered.root_path, fixture.root.to_str().unwrap());
    assert_eq!(
        reopened.folders(&vault.identity, None, None, 100).unwrap()[0].identity,
        folder.identity
    );
}

#[test]
fn stable_ids_do_not_depend_on_enumeration_order_or_the_private_index_file() {
    let mut fixture = Fixture::new();
    fs::write(fixture.root.join("b.txt"), b"b").unwrap();
    fs::write(fixture.root.join("a.txt"), b"a").unwrap();
    let first = fixture.register();
    fixture
        .index
        .rescan_local(&first.identity, ScanLimits::default())
        .unwrap();
    let mut other = AssetIndex::create(&fixture.base.join("another.sqlite")).unwrap();
    let registered = other.register_local("Same root", &fixture.root).unwrap();
    other
        .rescan_local(&registered.identity, ScanLimits::default())
        .unwrap();
    assert_eq!(registered.identity, first.identity);
    let pairs = |assets: Vec<Asset>| {
        assets
            .into_iter()
            .map(|a| (a.identity, a.relative_path))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pairs(fixture.assets(&first.identity)),
        pairs(
            other
                .assets(&first.identity, None, None, 100, Some(""))
                .unwrap()
        )
    );
    let another_root = fixture.base.join("other-root");
    fs::create_dir(&another_root).unwrap();
    fs::write(another_root.join("a.txt"), b"a").unwrap();
    let second = fixture
        .index
        .register_local("Other", &another_root)
        .unwrap();
    fixture
        .index
        .rescan_local(&second.identity, ScanLimits::default())
        .unwrap();
    assert_ne!(second.identity, first.identity);
    assert!(!fixture
        .assets(&first.identity)
        .iter()
        .any(|a| a.identity == fixture.assets(&second.identity)[0].identity));
}

#[test]
fn missing_roots_retain_the_snapshot_and_replacement_roots_cannot_rebind_a_registration() {
    let mut fixture = Fixture::new();
    fs::write(fixture.root.join("keep.txt"), b"keep").unwrap();
    let vault = fixture.register();
    fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    let original = fixture.assets(&vault.identity).remove(0);
    let parked = fixture.base.join("parked");
    fs::rename(&fixture.root, &parked).unwrap();
    let report = fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    assert!(report.offline && report.retained_index);
    assert_eq!(report.assets, 1);
    let offline = fixture.index.local_vaults(None, 100).unwrap().remove(0);
    assert!(!offline.online && offline.index_available);
    assert_eq!(
        fixture.assets(&vault.identity)[0].identity,
        original.identity
    );
    fs::create_dir(&fixture.root).unwrap();
    fs::write(fixture.root.join("foreign.txt"), b"do not index").unwrap();
    assert!(matches!(
        fixture
            .index
            .rescan_local(&vault.identity, ScanLimits::default()),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        fixture.index.register_local("Replacement", &fixture.root),
        Err(Error::Conflict)
    ));
    assert_eq!(fixture.assets(&vault.identity)[0].name, "keep.txt");
    fs::rename(&fixture.root, fixture.base.join("replacement")).unwrap();
    fs::rename(&parked, &fixture.root).unwrap();
    fs::remove_file(fixture.root.join("keep.txt")).unwrap();
    let report = fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    assert!(!report.offline);
    assert_eq!(report.assets, 0);
    assert!(fixture.assets(&vault.identity).is_empty());
}

#[test]
fn every_scan_limit_fails_without_replacing_the_previous_complete_index() {
    let mut fixture = Fixture::new();
    fs::write(fixture.root.join("keep.txt"), b"original").unwrap();
    let vault = fixture.register();
    fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    fs::create_dir(fixture.root.join("nested")).unwrap();
    fs::write(fixture.root.join("nested/new.txt"), b"new").unwrap();
    let default = ScanLimits::default();
    for limits in [
        ScanLimits {
            max_depth: 0,
            ..default
        },
        ScanLimits {
            max_assets: 1,
            ..default
        },
        ScanLimits {
            max_folders: 0,
            ..default
        },
        ScanLimits {
            max_entries: 1,
            ..default
        },
        ScanLimits {
            max_metadata_bytes: 1,
            ..default
        },
        ScanLimits {
            max_file_bytes: 1,
            ..default
        },
    ] {
        assert!(matches!(
            fixture.index.rescan_local(&vault.identity, limits),
            Err(Error::Limit)
        ));
        assert_eq!(fixture.assets(&vault.identity).len(), 1);
        assert_eq!(fixture.assets(&vault.identity)[0].name, "keep.txt");
        assert!(fixture
            .index
            .folders(&vault.identity, None, None, 100)
            .unwrap()
            .is_empty());
    }
    assert!(matches!(
        fixture.index.rescan_local(
            &vault.identity,
            ScanLimits {
                max_assets: 32769,
                ..default
            }
        ),
        Err(Error::Invalid)
    ));
}

#[test]
fn sqlite_insert_failure_rolls_back_deletions_and_registration_status() {
    let mut fixture = Fixture::new();
    fs::write(fixture.root.join("keep.txt"), b"old").unwrap();
    let vault = fixture.register();
    fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    let before = fixture.index.local_vaults(None, 100).unwrap().remove(0);
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    connection.execute_batch("CREATE TRIGGER synthetic_scan_failure BEFORE INSERT ON assets WHEN NEW.name = 'explode.txt' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    drop(connection);
    fs::remove_file(fixture.root.join("keep.txt")).unwrap();
    fs::write(fixture.root.join("explode.txt"), b"new").unwrap();
    assert!(matches!(
        fixture
            .index
            .rescan_local(&vault.identity, ScanLimits::default()),
        Err(Error::Unavailable)
    ));
    let after = fixture.index.local_vaults(None, 100).unwrap().remove(0);
    assert_eq!(after.revision, before.revision);
    assert!(after.online && after.index_available);
    assert_eq!(fixture.assets(&vault.identity)[0].name, "keep.txt");
}

#[test]
fn legacy_import_cannot_overwrite_a_pinned_local_registration() {
    let mut fixture = Fixture::new();
    let vault = fixture.register();
    let archive = serde_json::json!({
        "format":"uebox-local-asset-index", "version":1, "source":"electron-local-vault",
        "vault":{"identity":vault.identity,"name":"Other","rootPath":"/other","indexAvailable":false},
        "folders":[], "assets":[]
    });
    assert!(matches!(
        fixture
            .index
            .import(Archive::parse(&archive.to_string()).unwrap()),
        Err(Error::Conflict)
    ));
    assert_eq!(
        fixture.index.local_vaults(None, 100).unwrap()[0].root_path,
        fixture.root.to_str().unwrap()
    );
}

#[test]
fn version_one_indexes_upgrade_without_losing_imported_metadata() {
    let mut fixture = Fixture::new();
    let archive = serde_json::json!({
        "format":"uebox-local-asset-index", "version":1, "source":"electron-local-vault",
        "vault":{"identity":"legacy","name":"Offline","rootPath":"/offline","indexAvailable":false},
        "folders":[], "assets":[]
    });
    fixture
        .index
        .import(Archive::parse(&archive.to_string()).unwrap())
        .unwrap();
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    connection
        .execute_batch("DROP TABLE local_vaults; PRAGMA user_version=1;")
        .unwrap();
    drop(connection);
    let mut reopened = AssetIndex::open_existing(&fixture.database).unwrap();
    assert_eq!(reopened.vaults(None, 100).unwrap()[0].identity, "legacy");
    let local = reopened.register_local("Local", &fixture.root).unwrap();
    reopened
        .rescan_local(&local.identity, ScanLimits::default())
        .unwrap();
    assert_eq!(reopened.vaults(None, 100).unwrap().len(), 2);
}

#[test]
fn invalid_roots_and_unknown_registrations_are_rejected_before_scanning() {
    let mut fixture = Fixture::new();
    for root in [
        Path::new("relative"),
        Path::new("//fixture.invalid/share"),
        Path::new(r"\\fixture.invalid\share"),
        Path::new(r"\\?\C:\fixture"),
    ] {
        assert!(matches!(
            fixture.index.register_local("Invalid", root),
            Err(Error::Invalid)
        ));
    }
    assert!(matches!(
        fixture.index.register_local("", &fixture.root),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        fixture.index.register_local("Bad\u{0085}", &fixture.root),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        fixture.index.rescan_local("unknown", ScanLimits::default()),
        Err(Error::Missing)
    ));
    assert!(fixture.index.vaults(None, 100).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_special_entries_and_unsafe_names_are_skipped_without_reading_file_contents() {
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{symlink, PermissionsExt},
    };
    let mut fixture = Fixture::new();
    let outside = fixture.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"outside").unwrap();
    let sealed = fixture.root.join("sealed.txt");
    fs::write(&sealed, b"metadata only").unwrap();
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o0)).unwrap();
    symlink(outside.join("secret.txt"), fixture.root.join("file-link")).unwrap();
    symlink(&outside, fixture.root.join("dir-link")).unwrap();
    symlink(&fixture.root, fixture.root.join("loop")).unwrap();
    fs::write(fixture.root.join("bad:name"), b"skip").unwrap();
    let fifo = std::ffi::CString::new(fixture.root.join("pipe").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let alias = fixture.base.join("alias");
    symlink(&fixture.root, &alias).unwrap();
    assert!(matches!(
        fixture.index.register_local("Linked root", &alias),
        Err(Error::Invalid)
    ));
    let vault = fixture.register();
    let report = fixture
        .index
        .rescan_local(&vault.identity, ScanLimits::default())
        .unwrap();
    assert_eq!((report.assets, report.folders, report.skipped), (1, 0, 5));
    let asset = fixture.assets(&vault.identity).remove(0);
    assert_eq!(asset.name, "sealed.txt");
    assert_eq!(asset.size, Some(13));
    assert_eq!(
        fs::metadata(&sealed).unwrap().permissions().mode() & 0o777,
        0
    );
}
