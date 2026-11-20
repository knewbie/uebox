use super::{files::Workspace, state::*};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "uebox-recording-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root).unwrap();
        Self(root)
    }

    fn work(&self, seed: u8) -> Workspace {
        Workspace::prepare(&self.0, [seed; 16]).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn start(machine: &mut Machine, fixture: &Fixture, seed: u8) -> (u64, PathBuf, PathBuf) {
    let work = fixture.work(seed);
    let path = work.path().to_owned();
    let final_path = fixture.0.join(work.final_name());
    let token = machine.begin(work, true).unwrap();
    (token, path, final_path)
}

#[test]
fn only_main_and_explicit_opt_in_can_start() {
    assert_eq!(authorize("main"), Ok(()));
    for label in ["browser", "uebox-isolated-browser", "Main", ""] {
        assert_eq!(authorize(label), Err(FORBIDDEN));
        assert_eq!(check_start(label, true, true, false), Err(FORBIDDEN));
    }
    assert_eq!(
        check_start("main", false, true, false),
        Err(OPT_IN_REQUIRED)
    );
    assert_eq!(check_start("main", true, false, false), Err(UNSUPPORTED));
    assert_eq!(check_start("main", true, true, true), Err(BUSY));
    assert_eq!(check_start("main", true, true, false), Ok(()));
}

#[test]
fn one_active_token_is_reserved_until_a_terminal_callback() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (token, path, _) = start(&mut machine, &fixture, 1);
    assert_eq!(machine.begin(fixture.work(2), false), Err(BUSY));
    assert!(path.parent().unwrap().exists());
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    assert_eq!(machine.stop(token), Ok(true));
    assert!(machine.busy());
    assert_eq!(machine.snapshot().phase, Phase::Stopping);
    machine.event(token, 4);
    assert!(!machine.busy());
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn stop_is_idempotent_and_late_start_notifications_cannot_undo_it() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (token, _, _) = start(&mut machine, &fixture, 3);
    assert_eq!(machine.stop(token), Ok(true));
    assert_eq!(machine.stop(token), Ok(false));
    machine.event(token, 1);
    machine.event(token, 2);
    assert_eq!(machine.snapshot().phase, Phase::Stopping);
    machine.event(token, 4);
    assert_eq!(machine.stop(token), Ok(false));
    assert_eq!(machine.snapshot().phase, Phase::Cancelled);
}

#[test]
fn only_finalized_publishes_a_complete_private_file_and_cleans_staging() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (token, path, final_path) = start(&mut machine, &fixture, 4);
    fs::write(&path, b"synthetic finalized mp4 fixture").unwrap();
    machine.event(token, 1);
    machine.event(token, 2);
    assert_eq!(machine.snapshot().phase, Phase::Recording);
    assert!(!final_path.exists());
    machine.stop(token).unwrap();
    machine.event(token, 3);
    let status = machine.snapshot();
    assert_eq!(status.phase, Phase::Completed);
    assert_eq!(
        status.file_name.as_deref(),
        final_path.file_name().unwrap().to_str()
    );
    assert_eq!(status.error, None);
    assert!(!status.cleanup_pending);
    assert_eq!(
        fs::read(&final_path).unwrap(),
        b"synthetic finalized mp4 fixture"
    );
    assert!(!path.parent().unwrap().exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(final_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn stale_and_duplicate_callbacks_cannot_replace_or_clean_a_new_task() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (old, old_path, _) = start(&mut machine, &fixture, 5);
    machine.event(old, 4);
    let (next, next_path, _) = start(&mut machine, &fixture, 6);
    assert_ne!(old, next);
    fs::write(&next_path, b"new task").unwrap();
    for event in [1, 2, 3, 4, 5, 6, 7] {
        machine.event(old, event);
        assert_eq!(machine.snapshot().token, Some(next));
        assert_eq!(machine.snapshot().phase, Phase::Selecting);
        assert_eq!(fs::read(&next_path).unwrap(), b"new task");
    }
    assert!(!old_path.parent().unwrap().exists());
    machine.event(next, 2);
    machine.event(next, 4);
    let before = machine.snapshot();
    machine.event(next, 3);
    assert_eq!(machine.snapshot(), before);
}

#[test]
fn wrong_stop_tokens_have_no_effect_including_a_previous_task_token() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (old, _, _) = start(&mut machine, &fixture, 7);
    machine.event(old, 4);
    let (token, path, _) = start(&mut machine, &fixture, 8);
    let before = machine.snapshot();
    for wrong in [0, old, token + 1, u64::MAX] {
        assert_eq!(machine.stop(wrong), Err(INVALID_TOKEN));
        assert_eq!(machine.snapshot(), before);
        assert!(path.parent().unwrap().exists());
    }
    for invalid in [
        "",
        "0",
        "01",
        "+1",
        " 1",
        "1 ",
        "../1",
        "18446744073709551616",
    ] {
        assert_eq!(parse_token(invalid), Err(INVALID_TOKEN));
    }
    assert_eq!(parse_token(&u64::MAX.to_string()), Ok(u64::MAX));
    machine.event(token, 4);
}

#[test]
fn each_native_failure_cleans_only_its_temporary_output() {
    let fixture = Fixture::new();
    let preserved = fixture.0.join("existing.mp4");
    fs::write(&preserved, b"keep").unwrap();
    let mut machine = Machine::default();
    for (event, expected) in [
        (4, Phase::Cancelled),
        (5, Phase::Failed),
        (6, Phase::Unsupported),
        (7, Phase::Busy),
    ] {
        let (token, path, final_path) = start(&mut machine, &fixture, event as u8 + 10);
        fs::write(&path, b"partial capture").unwrap();
        machine.event(token, event);
        assert_eq!(machine.snapshot().phase, expected);
        assert_eq!(machine.snapshot().file_name, None);
        assert!(!machine.busy());
        assert!(!path.parent().unwrap().exists());
        assert!(!final_path.exists());
        assert_eq!(fs::read(&preserved).unwrap(), b"keep");
    }
}

#[test]
fn finalized_does_not_overwrite_an_existing_destination() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (token, path, final_path) = start(&mut machine, &fixture, 20);
    fs::write(&path, b"new capture").unwrap();
    fs::write(&final_path, b"keep existing recording").unwrap();
    machine.event(token, 3);
    assert_eq!(machine.snapshot().phase, Phase::Failed);
    assert_eq!(machine.snapshot().error, Some(PUBLISH_FAILED));
    assert_eq!(machine.snapshot().file_name, None);
    assert_eq!(fs::read(final_path).unwrap(), b"keep existing recording");
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn missing_empty_or_non_file_outputs_cannot_be_published() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    for seed in 21..24 {
        let (token, path, final_path) = start(&mut machine, &fixture, seed);
        if seed == 22 {
            fs::write(&path, []).unwrap();
        }
        if seed == 23 {
            fs::create_dir(&path).unwrap();
        }
        machine.event(token, 3);
        assert_eq!(machine.snapshot().phase, Phase::Failed);
        assert!(!final_path.exists());
        if seed == 23 {
            // Unknown directories are never recursively removed by recording cleanup.
            assert!(machine.snapshot().cleanup_pending);
            fs::remove_dir(&path).unwrap();
            fs::remove_dir(path.parent().unwrap()).unwrap();
        } else {
            assert!(!path.parent().unwrap().exists());
        }
    }
}

#[test]
fn cleanup_preserves_unowned_files_even_inside_its_staging_directory() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (token, path, _) = start(&mut machine, &fixture, 24);
    fs::write(&path, b"partial").unwrap();
    let other = path.parent().unwrap().join("unowned.txt");
    fs::write(&other, b"preserve").unwrap();
    machine.event(token, 5);
    assert!(!path.exists());
    assert!(machine.snapshot().cleanup_pending);
    assert_eq!(fs::read(other).unwrap(), b"preserve");
}

#[cfg(unix)]
#[test]
fn symlink_outputs_and_destinations_never_modify_their_targets() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let external = fixture.0.join("unowned.txt");
    fs::write(&external, b"preserve").unwrap();
    let mut machine = Machine::default();
    let (token, path, final_path) = start(&mut machine, &fixture, 25);
    symlink(&external, &path).unwrap();
    machine.event(token, 3);
    assert_eq!(machine.snapshot().phase, Phase::Failed);
    assert!(!final_path.exists());
    assert!(!path.parent().unwrap().exists());
    let (next, path, final_path) = start(&mut machine, &fixture, 26);
    fs::write(&path, b"new").unwrap();
    symlink(&external, &final_path).unwrap();
    machine.event(next, 3);
    assert_eq!(machine.snapshot().phase, Phase::Failed);
    assert!(fs::symlink_metadata(final_path).unwrap().is_symlink());
    assert_eq!(fs::read(&external).unwrap(), b"preserve");
}

#[cfg(unix)]
#[test]
fn hard_linked_output_cannot_publish_or_change_the_original_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let original = fixture.0.join("original.txt");
    fs::write(&original, b"preserve").unwrap();
    fs::set_permissions(&original, fs::Permissions::from_mode(0o640)).unwrap();
    let mut machine = Machine::default();
    let (token, path, final_path) = start(&mut machine, &fixture, 28);
    fs::hard_link(&original, &path).unwrap();
    machine.event(token, 3);
    assert_eq!(machine.snapshot().phase, Phase::Failed);
    assert!(!final_path.exists());
    assert!(!path.parent().unwrap().exists());
    assert_eq!(fs::read(&original).unwrap(), b"preserve");
    assert_eq!(
        fs::metadata(original).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[cfg(unix)]
#[test]
fn replaced_staging_directory_is_never_published_or_cleaned() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let other = fixture.0.join("unowned");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("output.mp4"), b"preserve").unwrap();
    let mut machine = Machine::default();
    for (seed, event) in [(29, 3), (30, 5)] {
        let (token, path, final_path) = start(&mut machine, &fixture, seed);
        let staging = path.parent().unwrap();
        fs::rename(staging, fixture.0.join(format!("displaced-{seed}"))).unwrap();
        symlink(&other, staging).unwrap();
        machine.event(token, event);
        assert_eq!(machine.snapshot().phase, Phase::Failed);
        assert!(machine.snapshot().cleanup_pending);
        assert!(!final_path.exists());
        assert_eq!(fs::read(other.join("output.mp4")).unwrap(), b"preserve");
        assert!(fs::symlink_metadata(staging).unwrap().is_symlink());
    }
}

#[test]
fn unknown_events_leave_the_session_reserved_and_nonce_collisions_preserve_existing_data() {
    let fixture = Fixture::new();
    let mut machine = Machine::default();
    let (token, path, _) = start(&mut machine, &fixture, 27);
    machine.event(token, 99);
    assert!(machine.busy());
    assert_eq!(machine.snapshot().phase, Phase::Selecting);
    assert!(Workspace::prepare(&fixture.0, [27; 16]).is_err());
    assert!(path.parent().unwrap().exists());
    machine.event(token, 4);
}

#[test]
fn idle_snapshot_contains_no_path_or_reserved_token() {
    let status = Machine::default().snapshot();
    assert_eq!(status.phase, Phase::Idle);
    assert_eq!(status.token, None);
    assert_eq!(status.file_name, None);
    assert_eq!(status.error, None);
    assert!(!status.cleanup_pending);
    assert!(Workspace::prepare(Path::new("relative"), [0; 16]).is_err());
}
