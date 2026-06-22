#[cfg(not(windows))]
use std::process::Command;

/// hosts 文件路径：Windows 在 %SystemRoot%\System32\drivers\etc\hosts，其余在 /etc/hosts。
fn hosts_path() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        std::path::PathBuf::from(root)
            .join("System32")
            .join("drivers")
            .join("etc")
            .join("hosts")
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from("/etc/hosts")
    }
}

/// 读取 hosts（全员可读，无需提权）。
#[tauri::command]
pub async fn read_hosts() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        std::fs::read_to_string(hosts_path()).map_err(|e| format!("读取 hosts 失败: {e}"))
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}

/// 写回 hosts：先备份、再覆盖、可选刷新 DNS，整个过程一次提权完成。
#[tauri::command]
pub async fn write_hosts(content: String, flush_dns: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || write_hosts_blocking(content, flush_dns))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

fn write_hosts_blocking(content: String, flush_dns: bool) -> Result<String, String> {
    #[cfg(windows)]
    {
        write_hosts_windows(content, flush_dns)
    }
    #[cfg(not(windows))]
    {
        write_hosts_unix(content, flush_dns)
    }
}

// ── macOS / Linux：osascript 提权（弹一次系统授权框）──
#[cfg(not(windows))]
fn write_hosts_unix(content: String, flush_dns: bool) -> Result<String, String> {
    // 1. 新内容先写到用户私有临时目录（macOS 的 $TMPDIR 是 700 权限，避免世界可写脚本被提权执行的风险）
    let dir = std::env::temp_dir();
    let new_file = dir.join("baibao_hosts.new");
    std::fs::write(&new_file, content.as_bytes()).map_err(|e| format!("写临时文件失败: {e}"))?;

    // 2. 组装一个 shell 脚本：备份(带时间戳) → 覆盖 → 可选刷新 DNS
    let mut sh = format!(
        "cp /etc/hosts \"/etc/hosts.baibao.$(date +%Y%m%d-%H%M%S).bak\"\ncp '{}' /etc/hosts\n",
        new_file.to_string_lossy()
    );
    if flush_dns {
        sh.push_str("dscacheutil -flushcache\nkillall -HUP mDNSResponder\n");
    }

    run_admin(&sh)?;
    let _ = std::fs::remove_file(&new_file);
    Ok("已保存（已自动备份原文件）".into())
}

/// 把 shell 命令写进临时脚本，osascript 只引用脚本路径——规避把含引号/换行的命令
/// 塞进 AppleScript 字符串的转义地狱，也避免命令被日志记录。只弹一次系统授权框。
#[cfg(not(windows))]
fn run_admin(shell_cmd: &str) -> Result<(), String> {
    let script_path = std::env::temp_dir().join("baibao_hosts_run.sh");
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

// ── Windows：写一个 .bat（备份→覆盖→可选刷新 DNS），用 UAC 提权运行（弹一次 UAC）──
#[cfg(windows)]
fn write_hosts_windows(content: String, flush_dns: bool) -> Result<String, String> {
    let dir = std::env::temp_dir();
    let new_file = dir.join("baibao_hosts.new");
    // Windows hosts 用 CRLF 换行
    let normalized = content.replace("\r\n", "\n").replace('\n', "\r\n");
    std::fs::write(&new_file, normalized.as_bytes()).map_err(|e| format!("写临时文件失败: {e}"))?;

    let hosts = hosts_path();
    let bat = dir.join("baibao_hosts.bat");
    let mut script = String::from("@echo off\r\n");
    script += &format!("copy /Y \"{0}\" \"{0}.baibao.bak\"\r\n", hosts.display());
    script += &format!("copy /Y \"{}\" \"{}\"\r\n", new_file.display(), hosts.display());
    if flush_dns {
        script += "ipconfig /flushdns\r\n";
    }
    std::fs::write(&bat, script).map_err(|e| format!("写脚本失败: {e}"))?;

    // Start-Process -Verb RunAs 触发 UAC；-Wait 等执行完；用户取消会使 powershell 非零退出
    let ps = format!(
        "Start-Process -FilePath cmd.exe -ArgumentList '/c \"{}\"' -Verb RunAs -Wait",
        bat.display()
    );
    let status = crate::tools::hidden_command("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps])
        .status()
        .map_err(|e| format!("提权执行失败: {e}"))?;

    let _ = std::fs::remove_file(&new_file);
    let _ = std::fs::remove_file(&bat);

    if status.success() {
        Ok("已保存（已自动备份原文件）".into())
    } else {
        Err("提权失败或已取消（需管理员权限写入 hosts）".into())
    }
}
