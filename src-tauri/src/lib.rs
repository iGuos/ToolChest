mod tools;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(tools::lan::LanState::default())
        .invoke_handler(tauri::generate_handler![
            tools::port::list_ports,
            tools::port::kill_process,
            tools::hosts::read_hosts,
            tools::hosts::write_hosts,
            tools::http::http_send,
            tools::trust_app::pick_app,
            tools::trust_app::trust_app,
            tools::lan::lan_start,
            tools::lan::lan_my_info,
            tools::lan::lan_peers,
            tools::lan::lan_set_alias,
            tools::lan::lan_set_compat,
            tools::lan::lan_set_dir,
            tools::lan::lan_cancel,
            tools::lan::lan_cancel_send,
            tools::lan::lan_add_peer,
            tools::lan::lan_respond,
            tools::lan::lan_pick_files,
            tools::lan::lan_pick_dir,
            tools::lan::lan_reveal,
            tools::lan::lan_send_message,
            tools::lan::lan_send_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
