use std::path::Path;

pub fn windows_host_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
    }
}

pub fn windows_wire_path(path: &str) -> String {
    let path = windows_host_path(path);
    let mut path = path.replace('\\', "/");
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

pub fn godot_wire_path(path: &Path) -> String {
    let raw = path.to_str().expect("fixture path must be UTF-8");
    if cfg!(windows) {
        windows_wire_path(raw)
    } else {
        raw.to_owned()
    }
}
