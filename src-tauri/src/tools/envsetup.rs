//! 移动端打包「环境配置」：检测 iOS / Android 工具链是否就绪，并对安全项提供一键安装。
//! 重型/带交互的安装（Xcode、Android Studio、NDK）只给命令或下载链接，由前端复制/打开。
use serde::Serialize;
use std::process::Command;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvItem {
    key: String,
    name: String,
    ok: bool,
    detail: String,
    /// 修复方式：none / auto(后端可一键执行) / copy(复制命令到终端跑) / link(打开网址)
    fix_kind: String,
    fix_value: String,
    fix_label: String,
}

fn item(
    key: &str,
    name: &str,
    ok: bool,
    detail: String,
    fix: Option<(&str, &str, &str)>, // (kind, value, label)
) -> EnvItem {
    let (k, v, l) = fix.unwrap_or(("none", "", ""));
    EnvItem {
        key: key.into(),
        name: name.into(),
        ok,
        detail,
        fix_kind: k.into(),
        fix_value: v.into(),
        fix_label: l.into(),
    }
}

/// GUI（Finder/Dock）启动的 App 继承的 PATH 极简，常找不到 rustup/brew/pod/adb。
/// 这里把常见安装目录补进 PATH，保证检测与安装命令能定位到这些工具。
#[cfg(not(windows))]
fn augmented_path() -> String {
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(format!("{home}/.cargo/bin")); // rustup / cargo
        dirs.push(format!("{home}/Library/Android/sdk/platform-tools")); // adb
        dirs.push(format!("{home}/.gem/bin")); // cocoapods(gem 安装时)
    }
    for k in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(v) = std::env::var(k) {
            dirs.push(format!("{v}/platform-tools"));
        }
    }
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
            "/usr/local/bin",
            "/usr/local/sbin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    if let Ok(p) = std::env::var("PATH") {
        dirs.push(p);
    }
    dirs.join(":")
}

#[cfg(windows)]
fn augmented_path() -> String {
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(up) = std::env::var("USERPROFILE") {
        dirs.push(format!("{up}\\.cargo\\bin"));
    }
    for k in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(v) = std::env::var(k) {
            dirs.push(format!("{v}\\platform-tools"));
        }
    }
    if let Ok(p) = std::env::var("PATH") {
        dirs.push(p);
    }
    dirs.join(";")
}

/// 跑一个命令并取首个非空输出行；命令不存在返回 None。
fn probe(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).env("PATH", augmented_path()).args(args).output().ok()?;
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    let line = so
        .lines()
        .chain(se.lines())
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    Some(line)
}

fn rustup_targets() -> String {
    Command::new("rustup")
        .env("PATH", augmented_path())
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Android SDK 目录（ANDROID_HOME / ANDROID_SDK_ROOT）。
fn android_sdk() -> Option<std::path::PathBuf> {
    for k in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(v) = std::env::var(k) {
            let p = std::path::PathBuf::from(v);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    None
}

#[tauri::command]
pub async fn env_check(platform: String) -> Vec<EnvItem> {
    tauri::async_runtime::spawn_blocking(move || env_check_blocking(&platform))
        .await
        .unwrap_or_default()
}

fn env_check_blocking(platform: &str) -> Vec<EnvItem> {
    let targets = rustup_targets();
    let mut v = Vec::new();

    // 通用：Rust 工具链
    let rustc = probe("rustc", &["--version"]);
    v.push(item(
        "rust",
        "Rust 工具链",
        rustc.is_some(),
        rustc.clone().unwrap_or_else(|| "未安装".into()),
        rustc.is_none().then_some(("link", "https://rustup.rs", "安装 Rust")),
    ));

    if platform == "ios" {
        // Xcode（含命令行工具）
        let xc = probe("xcodebuild", &["-version"]);
        v.push(item(
            "xcode",
            "Xcode",
            xc.is_some(),
            xc.unwrap_or_else(|| "未安装（请从 App Store 安装 Xcode）".into()),
            Some(("link", "https://apps.apple.com/app/xcode/id497799835", "去 App Store 安装")),
        ));
        // 命令行工具
        let clt = probe("xcode-select", &["-p"]);
        v.push(item(
            "xcode-clt",
            "Xcode 命令行工具",
            clt.as_deref().map(|s| !s.is_empty()).unwrap_or(false),
            clt.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "未安装".into()),
            clt.as_deref().filter(|s| !s.is_empty()).is_none().then_some((
                "auto",
                "xcode-select --install",
                "一键安装",
            )),
        ));
        // CocoaPods
        let pod = probe("pod", &["--version"]);
        v.push(item(
            "cocoapods",
            "CocoaPods",
            pod.is_some(),
            pod.map(|s| format!("v{s}")).unwrap_or_else(|| "未安装".into()),
            Some(("auto", "brew install cocoapods", "一键安装(需 Homebrew)")),
        ));
        // Rust iOS targets
        let ok = targets.contains("aarch64-apple-ios");
        v.push(item(
            "rust-ios",
            "Rust iOS 目标",
            ok,
            if ok { "已安装".into() } else { "缺 aarch64-apple-ios".into() },
            (!ok).then_some((
                "auto",
                "rustup target add aarch64-apple-ios aarch64-apple-ios-sim",
                "一键安装",
            )),
        ));
    } else {
        // Android：JDK
        let jdk = probe("java", &["-version"]);
        v.push(item(
            "jdk",
            "JDK (Java)",
            jdk.is_some(),
            jdk.unwrap_or_else(|| "未安装（Android Studio 自带 JDK）".into()),
            None,
        ));
        // Android SDK
        let sdk = android_sdk();
        v.push(item(
            "android-sdk",
            "Android SDK",
            sdk.is_some(),
            sdk.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "未设置 ANDROID_HOME".into()),
            sdk.is_none().then_some((
                "link",
                "https://developer.android.com/studio",
                "下载 Android Studio",
            )),
        ));
        // NDK
        let ndk = std::env::var("NDK_HOME").ok().filter(|s| std::path::Path::new(s).is_dir()).is_some()
            || sdk.as_ref().map(|p| p.join("ndk").is_dir()).unwrap_or(false);
        v.push(item(
            "ndk",
            "Android NDK",
            ndk,
            if ndk { "已安装".into() } else { "未安装（Android Studio → SDK Manager 装 NDK）".into() },
            (!ndk).then_some(("copy", "在 Android Studio 的 SDK Manager 勾选 NDK (Side by side)", "查看安装说明")),
        ));
        // adb / platform-tools
        let adb = probe("adb", &["version"]);
        v.push(item(
            "adb",
            "adb (platform-tools)",
            adb.is_some(),
            adb.unwrap_or_else(|| "未安装或不在 PATH".into()),
            None,
        ));
        // Rust android targets
        let ok = targets.contains("aarch64-linux-android");
        v.push(item(
            "rust-android",
            "Rust Android 目标",
            ok,
            if ok { "已安装".into() } else { "缺 aarch64-linux-android 等".into() },
            (!ok).then_some((
                "auto",
                "rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android",
                "一键安装",
            )),
        ));
    }
    v
}

/// 用系统默认浏览器打开下载/文档链接（环境配置里的「打开网址」）。
#[tauri::command]
pub fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("打开失败：{e}"))
}

/// 对「auto」类项执行安装（按 key 白名单，不接收任意命令，防注入）。
#[tauri::command]
pub async fn env_fix(key: String) -> Result<String, String> {
    let argv: Vec<&str> = match key.as_str() {
        "xcode-clt" => vec!["xcode-select", "--install"],
        "cocoapods" => vec!["brew", "install", "cocoapods"],
        "rust-ios" => vec!["rustup", "target", "add", "aarch64-apple-ios", "aarch64-apple-ios-sim"],
        "rust-android" => vec![
            "rustup",
            "target",
            "add",
            "aarch64-linux-android",
            "armv7-linux-androideabi",
            "i686-linux-android",
            "x86_64-linux-android",
        ],
        _ => return Err("该项需手动安装".into()),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let out = Command::new(argv[0])
            .env("PATH", augmented_path())
            .args(&argv[1..])
            .output()
            .map_err(|e| format!("执行失败（命令不存在？）：{e}"))?;
        let so = String::from_utf8_lossy(&out.stdout);
        let se = String::from_utf8_lossy(&out.stderr);
        let log = format!("{so}{se}");
        if out.status.success() {
            Ok(if log.trim().is_empty() { "完成".into() } else { log })
        } else {
            Err(if log.trim().is_empty() { "安装失败".into() } else { log })
        }
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}
