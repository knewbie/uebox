use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions, TryLockError},
    io::ErrorKind,
    path::{Path, PathBuf},
};

const INVALID: &str = "project_library_invalid";
const UNAVAILABLE: &str = "project_library_unavailable";

#[derive(Debug)]
pub(super) struct LockedStorage {
    database: PathBuf,
    _lock: File,
}

impl LockedStorage {
    pub(super) fn database(&self) -> &Path {
        &self.database
    }
}

/// Use only the trusted app profile resolved by the Rust host. The lock
/// coordinates cooperating hosts; path checks cannot prevent replacement by
/// unrelated software with write access to the profile or its ancestors.
/// Windows inherits LocalAppData ACLs. This helper does not inspect those ACLs
/// or create a security boundary against other processes running as the user.
pub(super) fn lock_storage(
    app_local_data: &Path,
    create: bool,
) -> Result<Option<LockedStorage>, &'static str> {
    if !app_local_data.is_absolute() {
        return Err(INVALID);
    }
    if !ensure_directory(app_local_data, create, 0o022)? {
        return Ok(None);
    }
    let directory = app_local_data.join("project-library");
    if !ensure_directory(&directory, create, 0o077)? {
        return Ok(None);
    }

    let lock = acquire_lock(&directory.join("library.lock"))?;
    let database = directory.join("library.sqlite");
    let has_database = checked_file_exists(&database)?;
    if checked_file_exists(&directory.join("library.sqlite-wal"))?
        || checked_file_exists(&directory.join("library.sqlite-shm"))?
    {
        return Err(INVALID);
    }
    if checked_file_exists(&directory.join("library.sqlite-journal"))? {
        return Err(if has_database {
            "project_library_recovery_required"
        } else {
            INVALID
        });
    }
    Ok(Some(LockedStorage {
        database,
        _lock: lock,
    }))
}

fn metadata_at(path: &Path) -> Result<Option<Metadata>, &'static str> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(UNAVAILABLE),
    }
}

fn ensure_directory(path: &Path, create: bool, forbidden_mode: u32) -> Result<bool, &'static str> {
    let metadata = match metadata_at(path)? {
        Some(metadata) => metadata,
        None if !create => return Ok(false),
        None => {
            let mut builder = DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|_| UNAVAILABLE)?;
            metadata_at(path)?.ok_or(UNAVAILABLE)?
        }
    };
    if !metadata.is_dir() || is_link(&metadata) || !mode_allows(&metadata, forbidden_mode) {
        return Err(INVALID);
    }
    Ok(true)
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

fn mode_allows(_metadata: &Metadata, _forbidden_mode: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        _metadata.mode() & _forbidden_mode == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn check_file(metadata: &Metadata) -> Result<(), &'static str> {
    if !metadata.is_file() || is_link(metadata) || !mode_allows(metadata, 0o077) {
        return Err(INVALID);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn checked_file_exists(path: &Path) -> Result<bool, &'static str> {
    let Some(metadata) = metadata_at(path)? else {
        return Ok(false);
    };
    check_file(&metadata)?;
    Ok(true)
}

fn acquire_lock(path: &Path) -> Result<File, &'static str> {
    checked_file_exists(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let lock = options.open(path).map_err(|_| UNAVAILABLE)?;
    check_file(&lock.metadata().map_err(|_| UNAVAILABLE)?)?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err("project_library_locked"),
        Err(TryLockError::Error(_)) => Err(UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::lock_storage;
    use std::{fs, path::Path};

    fn fixture_file(path: &Path) {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        options.open(path).unwrap().write_all(b"keep").unwrap();
    }

    #[test]
    fn listing_without_storage_creates_neither_profile_nor_storage() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("preview");
        assert!(lock_storage(&app, false).unwrap().is_none());
        assert!(!app.exists());

        fs::create_dir(&app).unwrap();
        assert!(lock_storage(&app, false).unwrap().is_none());
        assert!(!app.join("project-library").exists());
    }

    #[test]
    fn lock_is_held_until_drop_and_does_not_create_a_database() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("preview");
        let storage = lock_storage(&app, true).unwrap().unwrap();
        let database = app.join("project-library/library.sqlite");
        assert_eq!(storage.database(), database);
        assert!(!database.exists());
        assert_eq!(
            lock_storage(&app, false).unwrap_err(),
            "project_library_locked"
        );
        drop(storage);

        fixture_file(&database);
        assert_eq!(
            lock_storage(&app, false).unwrap().unwrap().database(),
            database
        );
        assert_eq!(fs::read(database).unwrap(), b"keep");
    }

    #[test]
    fn lock_coordinates_separate_host_processes() {
        use std::{env, process::Command};

        const FIXTURE: &str = "UEBOX_TEST_REGISTRATION_LOCK_FIXTURE";
        const EXPECTATION: &str = "UEBOX_TEST_REGISTRATION_LOCK_EXPECTATION";
        if let Some(path) = env::var_os(FIXTURE) {
            let result = lock_storage(Path::new(&path), false);
            match env::var(EXPECTATION).unwrap().as_str() {
                "busy" => assert_eq!(result.unwrap_err(), "project_library_locked"),
                "available" => assert!(result.unwrap().is_some()),
                unexpected => panic!("unexpected child probe expectation: {unexpected}"),
            }
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("preview");
        let run_child = |expectation: &str| {
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "project_library::storage::tests::lock_coordinates_separate_host_processes",
                ])
                .env(FIXTURE, &app)
                .env(EXPECTATION, expectation)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{stdout}"
            );
        };

        let storage = lock_storage(&app, true).unwrap().unwrap();
        run_child("busy");
        drop(storage);
        run_child("available");
        assert!(lock_storage(&app, false).unwrap().is_some());
    }

    #[test]
    fn journal_requires_recovery_only_when_a_main_file_exists() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path();
        let storage = lock_storage(app, true).unwrap().unwrap();
        let database = storage.database().to_owned();
        drop(storage);
        let journal = app.join("project-library/library.sqlite-journal");
        fixture_file(&journal);
        assert_eq!(
            lock_storage(app, true).unwrap_err(),
            "project_library_invalid"
        );
        assert!(!database.exists());

        fixture_file(&database);
        assert_eq!(
            lock_storage(app, false).unwrap_err(),
            "project_library_recovery_required"
        );
        assert_eq!(fs::read(&database).unwrap(), b"keep");
        assert_eq!(fs::read(&journal).unwrap(), b"keep");
        fs::remove_file(journal).unwrap();
        assert!(lock_storage(app, false).unwrap().is_some());
    }

    #[test]
    fn wal_and_shm_are_invalid_even_with_a_main_file_and_journal() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path();
        let storage = lock_storage(app, true).unwrap().unwrap();
        fixture_file(storage.database());
        drop(storage);
        let directory = app.join("project-library");
        fixture_file(&directory.join("library.sqlite-journal"));
        for name in ["library.sqlite-wal", "library.sqlite-shm"] {
            let sidecar = directory.join(name);
            fixture_file(&sidecar);
            assert_eq!(
                lock_storage(app, false).unwrap_err(),
                "project_library_invalid"
            );
            assert_eq!(fs::read(&sidecar).unwrap(), b"keep");
            fs::remove_file(sidecar).unwrap();
        }
    }

    #[test]
    fn directories_cannot_stand_in_for_database_lock_or_sidecar_files() {
        let temp = tempfile::tempdir().unwrap();
        drop(lock_storage(temp.path(), true).unwrap().unwrap());
        let directory = temp.path().join("project-library");
        fs::remove_file(directory.join("library.lock")).unwrap();
        for name in ["library.lock", "library.sqlite", "library.sqlite-journal"] {
            let entry = directory.join(name);
            fs::create_dir(&entry).unwrap();
            assert_eq!(
                lock_storage(temp.path(), true).unwrap_err(),
                "project_library_invalid"
            );
            assert!(entry.is_dir());
            fs::remove_dir(entry).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_and_storage_permissions_are_checked_separately() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("preview");
        drop(lock_storage(&app, true).unwrap().unwrap());
        let directory = app.join("project-library");
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(directory.join("library.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(lock_storage(&app, false).unwrap().is_some());
        fs::set_permissions(&app, fs::Permissions::from_mode(0o775)).unwrap();
        assert_eq!(
            lock_storage(&app, false).unwrap_err(),
            "project_library_invalid"
        );
        fs::set_permissions(&app, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            lock_storage(&app, false).unwrap_err(),
            "project_library_invalid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_rejected_at_the_profile_directory_and_file_boundaries() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("preview");
        symlink(temp.path(), &app).unwrap();
        assert_eq!(
            lock_storage(&app, true).unwrap_err(),
            "project_library_invalid"
        );
        assert!(!temp.path().join("project-library").exists());
        fs::remove_file(app).unwrap();

        let directory = temp.path().join("project-library");
        symlink(temp.path(), &directory).unwrap();
        assert_eq!(
            lock_storage(temp.path(), false).unwrap_err(),
            "project_library_invalid"
        );
        assert!(!temp.path().join("library.lock").exists());
        fs::remove_file(&directory).unwrap();

        drop(lock_storage(temp.path(), true).unwrap().unwrap());
        fs::remove_file(directory.join("library.lock")).unwrap();
        let target = temp.path().join("outside");
        fixture_file(&target);
        for name in ["library.lock", "library.sqlite", "library.sqlite-journal"] {
            let entry = directory.join(name);
            symlink(&target, &entry).unwrap();
            assert_eq!(
                lock_storage(temp.path(), true).unwrap_err(),
                "project_library_invalid"
            );
            fs::remove_file(entry).unwrap();
        }
        assert_eq!(fs::read(target).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn database_lock_and_journal_require_private_single_link_files() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let app = temp.path();
        let storage = lock_storage(app, true).unwrap().unwrap();
        fixture_file(storage.database());
        drop(storage);
        let directory = app.join("project-library");
        for name in ["library.sqlite", "library.lock", "library.sqlite-journal"] {
            let path = directory.join(name);
            if name == "library.sqlite-journal" {
                fixture_file(&path);
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                lock_storage(app, false).unwrap_err(),
                "project_library_invalid"
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let alias = app.join("alias");
            fs::hard_link(&path, &alias).unwrap();
            assert_eq!(
                lock_storage(app, false).unwrap_err(),
                "project_library_invalid"
            );
            fs::remove_file(alias).unwrap();
        }
    }
}
