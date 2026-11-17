#[path = "support/environment.rs"]
mod environment;
#[path = "support/host.rs"]
mod host;
#[path = "support/paths.rs"]
mod paths;

use std::{collections::HashMap, ffi::OsString, path::Path, process::Command};

#[test]
fn windows_canonical_paths_match_godots_wire_representation() {
    assert_eq!(
        paths::windows_wire_path(r"\\?\C:\Projects\Godot Demo"),
        "C:/Projects/Godot Demo"
    );
    assert_eq!(
        paths::windows_wire_path(r"\\?\UNC\server\share\Godot Demo"),
        "//server/share/Godot Demo"
    );
    assert_eq!(
        paths::windows_wire_path(r"C:\Projects\Godot Demo\"),
        "C:/Projects/Godot Demo"
    );
    if cfg!(windows) {
        assert_eq!(
            paths::godot_wire_path(Path::new(r"C:\Projects\Fixture")),
            "C:/Projects/Fixture"
        );
    } else {
        assert_eq!(paths::godot_wire_path(Path::new(r"/tmp/A\B")), r"/tmp/A\B");
    }
}

#[test]
fn windows_canonical_paths_for_godot_process_remove_extended_prefix() {
    assert_eq!(
        paths::windows_host_path(r"\\?\C:\Projects\Fixture"),
        r"C:\Projects\Fixture"
    );
    assert_eq!(
        paths::windows_host_path(r"\\?\UNC\server\share\Fixture"),
        r"\\server\share\Fixture"
    );
    assert_eq!(
        host::godot_host_path(Path::new("fixture")),
        Path::new("fixture")
    );
}

#[test]
fn editor_environment_overrides_godot_config_cache_and_temporary_paths() {
    let home = Path::new("isolated-fixture-home");
    let mut command = Command::new("godot");
    environment::configure_isolated_environment(&mut command, home);
    let env: HashMap<OsString, OsString> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.unwrap().to_owned()))
        .collect();
    for (key, suffix) in [
        ("HOME", ""),
        ("USERPROFILE", ""),
        ("APPDATA", "appdata"),
        ("LOCALAPPDATA", "localappdata"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("TMPDIR", "temp"),
        ("TMP", "temp"),
        ("TEMP", "temp"),
    ] {
        let expected = if suffix.is_empty() {
            home.to_path_buf()
        } else {
            home.join(suffix)
        };
        assert_eq!(
            env.get(std::ffi::OsStr::new(key)),
            Some(&expected.into_os_string()),
            "{key}"
        );
    }
}
