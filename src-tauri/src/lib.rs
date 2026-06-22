mod tools;

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

/// 应用级配置：关闭按钮行为。close_to_tray=true 关到托盘后台常驻，false 直接退出。
/// 由前端「设置」推送，Rust 在窗口关闭事件时读取。
struct AppConfig {
    close_to_tray: AtomicBool,
}

#[tauri::command]
fn set_close_to_tray(state: tauri::State<'_, AppConfig>, enabled: bool) {
    state.close_to_tray.store(enabled, Ordering::Relaxed);
}

/// 显示并聚焦主窗口（从托盘恢复）。
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

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
        // 默认「关到托盘」（LAN 互传/代理等需后台常驻）；前端设置可改为直接退出
        .manage(AppConfig {
            close_to_tray: AtomicBool::new(true),
        })
        .manage(tools::lan::LanState::default())
        .invoke_handler(tauri::generate_handler![
            tools::port::list_ports,
            tools::port::kill_process,
            tools::hosts::read_hosts,
            tools::hosts::write_hosts,
            tools::http::http_send,
            tools::trust_app::pick_app,
            tools::trust_app::trust_app,
            tools::deepseek::open_deepseek,
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
            tools::lan::lan_share_upload_cancel,
            tools::lan::lan_share_receive_reject,
            tools::lan::lan_dir_flags,
            tools::lan::lan_share_op,
            tools::lan::lan_respond,
            tools::lan::lan_pick_files,
            tools::lan::lan_pick_dir,
            tools::lan::lan_reveal,
            tools::lan::lan_open_path,
            tools::lan::lan_proxy_status,
            tools::lan::lan_proxy_stop,
            tools::lan::lan_proxy_start_server,
            tools::lan::lan_proxy_start_client,
            tools::lan::lan_proxy_test,
            tools::lan::lan_set_system_proxy,
            tools::lan::lan_proxy_pick_app,
            tools::lan::lan_proxy_launch_app,
            tools::lan::lan_scan_subnet,
            tools::lan::lan_send_message,
            tools::lan::lan_recall_message,
            tools::lan::lan_send_files,
            set_close_to_tray,
        ])
        .setup(|app| {
            // 系统托盘：左键点图标恢复窗口；右键菜单可显示/退出
            let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出百宝箱", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut builder = TrayIconBuilder::new()
                .tooltip("百宝箱")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon().cloned() {
                builder = builder.icon(icon);
            }
            builder.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // 主窗口点关闭：按设置决定「关到托盘（隐藏）」还是「直接退出」
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main"
                    && window.state::<AppConfig>().close_to_tray.load(Ordering::Relaxed)
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, _event| {
            // macOS：点击 Dock 图标（窗口已隐藏到托盘时）恢复窗口
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                show_main(_app);
            }
        });
}
