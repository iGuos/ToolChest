use std::process::Command;

/// 弹出原生选择器让用户挑一个 .app（用 osascript，无需额外依赖）。
/// `of type {"com.apple.application-bundle"}` 让 .app 这种「包」能被当作文件选中，
/// 而不是被当成目录点进去。用户取消时返回 None。
#[tauri::command]
pub async fn pick_app() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let script = r#"POSIX path of (choose file with prompt "选择要信任的 App" of type {"com.apple.application-bundle"} default location (path to applications folder))"#;
        let out = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .map_err(|e| format!("打开选择器失败: {e}"))?;
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Ok(if p.is_empty() { None } else { Some(p) })
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            // 用户取消（-128）属正常，不报错
            if err.contains("-128") || err.contains("User canceled") {
                Ok(None)
            } else {
                Err(format!("选择失败: {}", err.trim()))
            }
        }
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}

/// 信任一个 App：递归移除 Gatekeeper 的隔离标记（com.apple.quarantine）。
/// 用于打开「无法验证开发者」「已损坏，应移到废纸篓」的应用。
/// 整个过程一次管理员授权（系统密码框）完成。
#[tauri::command]
pub async fn trust_app(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || trust_app_blocking(path))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

fn trust_app_blocking(path: String) -> Result<String, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("未选择 App".into());
    }
    let p = std::path::Path::new(trimmed);
    if !p.exists() {
        return Err(format!("路径不存在：{trimmed}"));
    }
    if p.extension().and_then(|e| e.to_str()) != Some("app") {
        return Err("请选择 .app 应用".into());
    }
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("应用")
        .to_string();

    // 单引号转义，安全拼进 shell（路径可能含空格/引号）
    let safe = trimmed.replace('\'', r"'\''");
    // -r 递归整个 bundle；没有该属性时 xattr 会报错，用重定向 + `|| true` 容错。
    let sh = format!("/usr/bin/xattr -dr com.apple.quarantine '{safe}' 2>/dev/null || true\n");

    run_admin(&sh)?;
    Ok(format!("已信任「{name}」，现在可以正常打开了"))
}

/// 把命令写进临时脚本，osascript 只引用脚本路径——规避把含引号/换行的命令塞进
/// AppleScript 字符串的转义地狱。只弹一次系统授权框。
fn run_admin(shell_cmd: &str) -> Result<(), String> {
    let script_path = std::env::temp_dir().join("baibao_trust_run.sh");
    std::fs::write(&script_path, format!("#!/bin/sh\nset -e\n{shell_cmd}"))
        .map_err(|e| format!("写脚本失败: {e}"))?;

    let osa = format!(
        "do shell script \"/bin/sh '{}'\" with administrator privileges",
        script_path.to_string_lossy()
    );
    let out = Command::new("osascript")
        .arg("-e")
        .arg(&osa)
        .output()
        .map_err(|e| format!("提权执行失败: {e}"))?;

    let _ = std::fs::remove_file(&script_path);

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("-128") || err.contains("User canceled") {
            return Err("已取消授权".into());
        }
        return Err(format!("提权执行失败: {}", err.trim()));
    }
    Ok(())
}
