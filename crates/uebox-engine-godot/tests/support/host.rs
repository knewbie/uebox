use std::path::{Path, PathBuf};

pub fn godot_host_path(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(super::paths::windows_host_path(
            path.to_str().expect("fixture path must be UTF-8"),
        ))
    } else {
        path.to_path_buf()
    }
}
