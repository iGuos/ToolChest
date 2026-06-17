mod tools;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            tools::port::list_ports,
            tools::port::kill_process,
            tools::hosts::read_hosts,
            tools::hosts::write_hosts,
            tools::http::http_send,
            tools::trust_app::pick_app,
            tools::trust_app::trust_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
