use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use uebox_engine_unreal::{InstanceSummary, UnrealBridge};

#[derive(Default)]
pub(crate) struct UnrealListenerState {
    pub(crate) bridge: tauri::async_runtime::Mutex<Option<Arc<UnrealBridge>>>,
    pub(crate) agents: Arc<crate::unreal_agent::UnrealAgentHost>,
}

#[derive(Serialize)]
pub(crate) struct UnrealListenerInfo {
    address: String,
    port: u16,
}

#[tauri::command]
pub(crate) async fn start_unreal_listener(
    port: u16,
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<UnrealListenerInfo, String> {
    if port == 0 {
        return Err("A specific nonzero Unreal listener port is required".into());
    }
    let mut bridge = state.bridge.lock().await;
    if bridge.is_some() {
        return Err("Unreal listener is already running".into());
    }
    let listener = UnrealBridge::bind(port)
        .await
        .map_err(|error| format!("Failed to start Unreal listener: {error}"))?;
    let address = listener.local_addr();
    *bridge = Some(Arc::new(listener));
    Ok(UnrealListenerInfo {
        address: address.ip().to_string(),
        port: address.port(),
    })
}

#[tauri::command]
pub(crate) async fn list_unreal_instances(
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<Vec<InstanceSummary>, String> {
    let bridge = state.bridge.lock().await;
    bridge
        .as_ref()
        .map(|listener| listener.instances())
        .ok_or_else(|| "Unreal listener is not running".into())
}

#[tauri::command]
pub(crate) async fn query_unreal_project(
    connection_id: String,
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<serde_json::Value, String> {
    let selected = {
        let bridge = state.bridge.lock().await;
        bridge
            .as_ref()
            .ok_or_else(|| "Unreal listener is not running".to_string())?
            .select_instance(&connection_id)
            .map_err(|error| error.to_string())?
    };
    selected
        .get_project_info(Duration::from_secs(10))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn stop_unreal_listener(
    state: tauri::State<'_, UnrealListenerState>,
) -> Result<(), String> {
    let mut bridge = state.bridge.lock().await;
    let listener = bridge
        .take()
        .ok_or_else(|| "Unreal listener is not running".to_string())?;
    state.agents.retire_all();
    listener.shutdown().await;
    Ok(())
}
