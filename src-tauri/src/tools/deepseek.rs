use tauri::webview::PageLoadEvent;
use tauri::window::Color;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// 把 "#rrggbb" / "#rgb" 解析成窗口背景色（失败返回 None）。
fn parse_hex_color(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    let (r, g, b) = match h.len() {
        6 => (
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        ),
        3 => {
            let d = |c: &str| u8::from_str_radix(c, 16).ok().map(|v| v * 17);
            (d(&h[0..1])?, d(&h[1..2])?, d(&h[2..3])?)
        }
        _ => return None,
    };
    Some(Color(r, g, b, 255))
}

/// 打开（或前置）一个 DeepSeek 站点窗口。
/// 关键点：先以 `visible(false)` 建窗 + 设深/浅主题背景色，等页面首次加载完成再 `show()`，
/// 以此消除 macOS WKWebView 在远程页面渲染前的白屏闪烁。
#[tauri::command]
pub async fn open_deepseek(
    app: AppHandle,
    label: String,
    url: String,
    title: String,
    width: f64,
    height: f64,
    x: Option<f64>,
    y: Option<f64>,
    bg: Option<String>,
) -> Result<(), String> {
    // 已存在：仅当它已可见时才前置；若仍隐藏（首次加载中）则不打扰，
    // 交给 on_page_load 加载完自行显示，避免连点把未加载完的窗口提前显示出来（白屏）。
    if let Some(w) = app.get_webview_window(&label) {
        if w.is_visible().unwrap_or(true) {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
        return Ok(());
    }

    let parsed = url.parse().map_err(|e| format!("非法 URL：{e}"))?;
    let mut builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed))
        .title(&title)
        .inner_size(width, height)
        .visible(false); // 先隐藏，加载完成再显示
    if let (Some(x), Some(y)) = (x, y) {
        builder = builder.position(x, y);
    } else {
        builder = builder.center();
    }
    if let Some(c) = bg.as_deref().and_then(parse_hex_color) {
        builder = builder.background_color(c); // 同时作用于窗口和 webview，盖住首帧白屏
    }
    // 只在「首次」页面加载完成时显示一次：站内后续跳转/重定向也会触发 Finished，
    // 若每次都 show+focus，会在用户切走后被反复抢到最前。用一次性标志保证只显示一次。
    let shown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shown_load = shown.clone();
    let builder = builder.on_page_load(move |win, payload| {
        if matches!(payload.event(), PageLoadEvent::Finished)
            && !shown_load.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let _ = win.show();
            let _ = win.set_focus();
        }
    });

    let win = builder.build().map_err(|e| format!("打开 DeepSeek 窗口失败：{e}"))?;

    // 兜底：万一页面迟迟不触发 Finished（网络异常等），4 秒后也显示一次（与首次显示二选一）。
    let fallback = win.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(4));
        if !shown.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let _ = fallback.show();
            let _ = fallback.set_focus();
        }
    });
    Ok(())
}
