use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use std::process::Command;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PortInfo {
    pub command: String,
    pub pid: u32,
    pub user: String,
    pub protocol: String,
    pub local_addr: String,
    pub local_port: String,
    pub remote_addr: Option<String>,
    pub remote_port: Option<String>,
    pub state: Option<String>,
    pub fd_type: String,
}

#[tauri::command]
pub async fn list_ports() -> Result<Vec<PortInfo>, String> {
    // lsof 可能跑几百 ms，放到阻塞线程池，避免冻结主线程（UI）。
    tauri::async_runtime::spawn_blocking(collect_ports)
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

fn collect_ports() -> Result<Vec<PortInfo>, String> {
    #[cfg(windows)]
    {
        collect_ports_windows()
    }
    #[cfg(not(windows))]
    {
        collect_ports_unix()
    }
}

/// Windows：netstat -ano 列连接，tasklist 映射 PID→进程名。
#[cfg(windows)]
fn collect_ports_windows() -> Result<Vec<PortInfo>, String> {
    use std::collections::HashMap;
    // PID -> 进程名（tasklist CSV）
    let names: HashMap<u32, String> = {
        let mut m = HashMap::new();
        if let Ok(o) = crate::tools::hidden_command("tasklist").args(["/fo", "csv", "/nh"]).output() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let cols: Vec<String> = line.split("\",\"").map(|s| s.trim_matches('"').trim().to_string()).collect();
                if cols.len() >= 2 {
                    if let Ok(pid) = cols[1].parse::<u32>() {
                        m.insert(pid, cols[0].clone());
                    }
                }
            }
        }
        m
    };
    let output = crate::tools::hidden_command("netstat")
        .args(["-ano"])
        .output()
        .map_err(|e| format!("无法运行 netstat: {e}"))?;
    if !output.status.success() {
        return Err(format!("netstat 失败: {}", String::from_utf8_lossy(&output.stderr)));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ports = Vec::new();
    for line in stdout.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.is_empty() {
            continue;
        }
        let proto = p[0].to_uppercase();
        if (proto != "TCP" && proto != "UDP") || p.len() < 4 {
            continue;
        }
        let local = p[1];
        let foreign = p[2];
        // TCP: proto local foreign STATE pid；UDP: proto local foreign pid（无 state）
        let (state, pid_str) = if proto == "TCP" {
            if p.len() < 5 {
                continue;
            }
            (Some(p[3].to_string()), p[4])
        } else {
            (None, p[3])
        };
        let pid: u32 = pid_str.parse().unwrap_or(0);
        let (local_addr, local_port) = split_host_port(local);
        let (remote_addr, remote_port) = if foreign.contains('*') || foreign.ends_with(":0") {
            (None, None)
        } else {
            let (a, b) = split_host_port(foreign);
            (Some(a), Some(b))
        };
        let fd_type = if local.contains('[') || local.contains("::") { "IPv6" } else { "IPv4" }.to_string();
        ports.push(PortInfo {
            command: names.get(&pid).cloned().unwrap_or_else(|| "—".into()),
            pid,
            user: String::new(),
            protocol: proto,
            local_addr,
            local_port,
            remote_addr,
            remote_port,
            state,
            fd_type,
        });
    }
    Ok(ports)
}

#[cfg(not(windows))]
fn collect_ports_unix() -> Result<Vec<PortInfo>, String> {
    let output = Command::new("lsof")
        .args(["-i", "-P", "-n"])
        .output()
        .map_err(|e| format!("无法运行 lsof: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("lsof 失败: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ports = Vec::new();

    for line in stdout.lines().skip(1) {
        if let Some(info) = parse_lsof_line(line) {
            ports.push(info);
        }
    }

    Ok(ports)
}

#[cfg(not(windows))]
fn parse_lsof_line(line: &str) -> Option<PortInfo> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 9 {
        return None;
    }

    let command = parts[0].to_string();
    let pid: u32 = parts[1].parse().ok()?;
    let user = parts[2].to_string();
    let fd_type = parts[4].to_string();

    // Find TCP/UDP column (usually index 7, but search to be robust)
    let proto_idx = parts[5..]
        .iter()
        .position(|&p| p == "TCP" || p == "UDP")
        .map(|i| i + 5)?;

    let protocol = parts[proto_idx].to_string();

    if proto_idx + 1 >= parts.len() {
        return None;
    }

    let name = parts[proto_idx + 1..].join(" ");
    let (connection_str, state) = extract_state(&name);
    let (local_str, remote_str) = split_connection(&connection_str);
    let (local_addr, local_port) = split_host_port(&local_str);

    let (remote_addr, remote_port) = match remote_str {
        Some(r) => {
            let (a, p) = split_host_port(&r);
            (Some(a), Some(p))
        }
        None => (None, None),
    };

    Some(PortInfo {
        command,
        pid,
        user,
        protocol,
        local_addr,
        local_port,
        remote_addr,
        remote_port,
        state,
        fd_type,
    })
}

#[cfg(not(windows))]
fn extract_state(name: &str) -> (String, Option<String>) {
    if let Some(start) = name.rfind(" (") {
        if name.ends_with(')') {
            let state = name[start + 2..name.len() - 1].to_string();
            let conn = name[..start].to_string();
            return (conn, Some(state));
        }
    }
    (name.to_string(), None)
}

#[cfg(not(windows))]
fn split_connection(s: &str) -> (String, Option<String>) {
    if let Some(idx) = s.find("->") {
        return (s[..idx].to_string(), Some(s[idx + 2..].to_string()));
    }
    (s.to_string(), None)
}

fn split_host_port(addr: &str) -> (String, String) {
    // IPv6 like [::1]:port
    if addr.starts_with('[') {
        if let Some(bracket_end) = addr.find(']') {
            let host = addr[1..bracket_end].to_string();
            let port = if addr.len() > bracket_end + 2 {
                addr[bracket_end + 2..].to_string()
            } else {
                String::new()
            };
            return (host, port);
        }
    }
    // IPv4 / hostname
    if let Some(colon) = addr.rfind(':') {
        let host = addr[..colon].to_string();
        let port = addr[colon + 1..].to_string();
        (host, port)
    } else {
        (addr.to_string(), String::new())
    }
}

#[tauri::command]
pub async fn kill_process(pid: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || terminate(pid))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

fn terminate(pid: u32) -> Result<(), String> {
    let pid_s = pid.to_string();

    #[cfg(unix)]
    {
        // 先发 SIGTERM 让进程优雅退出（有机会清理资源），
        // 等一会儿仍存活才 SIGKILL 强杀。
        let _ = Command::new("kill").args(["-TERM", &pid_s]).status();
        std::thread::sleep(std::time::Duration::from_millis(300));
        // `kill -0` 仅探测进程是否还在
        let alive = Command::new("kill")
            .args(["-0", &pid_s])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if alive {
            let status = Command::new("kill")
                .args(["-9", &pid_s])
                .status()
                .map_err(|e| format!("执行 kill 失败: {e}"))?;
            if !status.success() {
                return Err(format!("终止进程 {pid} 失败"));
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        let status = crate::tools::hidden_command("taskkill")
            .args(["/F", "/PID", &pid_s])
            .status()
            .map_err(|e| format!("执行 taskkill 失败: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("终止进程 {pid} 失败"))
        }
    }
}
