mod chat_history;
#[cfg(feature = "godot")]
mod godot;
#[cfg(feature = "godot")]
mod godot_agent;
mod host;
mod legacy_history;
mod profile_storage;
mod project_library;
#[cfg(feature = "unity")]
mod unity;
#[cfg(feature = "unity")]
mod unity_agent;
#[cfg(feature = "unreal")]
mod unreal;
#[cfg(feature = "unreal")]
mod unreal_actor_tool;
#[cfg(feature = "unreal")]
mod unreal_agent;
#[cfg(feature = "unreal")]
mod unreal_approval;

pub use host::{read_host_status, Capability, EngineModules, HostStatus};

/// Register the deliberately small command surface on a Tauri runtime.
pub fn configure_commands<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    let builder = builder
        .manage(project_library::ProjectRegistrations::default())
        .manage(chat_history::ChatHistoryHost::default())
        .manage(legacy_history::LegacyHistoryHost::default());
    #[cfg(feature = "godot")]
    let builder = builder.manage(godot::GodotSessions::default());
    #[cfg(feature = "unity")]
    let builder = builder.manage(unity::UnityQueries::default());
    #[cfg(feature = "unreal")]
    let builder = builder
        .manage(unreal::UnrealListenerState::default())
        .on_window_event(|window, event| {
            use tauri::Manager;
            if matches!(event, tauri::WindowEvent::Destroyed) {
                window
                    .state::<unreal::UnrealListenerState>()
                    .agents
                    .destroy_window(window.label());
            }
        });
    builder.invoke_handler(tauri::generate_handler![
        host::host_status,
        chat_history::list_agent_chats,
        chat_history::get_agent_chat,
        legacy_history::import_legacy_chat_history,
        legacy_history::list_legacy_chats,
        legacy_history::get_legacy_chat,
        legacy_history::read_legacy_chat_attachment,
        project_library::list_registered_projects,
        project_library::register_project,
        project_library::remove_registered_project,
        project_library::import_registered_projects,
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
        unreal_agent::close_unreal_agent,
        #[cfg(feature = "unreal")]
        unreal_agent::list_unreal_actor_approvals,
        #[cfg(feature = "unreal")]
        unreal_agent::answer_unreal_actor_approval
    ])
}

pub fn run() {
    configure_commands(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("failed to run the Tauri host");
}
