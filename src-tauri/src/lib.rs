mod asset_index;
mod browser;
mod chat_history;
mod desktop;
mod desktop_browser;
mod desktop_files;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod desktop_shortcut;
#[cfg(feature = "godot")]
mod godot;
#[cfg(feature = "godot")]
mod godot_agent;
#[cfg(feature = "godot")]
mod godot_rename_tool;
mod host;
mod legacy_history;
mod mcp;
#[cfg(all(
    any(target_os = "macos", target_os = "linux", windows),
    any(feature = "unreal", feature = "unity", feature = "godot")
))]
mod native_cli;
mod notes;
mod profile_storage;
mod project_collections;
mod project_library;
mod screen_recording;
mod settings;
#[cfg(feature = "unity")]
mod unity;
#[cfg(feature = "unity")]
mod unity_agent;
#[cfg(feature = "unity")]
mod unity_rename_tool;
#[cfg(feature = "unreal")]
mod unreal;
#[cfg(feature = "unreal")]
mod unreal_actor_tool;
#[cfg(feature = "unreal")]
mod unreal_agent;
#[cfg(feature = "unreal")]
mod unreal_approval;
#[cfg(any(feature = "unity", feature = "godot"))]
mod write_approval;

pub use host::{read_host_status, Capability, EngineModules, HostStatus};

/// Register the deliberately small command surface on a Tauri runtime.
pub fn configure_commands<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    let builder = builder
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .manage(browser::BrowserHost::default())
        .manage(project_library::ProjectRegistrations::default())
        .manage(chat_history::ChatHistoryHost::default())
        .manage(legacy_history::LegacyHistoryHost::default())
        .manage(settings::ModelSettings::default())
        .manage(notes::NotesHost::default())
        .manage(asset_index::AssetIndexHost::default())
        .manage(mcp::McpHost::default())
        .manage(desktop::DesktopState::default())
        .on_menu_event(desktop::menu_event)
        .on_window_event(desktop::window_event);
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    let builder = builder.manage(desktop_shortcut::ShortcutPreference::default());
    #[cfg(feature = "godot")]
    let builder = builder
        .manage(godot::GodotSessions::default())
        .on_window_event(|window, event| {
            use tauri::Manager;
            if matches!(event, tauri::WindowEvent::Destroyed) {
                window
                    .state::<godot::GodotSessions>()
                    .destroy_window(window.label());
            }
        });
    #[cfg(feature = "unity")]
    let builder = builder
        .manage(unity::UnityQueries::default())
        .on_window_event(|window, event| {
            use tauri::Manager;
            if matches!(event, tauri::WindowEvent::Destroyed) {
                window
                    .state::<unity::UnityQueries>()
                    .agent
                    .destroy_window(window.label());
            }
        });
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
    #[cfg(all(
        any(target_os = "macos", target_os = "linux", windows),
        any(feature = "unreal", feature = "unity", feature = "godot")
    ))]
    let builder = builder.manage(native_cli::NativeCli::default());
    builder.invoke_handler(desktop_browser::restrict_invocation::<R>(
        tauri::generate_handler![
            host::host_status,
            desktop::desktop_tray,
            #[cfg(any(target_os = "macos", target_os = "linux", windows))]
            desktop_shortcut::desktop_window_shortcut,
            #[cfg(any(target_os = "macos", target_os = "linux", windows))]
            desktop_shortcut::set_desktop_window_shortcut,
            desktop_files::open_registered_project_folder,
            desktop_browser::open_isolated_browser,
            browser::open_browser_window,
            browser::isolated_browser_status,
            browser::control_isolated_browser,
            mcp::inspect_mcp_endpoint,
            mcp::list_mcp_endpoints,
            mcp::save_mcp_endpoint,
            mcp::remove_mcp_endpoint,
            mcp::inspect_saved_mcp_endpoint,
            asset_index::import_asset_index,
            asset_index::list_asset_vaults,
            asset_index::list_asset_folders,
            asset_index::list_asset_items,
            asset_index::reveal_asset_original,
            desktop::set_desktop_tray,
            screen_recording::screen_recording_status,
            screen_recording::start_screen_recording,
            screen_recording::stop_screen_recording,
            screen_recording::reveal_screen_recording,
            #[cfg(all(
                any(target_os = "macos", target_os = "linux", windows),
                any(feature = "unreal", feature = "unity", feature = "godot")
            ))]
            native_cli::native_cli_endpoint,
            #[cfg(all(
                any(target_os = "macos", target_os = "linux", windows),
                any(feature = "unreal", feature = "unity", feature = "godot")
            ))]
            native_cli::start_native_cli,
            #[cfg(all(
                any(target_os = "macos", target_os = "linux", windows),
                any(feature = "unreal", feature = "unity", feature = "godot")
            ))]
            native_cli::stop_native_cli,
            chat_history::list_agent_chats,
            chat_history::get_agent_chat,
            legacy_history::import_legacy_chat_history,
            legacy_history::list_legacy_chats,
            legacy_history::get_legacy_chat,
            legacy_history::read_legacy_chat_attachment,
            settings::get_model_settings,
            settings::save_model_profile,
            settings::remove_model_profile,
            settings::bind_model_profile,
            settings::transfer::import_model_profiles,
            notes::list_notebooks,
            notes::list_notes,
            notes::get_note,
            notes::save_notebook,
            notes::save_note,
            notes::remove_note,
            notes::remove_notebook,
            notes::transfer::import_legacy_notes,
            project_library::list_registered_projects,
            project_library::register_project,
            project_library::remove_registered_project,
            project_library::import_registered_projects,
            project_collections::list_project_collections,
            project_collections::create_project_collection,
            project_collections::rename_project_collection,
            project_collections::move_project_collection,
            project_collections::set_project_collection_membership,
            project_collections::remove_project_collection,
            project_collections::transfer::import_project_collections,
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
            godot_agent::create_godot_agent_from_profile,
            #[cfg(feature = "godot")]
            godot_agent::start_godot_agent,
            #[cfg(feature = "godot")]
            godot_agent::poll_godot_agent,
            #[cfg(feature = "godot")]
            godot_agent::cancel_godot_agent,
            #[cfg(feature = "godot")]
            godot_agent::close_godot_agent,
            #[cfg(feature = "godot")]
            godot_agent::list_godot_name_activity,
            #[cfg(feature = "godot")]
            godot_agent::answer_godot_name_approval,
            #[cfg(feature = "unity")]
            unity::inspect_unity,
            #[cfg(feature = "unity")]
            unity_agent::attach_unity_agent_target,
            #[cfg(feature = "unity")]
            unity_agent::release_unity_agent_target,
            #[cfg(feature = "unity")]
            unity_agent::create_unity_agent,
            #[cfg(feature = "unity")]
            unity_agent::create_unity_agent_from_profile,
            #[cfg(feature = "unity")]
            unity_agent::start_unity_agent,
            #[cfg(feature = "unity")]
            unity_agent::poll_unity_agent,
            #[cfg(feature = "unity")]
            unity_agent::cancel_unity_agent,
            #[cfg(feature = "unity")]
            unity_agent::close_unity_agent,
            #[cfg(feature = "unity")]
            unity_agent::list_unity_name_activity,
            #[cfg(feature = "unity")]
            unity_agent::answer_unity_name_approval,
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
            unreal_agent::create_unreal_agent_from_profile,
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
        ],
    ))
}

pub fn run() {
    let app = configure_commands(tauri::Builder::default())
        .setup(|app| {
            #[cfg(any(target_os = "macos", target_os = "linux", windows))]
            let _ = app
                .handle()
                .plugin(tauri_plugin_global_shortcut::Builder::new().build());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Tauri host");
    #[cfg(target_os = "macos")]
    let recording_exit_ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(target_os = "macos")]
    let mut stopping_recording = false;
    app.run(move |_handle, _event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::ExitRequested { api, .. } = &_event {
            use std::sync::atomic::Ordering;
            if !recording_exit_ready.load(Ordering::Acquire)
                && screen_recording::stop_for_shutdown()
            {
                api.prevent_exit();
                if !stopping_recording {
                    stopping_recording = true;
                    let app = _handle.clone();
                    let exit_ready = recording_exit_ready.clone();
                    tauri::async_runtime::spawn(async move {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(18);
                        let _ = tauri::async_runtime::spawn_blocking(move || {
                            while screen_recording::stop_for_shutdown()
                                && std::time::Instant::now() < deadline
                            {
                                std::thread::sleep(std::time::Duration::from_millis(125));
                            }
                        })
                        .await;
                        exit_ready.store(true, Ordering::Release);
                        app.exit(0);
                    });
                }
                return;
            }
        }
        #[cfg(target_os = "macos")]
        if matches!(
            _event,
            tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            }
        ) {
            desktop::reveal(_handle);
        }
        #[cfg(all(
            any(target_os = "macos", target_os = "linux", windows),
            any(feature = "unreal", feature = "unity", feature = "godot")
        ))]
        if matches!(_event, tauri::RunEvent::Exit) {
            let _ = tauri::async_runtime::block_on(native_cli::stop_native_cli(_handle.clone()));
        }
    });
}
