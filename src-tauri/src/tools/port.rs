use serde::{Deserialize, Serialize};
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
pub fn list_ports() -> Result<Vec<PortInfo>, String> {
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
pub fn kill_process(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .map_err(|e| format!("执行 kill 失败: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("终止进程 {pid} 失败"))
        }
    }

    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
            .map_err(|e| format!("执行 taskkill 失败: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("终止进程 {pid} 失败"))
        }
    }
}
