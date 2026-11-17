#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A headless diagnostic exercises the same function as the registered command.
    if std::env::args().nth(1).as_deref() == Some("--host-status-json") {
        serde_json::to_writer(
            std::io::stdout().lock(),
            &uebox_tauri_lib::read_host_status(),
        )
        .expect("failed to serialize host status");
        println!();
        return;
    }

    uebox_tauri_lib::run();
}
