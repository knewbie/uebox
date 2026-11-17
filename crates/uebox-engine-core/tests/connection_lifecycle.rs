use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use uebox_engine_core::{
    EngineKind, EngineRegistry, InteractionMode, ProjectIdentity, RegistryError,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "uebox-engine-lifecycle-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create isolated test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn old_selection_cannot_silently_route_to_reconnected_editor() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Unity, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let old = registry
        .open_connection(EngineKind::Unity, "editor-1")
        .unwrap();
    registry.identify(&old, project.clone(), true).unwrap();
    let selection = registry.select(&project, Some("editor-1")).unwrap();
    assert!(registry.close_connection(&old));

    let current = registry
        .open_connection(EngineKind::Unity, "editor-1")
        .unwrap();
    registry.identify(&current, project.clone(), true).unwrap();
    assert_eq!(
        registry.resolve_selection(&selection),
        Err(RegistryError::StaleConnection)
    );
    let fresh = registry.select(&project, Some("editor-1")).unwrap();
    assert_eq!(
        registry.resolve_selection(&fresh).unwrap().generation(),
        current.generation()
    );
}

#[test]
fn old_connection_token_cannot_select_a_replacement_from_a_stale_list() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Unity, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let old = registry
        .open_connection(EngineKind::Unity, "editor-1")
        .unwrap();
    registry.identify(&old, project.clone(), true).unwrap();
    assert!(registry.close_connection(&old));
    let current = registry
        .open_connection(EngineKind::Unity, "editor-1")
        .unwrap();
    registry.identify(&current, project.clone(), true).unwrap();

    assert_eq!(
        registry.select_exact(&old, &project, true),
        Err(RegistryError::StaleConnection)
    );
    let selected = registry.select_exact(&current, &project, true).unwrap();
    assert_eq!(
        registry.resolve_selection(&selected).unwrap().generation(),
        current.generation()
    );
}

#[test]
fn token_from_a_different_registry_cannot_identify_or_close_connection() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Unity, &directory.0).unwrap();
    let mut first_registry = EngineRegistry::default();
    let foreign = first_registry
        .open_connection(EngineKind::Unity, "editor-1")
        .unwrap();
    let mut second_registry = EngineRegistry::default();
    let local = second_registry
        .open_connection(EngineKind::Unity, "editor-1")
        .unwrap();

    assert_eq!(
        second_registry.identify(&foreign, project.clone(), true),
        Err(RegistryError::StaleConnection)
    );
    assert!(!second_registry.close_connection(&foreign));
    second_registry
        .identify(&local, project.clone(), true)
        .unwrap();
    assert!(second_registry.select(&project, None).is_ok());
    let other_directory = TestDirectory::new();
    let other_project = ProjectIdentity::new(EngineKind::Unity, &other_directory.0).unwrap();
    assert_eq!(
        second_registry.identify(&foreign, other_project, true),
        Err(RegistryError::StaleConnection)
    );
    assert!(second_registry.select(&project, None).is_ok());
}

#[test]
fn late_identification_cannot_revive_a_closed_connection() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Godot, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let old = registry
        .open_connection(EngineKind::Godot, "editor")
        .unwrap();
    assert!(registry.close_connection(&old));
    let current = registry
        .open_connection(EngineKind::Godot, "editor")
        .unwrap();
    assert_eq!(
        registry.identify(&old, project.clone(), true),
        Err(RegistryError::StaleConnection)
    );
    assert_eq!(
        registry.select(&project, None),
        Err(RegistryError::NotFound)
    );
    assert!(!registry.close_connection(&old));
    registry.identify(&current, project.clone(), true).unwrap();
    assert!(registry.select(&project, None).is_ok());
}

#[test]
fn project_identity_and_engine_are_isolated() {
    let first = TestDirectory::new();
    let second = TestDirectory::new();
    let unity_a = ProjectIdentity::new(EngineKind::Unity, &first.0).unwrap();
    let unity_b = ProjectIdentity::new(EngineKind::Unity, &second.0).unwrap();
    let godot = ProjectIdentity::new(EngineKind::Godot, &first.0).unwrap();
    let mut registry = EngineRegistry::default();
    let connection = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    assert_eq!(
        registry.identify(&connection, godot.clone(), true),
        Err(RegistryError::EngineMismatch)
    );
    let connection = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    registry
        .identify(&connection, unity_a.clone(), true)
        .unwrap();
    assert_eq!(
        registry.select(&unity_b, Some("editor")),
        Err(RegistryError::NotFound)
    );
    assert_eq!(
        registry.select(&godot, Some("editor")),
        Err(RegistryError::NotFound)
    );
    assert!(registry.select(&unity_a, Some("editor")).is_ok());
    let godot_connection = registry
        .open_connection(EngineKind::Godot, "editor")
        .unwrap();
    registry
        .identify(&godot_connection, godot.clone(), true)
        .unwrap();
    let unity_selection = registry.select(&unity_a, Some("editor")).unwrap();
    let godot_selection = registry.select(&godot, Some("editor")).unwrap();
    assert_eq!(
        registry
            .resolve_selection(&unity_selection)
            .unwrap()
            .project(),
        &unity_a
    );
    assert_eq!(
        registry
            .resolve_selection(&godot_selection)
            .unwrap()
            .project(),
        &godot
    );
}

#[test]
fn conflicting_project_announcement_retires_the_current_connection() {
    let first = TestDirectory::new();
    let second = TestDirectory::new();
    let project_a = ProjectIdentity::new(EngineKind::Unity, &first.0).unwrap();
    let project_b = ProjectIdentity::new(EngineKind::Unity, &second.0).unwrap();
    let mut registry = EngineRegistry::default();
    let connection = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    registry
        .identify(&connection, project_a.clone(), true)
        .unwrap();
    let explicit = registry.select(&project_a, Some("editor")).unwrap();
    let implicit = registry.select(&project_a, None).unwrap();
    assert_eq!(
        registry.select_exact(&connection, &project_b, false),
        Err(RegistryError::NotFound)
    );

    assert_eq!(
        registry.identify(&connection, project_b.clone(), true),
        Err(RegistryError::InstanceBelongsToAnotherProject)
    );
    assert_eq!(
        registry.resolve_selection(&explicit),
        Err(RegistryError::StaleConnection)
    );
    assert_eq!(
        registry.resolve_selection(&implicit),
        Err(RegistryError::StaleConnection)
    );
    assert_eq!(
        registry.select(&project_a, None),
        Err(RegistryError::NotFound)
    );
    assert_eq!(
        registry.identify(&connection, project_a.clone(), true),
        Err(RegistryError::StaleConnection)
    );
    let replacement = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    registry
        .identify(&replacement, project_b.clone(), true)
        .unwrap();
    assert_eq!(
        registry.select_exact(&connection, &project_b, false),
        Err(RegistryError::StaleConnection)
    );
    assert!(registry
        .select_exact(&replacement, &project_b, false)
        .is_ok());
    assert_eq!(
        registry.select_exact(&replacement, &project_a, false),
        Err(RegistryError::NotFound)
    );
}

#[test]
fn conflicting_engine_announcement_retires_only_the_current_connection() {
    let directory = TestDirectory::new();
    let unity = ProjectIdentity::new(EngineKind::Unity, &directory.0).unwrap();
    let godot = ProjectIdentity::new(EngineKind::Godot, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let connection = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    registry.identify(&connection, unity.clone(), true).unwrap();
    let selected = registry.select(&unity, None).unwrap();

    assert_eq!(
        registry.identify(&connection, godot, true),
        Err(RegistryError::EngineMismatch)
    );
    assert_eq!(
        registry.resolve_selection(&selected),
        Err(RegistryError::StaleConnection)
    );
    assert_eq!(registry.select(&unity, None), Err(RegistryError::NotFound));
}

#[test]
fn concurrent_registries_issue_distinct_connection_generations() {
    let handles: Vec<_> = (0..16)
        .map(|_| {
            std::thread::spawn(|| {
                EngineRegistry::default()
                    .open_connection(EngineKind::Unreal, "editor")
                    .unwrap()
                    .generation()
            })
        })
        .collect();
    let generations: std::collections::HashSet<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(generations.len(), 16);
}

#[test]
fn duplicate_instance_id_in_same_engine_cannot_replace_a_live_connection() {
    let mut registry = EngineRegistry::default();
    let unity = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    assert_eq!(
        registry.open_connection(EngineKind::Unity, "editor"),
        Err(RegistryError::AlreadyConnected)
    );
    assert!(registry
        .open_connection(EngineKind::Godot, "editor")
        .is_ok());
    assert!(registry.close_connection(&unity));
}

#[test]
fn ambiguous_implicit_selection_is_rejected_and_explicit_selection_works() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Unity, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    for id in ["one", "two"] {
        let token = registry.open_connection(EngineKind::Unity, id).unwrap();
        registry.identify(&token, project.clone(), true).unwrap();
    }
    assert_eq!(
        registry.select(&project, None),
        Err(RegistryError::Ambiguous)
    );
    let selected = registry.select(&project, Some("two")).unwrap();
    assert_eq!(
        registry.resolve_selection(&selected).unwrap().instance_id(),
        "two"
    );
}

#[test]
fn implicit_selection_excludes_headless_editors() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Godot, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let headless = registry
        .open_connection(EngineKind::Godot, "headless")
        .unwrap();
    registry
        .identify(&headless, project.clone(), false)
        .unwrap();
    assert_eq!(
        registry.select(&project, None),
        Err(RegistryError::NotFound)
    );
    assert!(registry.select(&project, Some("headless")).is_ok());
    let interactive = registry
        .open_connection(EngineKind::Godot, "interactive")
        .unwrap();
    registry
        .identify(&interactive, project.clone(), true)
        .unwrap();
    let selected = registry.select(&project, None).unwrap();
    assert_eq!(
        registry.resolve_selection(&selected).unwrap().instance_id(),
        "interactive"
    );
}

#[test]
fn unknown_interaction_is_not_falsely_classified_as_headless_or_implicit() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Unity, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let connection = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    registry
        .identify_with_mode(&connection, project.clone(), InteractionMode::Unknown)
        .unwrap();

    assert_eq!(
        registry.select(&project, None),
        Err(RegistryError::NotFound)
    );
    let selected = registry.select_exact(&connection, &project, false).unwrap();
    assert_eq!(
        registry
            .resolve_selection(&selected)
            .unwrap()
            .interaction_mode(),
        InteractionMode::Unknown
    );
    assert_eq!(
        registry.select_exact(&connection, &project, true),
        Err(RegistryError::NotFound)
    );
}

#[test]
fn changing_from_interactive_to_unknown_invalidates_implicit_selection() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Unity, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let connection = registry
        .open_connection(EngineKind::Unity, "editor")
        .unwrap();
    registry
        .identify(&connection, project.clone(), true)
        .unwrap();
    let selected = registry.select(&project, None).unwrap();
    registry
        .identify_with_mode(&connection, project.clone(), InteractionMode::Unknown)
        .unwrap();
    assert_eq!(
        registry.resolve_selection(&selected),
        Err(RegistryError::StaleConnection)
    );
    let explicit = registry.select_exact(&connection, &project, false).unwrap();
    assert_eq!(
        registry
            .resolve_selection(&explicit)
            .unwrap()
            .interaction_mode(),
        InteractionMode::Unknown
    );
}

#[test]
fn implicit_selection_cannot_remain_valid_after_editor_becomes_headless() {
    let directory = TestDirectory::new();
    let project = ProjectIdentity::new(EngineKind::Godot, &directory.0).unwrap();
    let mut registry = EngineRegistry::default();
    let connection = registry
        .open_connection(EngineKind::Godot, "editor")
        .unwrap();
    registry
        .identify(&connection, project.clone(), true)
        .unwrap();
    let selected = registry.select(&project, None).unwrap();
    registry
        .identify(&connection, project.clone(), false)
        .unwrap();

    assert_eq!(
        registry.resolve_selection(&selected),
        Err(RegistryError::StaleConnection)
    );
    let explicit = registry.select(&project, Some("editor")).unwrap();
    assert!(!registry
        .resolve_selection(&explicit)
        .unwrap()
        .is_interactive());
}

#[test]
fn invalid_names_and_project_files_are_rejected() {
    let directory = TestDirectory::new();
    let mut registry = EngineRegistry::default();
    assert_eq!(
        registry.open_connection(EngineKind::Unity, " \t"),
        Err(RegistryError::InvalidInstanceId)
    );
    let file = directory.0.join("not-a-project");
    std::fs::write(&file, "not a directory").unwrap();
    assert_eq!(
        ProjectIdentity::new(EngineKind::Unity, &file)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[cfg(unix)]
#[test]
fn symbolic_link_to_project_resolves_to_the_same_identity() {
    let directory = TestDirectory::new();
    let project_path = directory.0.join("project");
    let alias_path = directory.0.join("alias");
    std::fs::create_dir(&project_path).unwrap();
    std::os::unix::fs::symlink(&project_path, &alias_path).unwrap();
    assert_eq!(
        ProjectIdentity::new(EngineKind::Godot, &project_path).unwrap(),
        ProjectIdentity::new(EngineKind::Godot, &alias_path).unwrap()
    );
}
