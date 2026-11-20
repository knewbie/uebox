#[cfg(feature = "godot")]
mod godot;
#[cfg(feature = "godot")]
mod godot_agent;
mod host;
mod project_library;
#[cfg(feature = "unity")]
mod unity;
#[cfg(feature = "unity")]
mod unity_agent;
#[cfg(feature = "unreal")]
mod unreal;
#[cfg(feature = "unreal")]
mod unreal_agent;

pub use host::{read_host_status, Capability, EngineModules, HostStatus};

/// Register the deliberately small command surface on a Tauri runtime.
pub fn configure_commands<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    let builder = builder.manage(project_library::ProjectRegistrations::default());
    #[cfg(feature = "godot")]
    let builder = builder.manage(godot::GodotSessions::default());
    #[cfg(feature = "unity")]
    let builder = builder.manage(unity::UnityQueries::default());
    #[cfg(feature = "unreal")]
    let builder = builder.manage(unreal::UnrealListenerState::default());
    builder.invoke_handler(tauri::generate_handler![
        host::host_status,
        project_library::list_registered_projects,
        project_library::register_project,
        project_library::remove_registered_project,
        #[cfg(feature = "godot")]
        godot::launch_godot,
        #[cfg(feature = "godot")]
        godot::connect_godot,
        #[cfg(feature = "godot")]
        godot::query_godot_project,
        #[cfg(feature = "godot")]
        godot::query_godot_scene,
        #[cfg(feature = "godot")]
        godot::status_godot_session,
        #[cfg(feature = "godot")]
        godot::release_godot_session,
        #[cfg(feature = "godot")]
        godot_agent::create_godot_agent,
        #[cfg(feature = "godot")]
        godot_agent::start_godot_agent,
        #[cfg(feature = "godot")]
        godot_agent::poll_godot_agent,
        #[cfg(feature = "godot")]
        godot_agent::cancel_godot_agent,
        #[cfg(feature = "godot")]
        godot_agent::close_godot_agent,
        #[cfg(feature = "unity")]
        unity::inspect_unity,
        #[cfg(feature = "unity")]
        unity_agent::attach_unity_agent_target,
        #[cfg(feature = "unity")]
        unity_agent::release_unity_agent_target,
        #[cfg(feature = "unity")]
        unity_agent::create_unity_agent,
        #[cfg(feature = "unity")]
        unity_agent::start_unity_agent,
        #[cfg(feature = "unity")]
        unity_agent::poll_unity_agent,
        #[cfg(feature = "unity")]
        unity_agent::cancel_unity_agent,
        #[cfg(feature = "unity")]
        unity_agent::close_unity_agent,
        #[cfg(feature = "unreal")]
        unreal::start_unreal_listener,
        #[cfg(feature = "unreal")]
        unreal::list_unreal_instances,
        #[cfg(feature = "unreal")]
        unreal::query_unreal_project,
        #[cfg(feature = "unreal")]
        unreal::stop_unreal_listener,
        #[cfg(feature = "unreal")]
        unreal_agent::create_unreal_agent,
        #[cfg(feature = "unreal")]
        unreal_agent::start_unreal_agent,
        #[cfg(feature = "unreal")]
        unreal_agent::poll_unreal_agent,
        #[cfg(feature = "unreal")]
        unreal_agent::cancel_unreal_agent,
        #[cfg(feature = "unreal")]
        unreal_agent::close_unreal_agent
    ])
}

pub fn run() {
    configure_commands(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("failed to run the Tauri host");
}
