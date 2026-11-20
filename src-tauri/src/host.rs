use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub id: &'static str,
    pub available: bool,
}

#[derive(Serialize)]
pub struct EngineModules {
    pub unity: bool,
    pub unreal: bool,
    pub godot: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub backend: &'static str,
    pub process_id: u32,
    pub os: &'static str,
    pub architecture: &'static str,
    pub version: &'static str,
    pub migration: &'static str,
    pub engine_modules: EngineModules,
    pub capabilities: Vec<Capability>,
}

/// Describes this process, not an editor connection or an Electron compatibility layer.
pub fn read_host_status() -> HostStatus {
    let process_id = std::process::id();
    if std::env::var_os("UEBOX_TAURI_TRACE").as_deref() == Some(std::ffi::OsStr::new("1")) {
        eprintln!("host_status process_id={process_id}");
    }
    HostStatus {
        backend: "rust",
        process_id,
        os: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        version: env!("CARGO_PKG_VERSION"),
        migration: "in_progress",
        engine_modules: EngineModules {
            unity: cfg!(feature = "unity"),
            unreal: cfg!(feature = "unreal"),
            godot: cfg!(feature = "godot"),
        },
        capabilities: [
            "engines",
            "agent",
            "libraries",
            "settings",
            "projectRegistration",
            "chatHistory",
        ]
        .into_iter()
        .map(|id| Capability {
            id,
            available: matches!(id, "projectRegistration" | "chatHistory")
                && cfg!(any(unix, windows)),
        })
        .collect(),
    }
}

#[tauri::command]
pub(crate) fn host_status() -> HostStatus {
    read_host_status()
}
