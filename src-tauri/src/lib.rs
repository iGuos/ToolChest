mod tools;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // 开机自启动：macOS 用 LaunchAgent，Windows 用注册表，跨平台统一 API
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
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
            tools::lan::lan_set_invisible,
            tools::lan::lan_set_dir,
            tools::lan::lan_cancel,
            tools::lan::lan_cancel_send,
            tools::lan::lan_add_peer,
            tools::lan::lan_interfaces,
            tools::lan::lan_overlay_routes,
            tools::lan::lan_list_shares,
            tools::lan::lan_add_share,
            tools::lan::lan_remove_share,
            tools::lan::lan_set_share_password,
            tools::lan::lan_set_share_perms,
            tools::lan::lan_share_roots,
            tools::lan::lan_share_list,
            tools::lan::lan_share_download,
            tools::lan::lan_share_upload,
            tools::lan::lan_share_op,
            tools::lan::lan_respond,
            tools::lan::lan_pick_files,
            tools::lan::lan_pick_dir,
            tools::lan::lan_reveal,
            tools::lan::lan_send_message,
            tools::lan::lan_recall_message,
            tools::lan::lan_send_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
