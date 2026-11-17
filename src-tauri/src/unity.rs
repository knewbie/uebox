use serde::Serialize;
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use uebox_engine_unity::{BridgeOptions, Target, UnityClient};

#[derive(Default)]
pub(crate) struct UnityQueries {
    active_ports: Arc<Mutex<HashSet<u16>>>,
}

struct UnityPortReservation {
    active_ports: Arc<Mutex<HashSet<u16>>>,
    port: u16,
}

impl UnityQueries {
    fn reserve(&self, port: u16) -> Result<UnityPortReservation, String> {
        let mut active = self
            .active_ports
            .lock()
            .map_err(|_| "Unity query state is unavailable".to_string())?;
        if !active.insert(port) {
            return Err("A Unity query is already using this port".into());
        }
        Ok(UnityPortReservation {
            active_ports: Arc::clone(&self.active_ports),
            port,
        })
    }
}

impl Drop for UnityPortReservation {
    fn drop(&mut self) {
        let mut active = self
            .active_ports
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        active.remove(&self.port);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnityProject {
    project_root: String,
    project_name: String,
    unity_version: String,
    assets_path: String,
    platform: String,
    render_pipeline: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnityScene {
    name: String,
    path: String,
    build_index: i64,
    is_dirty: bool,
    is_loaded: bool,
    root_count: u64,
}

#[derive(Serialize)]
pub(crate) struct UnityInspection {
    project: UnityProject,
    scene: UnityScene,
}

#[tauri::command]
pub(crate) async fn inspect_unity(
    project_root: String,
    port: u16,
    allow_client_takeover: Option<bool>,
    queries: tauri::State<'_, UnityQueries>,
) -> Result<UnityInspection, String> {
    if allow_client_takeover != Some(true) {
        return Err("Unity client takeover requires explicit authorization".into());
    }

    // The UI cannot choose a remote address and validation happens before any socket opens.
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let target = Target::new(address, project_root).map_err(|error| error.to_string())?;
    let reservation = queries.reserve(port)?;
    // This one-use authorization is created only after the UI's explicit
    // takeover approval and is tied to this exact loopback target.
    let authorized_target = target.authorize_takeover();
    tauri::async_runtime::spawn_blocking(move || {
        let _reservation = reservation;
        let mut client =
            UnityClient::connect_authorized(authorized_target, BridgeOptions::default())
                .map_err(|error| error.to_string())?;
        // active_scene rechecks the reported project on this connection. This snapshot
        // was just updated by that successful read; it is not an online or instance ID.
        let scene = client.active_scene().map_err(|error| error.to_string())?;
        let project = client.last_observed_project_info();
        Ok(UnityInspection {
            project: UnityProject {
                project_root: project.project_root.clone(),
                project_name: project.project_name.clone(),
                unity_version: project.unity_version.clone(),
                assets_path: project.assets_path.clone(),
                platform: project.platform.clone(),
                render_pipeline: project.render_pipeline.clone(),
            },
            scene: UnityScene {
                name: scene.name,
                path: scene.path,
                build_index: scene.build_index,
                is_dirty: scene.is_dirty,
                is_loaded: scene.is_loaded,
                root_count: scene.root_count,
            },
        })
    })
    .await
    .map_err(|error| format!("Unity query worker failed: {error}"))?
}
