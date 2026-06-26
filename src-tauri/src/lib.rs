mod tools;

use std::sync::atomic::{AtomicBool, Ordering};

/// 应用级配置：关闭按钮行为。close_to_tray=true 关到托盘后台常驻，false 直接退出。
/// 由前端「设置」推送，Rust 在窗口关闭事件时读取（仅桌面有窗口关闭/托盘概念）。
struct AppConfig {
    #[allow(dead_code)] // 移动端无窗口关闭事件，不读取此字段
    close_to_tray: AtomicBool,
}

#[tauri::command]
fn set_close_to_tray(state: tauri::State<'_, AppConfig>, enabled: bool) {
    state.close_to_tray.store(enabled, Ordering::Relaxed);
}

/// 软件详情：名称/版本/包标识 + iOS 签名描述文件到期日（开发签名约 1 年）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    name: String,
    version: String,
    identifier: String,
    /// 描述文件到期时间（ISO8601）；非 iOS 开发包为空。
    expiration: Option<String>,
}

/// 读 app 包内 embedded.mobileprovision 的 ExpirationDate。
/// .mobileprovision 是 CMS 签名块，内部 plist 以明文嵌着，直接截取 <plist>…</plist> 再取日期即可。
fn provision_expiration() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let data = std::fs::read(exe.parent()?.join("embedded.mobileprovision")).ok()?;
    let text = String::from_utf8_lossy(&data);
    let plist = {
        let s = text.find("<plist")?;
        let e = text.find("</plist>")? + "</plist>".len();
        &text[s..e]
    };
    let key = "<key>ExpirationDate</key>";
    let after = &plist[plist.find(key)? + key.len()..];
    let ds = after.find("<date>")? + "<date>".len();
    let de = after.find("</date>")?;
    Some(after[ds..de].trim().to_string())
}

#[tauri::command]
fn app_info(app: tauri::AppHandle) -> AppInfo {
    let pkg = app.package_info();
    AppInfo {
        name: "百宝箱".into(),
        version: pkg.version.to_string(),
        identifier: app.config().identifier.clone(),
        expiration: provision_expiration(),
    }
}

/// 显示并聚焦主窗口（从托盘恢复）。仅桌面有托盘/多窗口。
#[cfg(desktop)]
fn show_main(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 桌面系统托盘：左键恢复窗口，右键菜单显示/退出。
#[cfg(desktop)]
fn setup_desktop_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::{
        menu::{Menu, MenuItem},
        tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    };
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出百宝箱", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tb = TrayIconBuilder::new()
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
        tb = tb.icon(icon);
    }
    tb.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
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
            tools::envsetup::env_check,
            tools::envsetup::env_fix,
            tools::envsetup::open_external,
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
            tools::lan::lan_proxy_hosts,
            tools::lan::lan_proxy_stop,
            tools::lan::lan_proxy_start_server,
            tools::lan::lan_proxy_start_client,
            tools::lan::lan_proxy_test,
            tools::lan::lan_set_system_proxy,
            tools::lan::lan_proxy_leftover,
            tools::lan::lan_proxy_pick_app,
            tools::lan::lan_proxy_launch_app,
            tools::lan::lan_proxy_running_apps,
            tools::lan::lan_scan_subnet,
            tools::lan::lan_scan_cancel,
            tools::lan::lan_send_message,
            tools::lan::lan_recall_message,
            tools::lan::lan_send_files,
            set_close_to_tray,
            app_info,
        ])
        // 统一启动初始化（所有平台）：注入可写配置目录 + 桌面托盘。
        .setup(|app| {
            use tauri::Manager;
            if let Ok(dir) = app.path().app_data_dir() {
                tools::lan::init_config_dir(dir);
            }
            #[cfg(desktop)]
            setup_desktop_tray(app)?;
            Ok(())
        });

    // 桌面专属：开机自启 + 「关到托盘」。iOS/Android 无这些概念，跳过。
    #[cfg(desktop)]
    {
        use tauri::{Manager, WindowEvent};
        builder = builder
            // 开机自启动：macOS 用 LaunchAgent，Windows 用注册表，跨平台统一 API
            .plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ))
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
            });
    }

    builder
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
