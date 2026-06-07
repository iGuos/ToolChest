mod tools;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(tools::claude_watcher::WatcherProc(std::sync::Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            tools::port::list_ports,
            tools::port::kill_process,
            tools::claude_watcher::check_accessibility,
            tools::claude_watcher::check_claude_dialogs,
            tools::claude_watcher::open_accessibility_settings,
            tools::claude_watcher::dump_claude_buttons,
            tools::claude_watcher::get_binary_path,
            tools::claude_watcher::start_watcher,
            tools::claude_watcher::stop_watcher,
            tools::claude_watcher::read_watcher_clicks,
            tools::hosts::read_hosts,
            tools::hosts::write_hosts,
            tools::http::http_send,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
