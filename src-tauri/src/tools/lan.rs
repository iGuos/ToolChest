// 局域网互传：设备发现（UDP 多播）+ 收件 HTTP 服务（tiny_http）+ 发送（reqwest）。
// wire 格式与 LocalSend v2 一致（HTTP 明文）：
//   POST /api/localsend/v2/register        交换设备信息
//   POST /api/localsend/v2/prepare-upload   发文件前握手，接收方需确认
//   POST /api/localsend/v2/upload           流式上传单个文件
//   GET  /api/localsend/v2/info             查询设备信息
// 百宝箱自有扩展：
//   POST /api/baibao/v1/message             即时文本消息（无需确认）
//   POST /api/baibao/v1/recall              撤回一条已发送的文本消息
// announce/info 里带 app:"baibao" 标记区分百宝箱设备；compat 关闭时只认百宝箱、
// 拒绝外部 LocalSend 设备的发送，开启后才与真正的 LocalSend 互通。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
const PORT: u16 = 53317;
const PROTOCOL_VERSION: &str = "2.1";
const ANNOUNCE_EVERY: Duration = Duration::from_secs(5);
// TTL 取得比 announce 间隔大得多（≈4-5 个周期），容忍多播偶发丢包，避免设备在线/离线反复闪烁
const PEER_TTL_MS: u128 = 22_000;

// 共享密码防暴力破解：同一来源 IP 在 AUTH_WINDOW_MS 内累计 AUTH_MAX_FAILS 次失败 → 锁定 AUTH_LOCKOUT_MS。
const AUTH_MAX_FAILS: u32 = 8;
const AUTH_WINDOW_MS: u128 = 60_000; // 失败计数窗口：1 分钟
const AUTH_LOCKOUT_MS: u128 = 300_000; // 锁定时长：5 分钟

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 随机十六进制串（指纹 / sessionId / token / fileId 用）。局域网明文场景够用。
fn rand_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    let _ = getrandom::getrandom(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 随机密码：大小写字母+数字（去掉易混的 0/O/1/l/I），区分大小写。
fn rand_password(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut buf = vec![0u8; len];
    let _ = getrandom::getrandom(&mut buf);
    buf.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

fn ext_mime(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    let m = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain",
        "json" => "application/json",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "doc" | "docx" => "application/msword",
        "xls" | "xlsx" => "application/vnd.ms-excel",
        _ => "application/octet-stream",
    };
    m.to_string()
}

// ── wire 数据结构 ─────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceInfo {
    alias: String,
    version: String,
    #[serde(default)]
    device_model: Option<String>,
    #[serde(default)]
    device_type: Option<String>,
    fingerprint: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    download: Option<bool>,
    #[serde(default)]
    announce: Option<bool>,
    /// 百宝箱标记；LocalSend 会忽略未知字段
    #[serde(default)]
    app: Option<String>,
    /// 对外共享目录数量（>0 表示该设备有共享，供对端展示标签）
    #[serde(default)]
    shares: Option<u32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    alias: String,
    fingerprint: String,
    ip: String,
    port: u16,
    protocol: String,
    device_type: Option<String>,
    is_baibao: bool,
    last_seen_ms: u128,
    shares: u32, // 对方对外共享的目录数量（0=无）
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileMeta {
    id: String,
    file_name: String,
    size: u64,
    file_type: String,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    preview: Option<String>,
    /// 发送顺序号（同一次发送内 0,1,2…）；接收端据此保证展示顺序与发送顺序一致
    #[serde(default)]
    order: Option<u32>,
    /// 同一次发送的批次标识，配合 order 在接收端排序
    #[serde(default)]
    batch: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareUploadRequest {
    info: DeviceInfo,
    files: HashMap<String, FileMeta>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareUploadResponse {
    session_id: String,
    files: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessagePayload {
    alias: String,
    fingerprint: String,
    text: String,
    #[serde(default)]
    msg_id: Option<String>, // 双方共享的消息 ID，用于撤回时定位对端的同一条消息
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecallPayload {
    fingerprint: String,
    msg_id: String,
}

// ── 共享状态 ─────────────────────────────────────────────────

struct Decision {
    accept: bool,
    file_ids: Vec<String>,
}

struct FileSlot {
    token: String,
    file_name: String,
    size: u64,
    sha256: Option<String>,
}

struct Session {
    peer: String, // 对端指纹（发送方）
    files: HashMap<String, FileSlot>,
}

/// 一个对外共享的本地目录。password 为明文（None=无密码）；需要展示给用户以便告知对端，
/// 鉴权时按需 sha256 比对（局域网共享场景，明文存储于本机配置可接受）。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareCfg {
    id: String,
    name: String,
    path: String,
    #[serde(default, alias = "passwordHash")]
    password: Option<String>,
    // 授予访问方的写权限（读取默认开放）。
    #[serde(default)]
    can_create: bool,
    #[serde(default)]
    can_modify: bool,
    #[serde(default)]
    can_delete: bool,
}

/// 某来源 IP 的共享密码失败记录（防暴力破解）。
#[derive(Default, Clone)]
struct AuthFail {
    count: u32,
    window_start_ms: u128,
    locked_until_ms: u128,
}

struct Inner {
    started: bool,
    alias: String,
    fingerprint: String,
    download_dir: PathBuf,
    compat: bool,
    invisible: bool, // 隐身：不对外广播/应答，其他设备看到本机为离线
    shares: Vec<ShareCfg>, // 对外共享的目录列表
    auth_fails: HashMap<String, AuthFail>, // 共享密码失败记录（按来源 IP），用于防暴力破解
    peers: HashMap<String, Peer>,
    decisions: HashMap<String, Sender<Decision>>,
    sessions: HashMap<String, Session>, // sessionId -> 会话
    cancelled: HashSet<String>,         // 已取消的 sessionId（收发双向）
    cancelled_sends: HashSet<String>,   // 发送方取消的 fileId（对方还没接受前就撤销发送）
    socket: Option<Arc<UdpSocket>>,
}

/// 用户主目录（跨平台：类 Unix 用 HOME，Windows 用 USERPROFILE）。
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// 读取操作系统的稳定机器标识（不直接对外暴露，仅用于派生设备 ID）。
#[cfg(target_os = "macos")]
fn machine_id() -> Option<String> {
    let out = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if line.contains("IOPlatformUUID") {
            if let Some(eq) = line.find('=') {
                let v = line[eq + 1..].trim().trim_matches('"').to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn machine_id() -> Option<String> {
    let out = std::process::Command::new("reg")
        .args([
            "query",
            "HKLM\\SOFTWARE\\Microsoft\\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if line.contains("MachineGuid") {
            if let Some(v) = line.split_whitespace().last() {
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn machine_id() -> Option<String> {
    std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 由机器标识派生稳定的设备指纹：sha256 后取前 32 位十六进制（不直接暴露原始机器 UUID）。
fn machine_fingerprint() -> Option<String> {
    let id = machine_id()?;
    Some(sha256_hex(id.trim()).chars().take(32).collect())
}

impl Default for Inner {
    fn default() -> Self {
        let home = home_dir();
        // 从配置文件读回别名/指纹/兼容/接收目录，保证重启后稳定
        let cfg = load_config();
        let alias = cfg.alias.filter(|s| !s.is_empty()).unwrap_or_else(default_alias);
        // 设备唯一 ID（指纹）：优先用已持久化的；否则用「机器稳定标识」派生（跨重启/换 IP/重装都不变，
        // 避免同一台设备被识别成多个）；都拿不到才退回随机串。
        let fingerprint = cfg
            .fingerprint
            .filter(|s| !s.is_empty())
            .or_else(machine_fingerprint)
            .unwrap_or_else(|| rand_hex(16));
        let compat = cfg.compat.unwrap_or(false);
        let invisible = cfg.invisible.unwrap_or(false);
        let download_dir = cfg
            .download_dir
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Downloads"));
        let shares = cfg.shares.unwrap_or_default();
        let inner = Inner {
            started: false,
            alias,
            fingerprint,
            download_dir,
            compat,
            invisible,
            shares,
            auth_fails: HashMap::new(),
            peers: HashMap::new(),
            decisions: HashMap::new(),
            sessions: HashMap::new(),
            cancelled: HashSet::new(),
            cancelled_sends: HashSet::new(),
            socket: None,
        };
        // 落盘一次，确保（尤其是随机指纹）下次启动可复用
        persist(&inner);
        inner
    }
}

// ── 配置持久化（别名/指纹/兼容/接收目录）─────────────────────
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LanConfig {
    alias: Option<String>,
    fingerprint: Option<String>,
    compat: Option<bool>,
    invisible: Option<bool>,
    download_dir: Option<String>,
    #[serde(default)]
    shares: Option<Vec<ShareCfg>>,
}

fn config_path() -> Option<PathBuf> {
    // 跨平台配置位置：macOS 用 Application Support，Windows 用 %APPDATA%，其余用 ~/.config
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join("Library/Application Support/com.baibao.toolbox/lan.json"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("APPDATA").ok()?;
        Some(PathBuf::from(base).join("com.baibao.toolbox").join("lan.json"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".config/com.baibao.toolbox/lan.json"))
    }
}

fn load_config() -> LanConfig {
    config_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &LanConfig) {
    if let Some(p) = config_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_vec_pretty(cfg) {
            let _ = std::fs::write(p, s);
        }
    }
}

fn persist(g: &Inner) {
    save_config(&LanConfig {
        alias: Some(g.alias.clone()),
        fingerprint: Some(g.fingerprint.clone()),
        compat: Some(g.compat),
        invisible: Some(g.invisible),
        download_dir: Some(g.download_dir.to_string_lossy().to_string()),
        shares: Some(g.shares.clone()),
    });
}

#[derive(Clone, Default)]
pub struct LanState(Arc<Mutex<Inner>>);

fn default_alias() -> String {
    // macOS：取「电脑名称」
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    // Windows：取计算机名
    #[cfg(target_os = "windows")]
    {
        if let Ok(name) = std::env::var("COMPUTERNAME") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    // 通用回退：用户名（USER=类 Unix，USERNAME=Windows）
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .map(|u| format!("{u} 的百宝箱"))
        .unwrap_or_else(|_| "百宝箱设备".into())
}

impl LanState {
    fn device_info(&self, announce: Option<bool>) -> DeviceInfo {
        let g = self.0.lock().unwrap();
        DeviceInfo {
            alias: g.alias.clone(),
            version: PROTOCOL_VERSION.into(),
            device_model: Some("百宝箱".into()),
            device_type: Some("desktop".into()),
            fingerprint: g.fingerprint.clone(),
            port: Some(PORT),
            protocol: Some("http".into()),
            download: Some(false),
            announce,
            app: Some("baibao".into()),
            shares: Some(g.shares.len() as u32),
        }
    }

    fn is_cancelled(&self, session_id: &str) -> bool {
        self.0.lock().unwrap().cancelled.contains(session_id)
    }

    /// 发送方撤销某个文件的发送（对方接受前）。
    fn cancel_send(&self, file_id: &str) {
        self.0.lock().unwrap().cancelled_sends.insert(file_id.to_string());
    }

    fn is_send_cancelled(&self, file_id: &str) -> bool {
        self.0.lock().unwrap().cancelled_sends.contains(file_id)
    }

    /// 用完即清，避免 fileId 集合无限增长。
    fn take_send_cancelled(&self, file_id: &str) -> bool {
        self.0.lock().unwrap().cancelled_sends.remove(file_id)
    }
}

/// compat 关闭时只对外可见百宝箱设备，外部 LocalSend 设备隐藏。
fn visible_peers(g: &Inner) -> Vec<Peer> {
    g.peers
        .values()
        .filter(|p| g.compat || p.is_baibao)
        .cloned()
        .collect()
}

fn emit_peers(app: &AppHandle, state: &LanState) {
    let peers = {
        let g = state.0.lock().unwrap();
        visible_peers(&g)
    };
    let _ = app.emit("lan://peers", peers);
}

/// 收到对端设备信息：加入/刷新设备表（指纹为键）。
fn upsert_peer(app: &AppHandle, state: &LanState, info: &DeviceInfo, ip: String) {
    if info.fingerprint.is_empty() {
        return;
    }
    {
        let mut g = state.0.lock().unwrap();
        if info.fingerprint == g.fingerprint {
            return; // 自己
        }
        let peer = Peer {
            alias: info.alias.clone(),
            fingerprint: info.fingerprint.clone(),
            ip,
            port: info.port.unwrap_or(PORT),
            protocol: info.protocol.clone().unwrap_or_else(|| "http".into()),
            device_type: info.device_type.clone(),
            is_baibao: info.app.as_deref() == Some("baibao"),
            last_seen_ms: now_ms(),
            shares: info.shares.unwrap_or(0),
        };
        g.peers.insert(info.fingerprint.clone(), peer);
    }
    emit_peers(app, state);
}

/// 仅凭 alias + fingerprint + 来源 IP 登记一个百宝箱设备（用于收到消息时自动认识发送方）。
/// 已存在的设备只刷新别名/IP/活跃时间，保留其 shares 等已知信息，不被消息覆盖清零。
fn upsert_peer_minimal(app: &AppHandle, state: &LanState, alias: &str, fingerprint: &str, ip: String) {
    if fingerprint.is_empty() {
        return;
    }
    {
        let mut g = state.0.lock().unwrap();
        if fingerprint == g.fingerprint {
            return; // 自己
        }
        let now = now_ms();
        match g.peers.get_mut(fingerprint) {
            Some(p) => {
                p.alias = alias.to_string();
                p.ip = ip;
                p.last_seen_ms = now;
            }
            None => {
                g.peers.insert(
                    fingerprint.to_string(),
                    Peer {
                        alias: alias.to_string(),
                        fingerprint: fingerprint.to_string(),
                        ip,
                        port: PORT,
                        protocol: "http".into(),
                        device_type: None,
                        is_baibao: true,
                        last_seen_ms: now,
                        shares: 0,
                    },
                );
            }
        }
    }
    emit_peers(app, state);
}

// ── 启动：发现 + 收件服务 ─────────────────────────────────────

#[tauri::command]
pub async fn lan_start(app: AppHandle, state: State<'_, LanState>) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    {
        let mut g = state.0.lock().unwrap();
        if g.started {
            drop(g);
            return Ok(my_info_value(&state));
        }
        g.started = true;
    }

    // 1) HTTP 收件服务
    let server = tiny_http::Server::http(("0.0.0.0", PORT))
        .map_err(|e| format!("端口 {PORT} 监听失败（可能已被占用）：{e}"))?;
    {
        let app = app.clone();
        let state = state.clone();
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                let app = app.clone();
                let state = state.clone();
                std::thread::spawn(move || handle_request(app, state, request));
            }
        });
    }

    // 2) UDP 多播 socket（收发共用）
    let udp = build_multicast_socket().map_err(|e| format!("多播初始化失败：{e}"))?;
    let udp = Arc::new(udp);
    state.0.lock().unwrap().socket = Some(udp.clone());

    // 3) 监听多播
    {
        let app = app.clone();
        let state = state.clone();
        let sock = udp.clone();
        std::thread::spawn(move || multicast_listener(app, state, sock));
    }
    // 4) 周期广播 + 清理过期设备
    {
        let app = app.clone();
        let state = state.clone();
        let sock = udp.clone();
        std::thread::spawn(move || announcer(app, state, sock));
    }

    // 启动即主动 announce 一次（隐身时不广播）
    if !state.0.lock().unwrap().invisible {
        let info = state.device_info(Some(true));
        if let Ok(buf) = serde_json::to_vec(&info) {
            announce_to_all(&udp, &buf);
        }
    }

    Ok(my_info_value(&state))
}

/// 本机所有非回环 IPv4 接口地址。VPN 开启时会有多个（真实 LAN 网卡 + 虚拟 utun/tun），
/// 多播必须覆盖全部接口，否则只走默认路由（常是 VPN）那一张网卡，导致与同一 LAN 的设备互相发现不到。
fn local_ipv4_ifaces() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .map(|ifs| {
            ifs.into_iter()
                .filter_map(|i| match i.ip() {
                    std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 一张本机网卡的网段信息，供前端「查看当前所有网段」诊断用。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetIface {
    name: String,
    ip: String,
    netmask: String,
    prefix: u8,
    cidr: String, // 网段，如 192.168.1.0/24
    is_vpn: bool, // 依接口名猜测是否为 VPN/虚拟网卡
}

fn netmask_to_prefix(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

/// 接口名是否像 VPN/隧道虚拟网卡（utun/tun/tap/ppp/ipsec/wg…）。多个网段共存时据此提示用户。
fn looks_like_vpn(name: &str) -> bool {
    let n = name.to_lowercase();
    ["utun", "tun", "tap", "ppp", "ipsec", "wg", "zt", "tailscale"]
        .iter()
        .any(|p| n.starts_with(p))
        || n.contains("vpn")
}

/// 列出本机所有非回环 IPv4 网卡及其网段。前端用于诊断「VPN 导致多网段、互相发现不到」。
#[tauri::command]
pub fn lan_interfaces() -> Vec<NetIface> {
    let mut out = Vec::new();
    let Ok(ifs) = if_addrs::get_if_addrs() else {
        return out;
    };
    for i in ifs {
        let if_addrs::IfAddr::V4(v4) = i.addr else {
            continue;
        };
        if v4.ip.is_loopback() || v4.ip.is_unspecified() {
            continue;
        }
        let prefix = netmask_to_prefix(v4.netmask);
        let net = Ipv4Addr::from(u32::from(v4.ip) & u32::from(v4.netmask));
        out.push(NetIface {
            name: i.name.clone(),
            ip: v4.ip.to_string(),
            netmask: v4.netmask.to_string(),
            prefix,
            cidr: format!("{net}/{prefix}"),
            is_vpn: looks_like_vpn(&i.name),
        });
    }
    // VPN/虚拟网卡排在后面，真实 LAN 网卡靠前，便于一眼看到主网段
    out.sort_by_key(|n| n.is_vpn);
    out
}

/// 「经隧道路由可达」的组网/覆盖网地址（如 Tailglobal 类 mesh 的 100.64.0.0/10 节点）。
/// 这类地址不绑定在本机网卡上，只存在于路由表里（经 utun 等隧道的下一跳转发），
/// 因此不会出现在 lan_interfaces 里——单独作为「补充展示」，不影响真实网卡列表。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayRoute {
    dest: String,    // 目标地址/网段（已展开缩写）
    gateway: String, // 下一跳
    iface: String,   // 出口接口（macOS 为 utunN；Windows 为接口标识）
}

/// 展开 macOS netstat 里缩写的网段：如 "100.64/10" → "100.64.0.0/10"；纯主机地址原样返回。
#[cfg(target_os = "macos")]
fn expand_dest(d: &str) -> String {
    if let Some((addr, prefix)) = d.split_once('/') {
        let mut parts: Vec<&str> = addr.split('.').collect();
        while parts.len() < 4 {
            parts.push("0");
        }
        format!("{}/{}", parts.join("."), prefix)
    } else {
        d.to_string()
    }
}

#[cfg(target_os = "macos")]
fn overlay_routes_impl() -> Vec<OverlayRoute> {
    // 用绝对路径：GUI 启动的 App 环境 PATH 很精简，避免找不到 netstat
    let Ok(out) = std::process::Command::new("/usr/sbin/netstat")
        .args(["-rn", "-f", "inet"])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut routes = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let (dest, gateway, flags, netif) = (parts[0], parts[1], parts[2], parts[3]);
        if dest == "default" || !looks_like_vpn(netif) {
            continue;
        }
        // 仅取「网关路由」(Flags 含大写 G)：经下一跳转发的覆盖网地址，
        // 排除接口自身的 on-link 直连路由（默认路由的 cloned 标记是小写 g，不会被选中）。
        if !flags.contains('G') {
            continue;
        }
        routes.push(OverlayRoute {
            dest: expand_dest(dest),
            gateway: gateway.to_string(),
            iface: netif.to_string(),
        });
        if routes.len() >= 50 {
            break;
        }
    }
    routes
}

#[cfg(target_os = "windows")]
fn overlay_routes_impl() -> Vec<OverlayRoute> {
    let Ok(out) = std::process::Command::new("route").args(["print", "-4"]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut routes = Vec::new();
    let mut in_active = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("Active Routes:") {
            in_active = true;
            continue;
        }
        if l.starts_with("Persistent Routes:") {
            break;
        }
        if !in_active {
            continue;
        }
        let parts: Vec<&str> = l.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let (dest, mask, gateway) = (parts[0], parts[1], parts[2]);
        // 不靠列位置/表头文字（会随系统语言变化），而是校验值本身：
        // 目标与掩码必须是合法 IPv4，否则是表头/分隔/IPv6 行，跳过。
        let (Ok(_), Ok(maskv4)) = (dest.parse::<Ipv4Addr>(), mask.parse::<Ipv4Addr>()) else {
            continue;
        };
        // On-link 是接口直连，跳过；网关必须是合法 IPv4（真实下一跳）才算覆盖网路由。
        if dest == "0.0.0.0" || gateway.parse::<Ipv4Addr>().is_err() {
            continue;
        }
        let prefix = netmask_to_prefix(maskv4);
        routes.push(OverlayRoute {
            dest: format!("{dest}/{prefix}"),
            gateway: gateway.to_string(),
            iface: parts.get(3).copied().unwrap_or("").to_string(),
        });
        if routes.len() >= 50 {
            break;
        }
    }
    routes
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn overlay_routes_impl() -> Vec<OverlayRoute> {
    Vec::new()
}

/// 列出经隧道可达的组网/覆盖网地址（补充展示用）。
#[tauri::command]
pub fn lan_overlay_routes() -> Vec<OverlayRoute> {
    overlay_routes_impl()
}

/// 加入多播组。VPN 切换后新出现的接口会在 announcer 周期里被重新加入，实现「开关 VPN 自愈」。
/// 只按每张真实网卡的具体 IP 加入（en0/eth0 等 LAN 网卡靠这个收多播，最可靠）。
/// 不再混用 INADDR_ANY——单 socket 上 INADDR_ANY 会与具体网卡成员资格相互抢占，
/// 反而让真实 LAN 收不到多播；VPN/utun/组网这类多播到不了的，用「+ IP」手动添加兜底。
fn join_all_ifaces(udp: &UdpSocket) {
    let ifaces = local_ipv4_ifaces();
    if ifaces.is_empty() {
        // 枚举不到任何网卡时才退回默认接口
        let _ = udp.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED);
        return;
    }
    for ip in ifaces {
        let _ = udp.join_multicast_v4(&MULTICAST_ADDR, &ip);
    }
}

/// 把一条多播报文发出去：先按默认接口发一次（兼容 VPN/点对点接口），再按每张网卡各发一次。
/// 仅 announcer / 启动 / 取消隐身这几处调用，不与监听线程的单播应答争用 multicast_if
/// （单播走路由表，不受此选项影响）。
fn announce_to_all(udp: &UdpSocket, buf: &[u8]) {
    let dst = SocketAddr::from((MULTICAST_ADDR, PORT));
    let ifaces = local_ipv4_ifaces();
    if ifaces.is_empty() {
        let _ = udp.send_to(buf, dst);
        return;
    }
    // 按每张网卡各发一次：确保真实 LAN 网卡也发得出去（而不止默认/VPN 网卡）。
    let sref = socket2::SockRef::from(udp);
    for ip in ifaces {
        let _ = sref.set_multicast_if_v4(&ip);
        let _ = udp.send_to(buf, dst);
    }
    let _ = sref.set_multicast_if_v4(&Ipv4Addr::UNSPECIFIED);
}

fn build_multicast_socket() -> std::io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    let bind: SocketAddr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT));
    sock.bind(&bind.into())?;
    let udp: UdpSocket = sock.into();
    join_all_ifaces(&udp); // 在所有接口上加入组，而非仅默认接口
    udp.set_multicast_ttl_v4(1)?;
    let _ = udp.set_multicast_loop_v4(true);
    Ok(udp)
}

fn multicast_listener(app: AppHandle, state: LanState, sock: Arc<UdpSocket>) {
    let mut buf = [0u8; 8192];
    loop {
        let (n, src) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let info: DeviceInfo = match serde_json::from_slice(&buf[..n]) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ip = match src.ip() {
            std::net::IpAddr::V4(v4) => v4.to_string(),
            other => other.to_string(),
        };
        // 自己发的忽略
        if info.fingerprint == state.0.lock().unwrap().fingerprint {
            continue;
        }
        upsert_peer(&app, &state, &info, ip);
        // 对方在 announce → 回应自己的信息（announce=false），单播避免风暴。
        // 隐身时不应答，使对端无法发现本机（其信号灯显示离线）。
        if info.announce == Some(true) && !state.0.lock().unwrap().invisible {
            let reply = state.device_info(Some(false));
            if let Ok(b) = serde_json::to_vec(&reply) {
                let _ = sock.send_to(&b, SocketAddr::from((src.ip(), PORT)));
            }
        }
    }
}

fn announcer(app: AppHandle, state: LanState, sock: Arc<UdpSocket>) {
    loop {
        std::thread::sleep(ANNOUNCE_EVERY);
        // 每轮重新在所有接口加入组：覆盖运行期间新接入的网卡（如刚连上的 VPN / Wi-Fi 切换）
        join_all_ifaces(&sock);
        // 隐身时不广播，让对端在 TTL 过后把本机判定为离线
        if !state.0.lock().unwrap().invisible {
            let info = state.device_info(Some(true));
            if let Ok(buf) = serde_json::to_vec(&info) {
                announce_to_all(&sock, &buf);
            }
        }
        // 清理过期设备
        let removed = {
            let mut g = state.0.lock().unwrap();
            let before = g.peers.len();
            let now = now_ms();
            g.peers.retain(|_, p| now.saturating_sub(p.last_seen_ms) < PEER_TTL_MS);
            before != g.peers.len()
        };
        if removed {
            emit_peers(&app, &state);
        }
    }
}

// ── 收件请求处理 ─────────────────────────────────────────────

fn handle_request(app: AppHandle, state: LanState, mut request: tiny_http::Request) {
    let method = request.method().as_str().to_uppercase();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let query = url.splitn(2, '?').nth(1).unwrap_or("").to_string();
    let remote_ip = request
        .remote_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();

    let result: Result<(), ()> = match (method.as_str(), path.as_str()) {
        ("GET", "/api/localsend/v2/info") => {
            let info = state.device_info(None);
            respond_json(request, 200, &info);
            Ok(())
        }
        ("POST", "/api/localsend/v2/register") => {
            if let Ok(info) = read_json::<DeviceInfo>(&mut request) {
                upsert_peer(&app, &state, &info, remote_ip);
                let me = state.device_info(None);
                respond_json(request, 200, &me);
            } else {
                respond_text(request, 400, "bad request");
            }
            Ok(())
        }
        ("POST", "/api/baibao/v1/message") => {
            if let Ok(msg) = read_json::<MessagePayload>(&mut request) {
                // 用连接来源 IP 自动登记发送方：组网/VPN 等多播发现不到的场景，
                // 只要一方「+ IP」发起，收到消息的一方即自动认识对方，从而能回复。
                upsert_peer_minimal(&app, &state, &msg.alias, &msg.fingerprint, remote_ip.clone());
                let _ = app.emit(
                    "lan://message",
                    serde_json::json!({
                        "fingerprint": msg.fingerprint,
                        "alias": msg.alias,
                        "text": msg.text,
                        "id": msg.msg_id,
                        "ts": now_ms(),
                        "incoming": true,
                    }),
                );
                respond_text(request, 200, "ok");
            } else {
                respond_text(request, 400, "bad request");
            }
            Ok(())
        }
        ("POST", "/api/baibao/v1/recall") => {
            if let Ok(r) = read_json::<RecallPayload>(&mut request) {
                let _ = app.emit(
                    "lan://recall",
                    serde_json::json!({
                        "fingerprint": r.fingerprint,
                        "msgId": r.msg_id,
                    }),
                );
                respond_text(request, 200, "ok");
            } else {
                respond_text(request, 400, "bad request");
            }
            Ok(())
        }
        ("POST", "/api/localsend/v2/prepare-upload") => {
            handle_prepare_upload(&app, &state, request);
            Ok(())
        }
        ("POST", "/api/localsend/v2/upload") => {
            handle_upload(&app, &state, request, &query);
            Ok(())
        }
        ("GET", "/api/baibao/v1/partial") => {
            handle_partial(&state, request, &query);
            Ok(())
        }
        // 共享磁盘：列共享根 / 列目录 / 下载文件（详见下方处理函数）
        ("GET", "/api/baibao/v1/shares") => {
            handle_share_roots(&state, request);
            Ok(())
        }
        ("GET", "/api/baibao/v1/share/list") => {
            handle_share_list(&state, request, &query);
            Ok(())
        }
        ("GET", "/api/baibao/v1/share/download") => {
            handle_share_download(&state, request, &query);
            Ok(())
        }
        ("GET", "/api/baibao/v1/share/zip") => {
            handle_share_zip(&state, request, &query);
            Ok(())
        }
        ("POST", "/api/baibao/v1/share/upload") => {
            handle_share_upload(&state, request, &query);
            Ok(())
        }
        ("POST", "/api/baibao/v1/share/mkdir") => {
            handle_share_mkdir(&state, request, &query);
            Ok(())
        }
        ("POST", "/api/baibao/v1/share/rename") => {
            handle_share_rename(&state, request, &query);
            Ok(())
        }
        ("POST", "/api/baibao/v1/share/delete") => {
            handle_share_delete(&state, request, &query);
            Ok(())
        }
        ("POST", "/api/localsend/v2/cancel") => {
            respond_text(request, 200, "ok");
            Ok(())
        }
        _ => {
            respond_text(request, 404, "not found");
            Ok(())
        }
    };
    let _ = result;
}

// ── 共享磁盘：对外提供只读的目录浏览 / 文件下载 ───────────────

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn header_value(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.to_string().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// 把相对路径安全地拼到共享根下：拒绝 `..`/绝对前缀，并用 canonicalize 确认没逃出根目录。
fn resolve_share_path(root: &str, rel: &str) -> Option<PathBuf> {
    let root = PathBuf::from(root);
    let mut p = root.clone();
    for comp in std::path::Path::new(rel).components() {
        match comp {
            std::path::Component::Normal(c) => p.push(c),
            std::path::Component::CurDir => {}
            _ => return None, // .. / 根 / 盘符前缀 一律拒绝，防穿越
        }
    }
    let canon = p.canonicalize().ok()?;
    let root_canon = root.canonicalize().ok()?;
    canon.starts_with(&root_canon).then_some(canon)
}

enum ShareAuth {
    Open(String),   // 无密码，直接放行
    Ok(String),     // 有密码且正确
    Denied,         // 有密码但缺失/错误
    NotFound,
}

/// 校验某共享：无密码 → Open；有密码且 X-Share-Auth == sha256(密码) → Ok；否则 Denied。
fn authorize_share(g: &Inner, id: &str, auth: Option<&str>) -> ShareAuth {
    let Some(share) = g.shares.iter().find(|s| s.id == id) else {
        return ShareAuth::NotFound;
    };
    match &share.password {
        None => ShareAuth::Open(share.path.clone()),
        Some(pw) => {
            let expected = sha256_hex(pw);
            if auth.map(|a| a.eq_ignore_ascii_case(&expected)).unwrap_or(false) {
                ShareAuth::Ok(share.path.clone())
            } else {
                ShareAuth::Denied
            }
        }
    }
}

/// 该来源 IP 当前是否处于锁定期。
fn auth_is_locked(g: &Inner, ip: &str) -> bool {
    g.auth_fails.get(ip).map(|f| f.locked_until_ms > now_ms()).unwrap_or(false)
}

/// 记一次失败：滑动窗口内累计；达到上限则进入锁定期。
fn auth_record_fail(g: &mut Inner, ip: &str) {
    let now = now_ms();
    // 顺手清理过期条目（既未锁定、窗口也早已过期），避免 auth_fails 随 IP 数无限增长。
    g.auth_fails.retain(|_, f| {
        f.locked_until_ms > now || now.saturating_sub(f.window_start_ms) <= AUTH_WINDOW_MS
    });
    let f = g.auth_fails.entry(ip.to_string()).or_default();
    if now.saturating_sub(f.window_start_ms) > AUTH_WINDOW_MS {
        f.window_start_ms = now;
        f.count = 0;
    }
    f.count += 1;
    if f.count >= AUTH_MAX_FAILS {
        f.locked_until_ms = now + AUTH_LOCKOUT_MS;
        f.count = 0;
        f.window_start_ms = now;
    }
}

/// 认证成功：清除该 IP 的失败记录。
fn auth_record_success(g: &mut Inner, ip: &str) {
    g.auth_fails.remove(ip);
}

/// 带限流的鉴权：无密码共享不限流；密码保护的共享先查锁定期，再按对错计数。
/// 成功返回根路径；失败返回应答用的 HTTP 状态码（404 / 401 / 429）。
fn authorize_with_throttle(g: &mut Inner, id: &str, auth: Option<&str>, ip: &str) -> Result<String, u16> {
    match authorize_share(g, id, auth) {
        ShareAuth::Open(p) => Ok(p),
        ShareAuth::NotFound => Err(404),
        ShareAuth::Ok(p) => {
            if auth_is_locked(g, ip) {
                return Err(429); // 锁定期内即便密码正确也拒绝
            }
            auth_record_success(g, ip);
            Ok(p)
        }
        ShareAuth::Denied => {
            if auth_is_locked(g, ip) {
                return Err(429);
            }
            auth_record_fail(g, ip);
            if auth_is_locked(g, ip) {
                Err(429)
            } else {
                Err(401)
            }
        }
    }
}

/// GET /api/baibao/v1/shares —— 列出共享根（仅元数据：id/名称/是否加锁，无需认证）。
fn handle_share_roots(state: &LanState, request: tiny_http::Request) {
    let list: Vec<serde_json::Value> = state
        .0
        .lock()
        .unwrap()
        .shares
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id, "name": s.name, "locked": s.password.is_some(),
                "canCreate": s.can_create, "canModify": s.can_modify, "canDelete": s.can_delete,
            })
        })
        .collect();
    respond_json(request, 200, &serde_json::Value::Array(list));
}

/// GET /api/baibao/v1/share/list?id=&path= —— 列目录（加锁的需 X-Share-Auth）。
fn handle_share_list(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let Some(id) = q.get("id") else {
        return respond_text(request, 400, "missing id");
    };
    let rel = q.get("path").cloned().unwrap_or_default();
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let root = {
        let mut g = state.0.lock().unwrap();
        match authorize_with_throttle(&mut g, id, auth.as_deref(), &ip) {
            Ok(r) => r,
            Err(401) => return respond_text(request, 401, "auth required"),
            Err(429) => return respond_text(request, 429, "尝试过多，请稍后再试"),
            Err(_) => return respond_text(request, 404, "not found"),
        }
    };
    let Some(dir) = resolve_share_path(&root, &rel) else {
        return respond_text(request, 403, "bad path");
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return respond_text(request, 404, "not a directory");
    };
    let ms = |t: std::io::Result<std::time::SystemTime>| -> u128 {
        t.ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0)
    };
    // (是否目录, 名称, 大小, 修改时间ms, 创建时间ms)
    let mut rows: Vec<(bool, String, u64, u128, u128)> = Vec::new();
    for e in rd.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue; // 跳过隐藏文件
        }
        rows.push((meta.is_dir(), name, meta.len(), ms(meta.modified()), ms(meta.created())));
    }
    // 目录在前，再按名称（不区分大小写）排序
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(dir, name, size, mtime, ctime)| {
            serde_json::json!({ "name": name, "dir": dir, "size": size, "mtime": mtime, "ctime": ctime })
        })
        .collect();
    respond_json(request, 200, &serde_json::json!({ "entries": entries }));
}

/// GET /api/baibao/v1/share/download?id=&path= —— 下载单个文件（加锁的需 X-Share-Auth）。
fn handle_share_download(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let Some(id) = q.get("id") else {
        return respond_text(request, 400, "missing id");
    };
    let rel = q.get("path").cloned().unwrap_or_default();
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let root = {
        let mut g = state.0.lock().unwrap();
        match authorize_with_throttle(&mut g, id, auth.as_deref(), &ip) {
            Ok(r) => r,
            Err(401) => return respond_text(request, 401, "auth required"),
            Err(429) => return respond_text(request, 429, "尝试过多，请稍后再试"),
            Err(_) => return respond_text(request, 404, "not found"),
        }
    };
    let Some(file) = resolve_share_path(&root, &rel) else {
        return respond_text(request, 403, "bad path");
    };
    if !file.is_file() {
        return respond_text(request, 404, "not a file");
    }
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    match File::open(&file) {
        Ok(f) => {
            let ctype = tiny_http::Header::from_bytes(&b"Content-Type"[..], ext_mime(&name).as_bytes())
                .ok();
            let disp = tiny_http::Header::from_bytes(
                &b"Content-Disposition"[..],
                format!("attachment; filename=\"{name}\"").as_bytes(),
            )
            .ok();
            let mut resp = tiny_http::Response::from_file(f);
            if let Some(h) = ctype {
                resp.add_header(h);
            }
            if let Some(h) = disp {
                resp.add_header(h);
            }
            let _ = request.respond(resp);
        }
        Err(_) => respond_text(request, 500, "open failed"),
    }
}

// ── 共享磁盘：写操作（新增/修改/删除）+ 文件夹打包下载 ───────────

/// 取某共享授予的写权限 (新增, 修改, 删除)。
fn share_perms(g: &Inner, id: &str) -> (bool, bool, bool) {
    g.shares
        .iter()
        .find(|s| s.id == id)
        .map(|s| (s.can_create, s.can_modify, s.can_delete))
        .unwrap_or((false, false, false))
}

/// 统一鉴权（密码+限流）并返回 (共享根, 权限)；失败返回状态码。
fn share_authorize(
    state: &LanState,
    id: &str,
    auth: Option<&str>,
    ip: &str,
) -> Result<(String, (bool, bool, bool)), u16> {
    let mut g = state.0.lock().unwrap();
    let root = authorize_with_throttle(&mut g, id, auth, ip)?;
    let perms = share_perms(&g, id);
    Ok((root, perms))
}

/// 把状态码转成应答（401/403/404/429）并返回 true 表示已处理（出错）。
fn share_err_respond(request: tiny_http::Request, code: u16) {
    let msg = match code {
        401 => "auth required",
        403 => "无权限",
        429 => "尝试过多，请稍后再试",
        _ => "not found",
    };
    respond_text(request, code, msg);
}

/// 「新建」类操作的安全路径解析：拒绝 `..`/绝对路径；父目录必须存在且 canonicalize 后在共享根内。
/// 返回最终目标路径（父目录在根内 + 末段名）。
fn resolve_create_path(root: &str, rel: &str) -> Option<PathBuf> {
    let mut comps: Vec<std::ffi::OsString> = Vec::new();
    for c in std::path::Path::new(rel).components() {
        match c {
            std::path::Component::Normal(p) => comps.push(p.to_os_string()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    let name = comps.pop()?;
    let root_pb = PathBuf::from(root);
    let mut parent = root_pb.clone();
    for c in &comps {
        parent.push(c);
    }
    let parent_canon = parent.canonicalize().ok()?;
    let root_canon = root_pb.canonicalize().ok()?;
    if !parent_canon.starts_with(&root_canon) {
        return None;
    }
    Some(parent_canon.join(name))
}

/// 上传文件到共享目录（新增）。path = 目标相对路径（含文件名）；同名直接覆盖（客户端已二次确认）。
fn handle_share_upload(state: &LanState, mut request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(id), Some(rel)) = (q.get("id"), q.get("path")) else {
        return respond_text(request, 400, "missing params");
    };
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (root, perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    if !perms.0 {
        return respond_text(request, 403, "无新增权限");
    }
    let Some(dest) = resolve_create_path(&root, rel) else {
        return respond_text(request, 403, "bad path");
    };
    // 覆盖已存在的同名文件（覆盖确认在客户端做）；但拒绝把已存在的「目录」当文件覆盖
    if dest.is_dir() {
        return respond_text(request, 409, "同名目录已存在");
    }
    let mut f = match File::create(&dest) {
        Ok(f) => f,
        Err(e) => return respond_text(request, 500, &format!("写入失败：{e}")),
    };
    let res = std::io::copy(&mut request.as_reader(), &mut f);
    match res {
        Ok(_) => respond_text(request, 200, "ok"),
        Err(_) => {
            let _ = std::fs::remove_file(&dest);
            respond_text(request, 500, "写入中断")
        }
    }
}

/// 新建文件夹（新增）。
fn handle_share_mkdir(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(id), Some(rel)) = (q.get("id"), q.get("path")) else {
        return respond_text(request, 400, "missing params");
    };
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (root, perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    if !perms.0 {
        return respond_text(request, 403, "无新增权限");
    }
    let Some(dest) = resolve_create_path(&root, rel) else {
        return respond_text(request, 403, "bad path");
    };
    match std::fs::create_dir(&dest) {
        Ok(_) => respond_text(request, 200, "ok"),
        Err(e) => respond_text(request, 500, &format!("创建失败：{e}")),
    }
}

/// 重命名（修改）。path = 源相对路径；to = 新名称（不含路径分隔符）。
fn handle_share_rename(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(id), Some(rel), Some(to)) = (q.get("id"), q.get("path"), q.get("to")) else {
        return respond_text(request, 400, "missing params");
    };
    if to.contains('/') || to.contains('\\') || to.contains("..") || to.is_empty() {
        return respond_text(request, 400, "非法名称");
    }
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (root, perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    if !perms.1 {
        return respond_text(request, 403, "无修改权限");
    }
    let Some(src) = resolve_share_path(&root, rel) else {
        return respond_text(request, 403, "bad path");
    };
    let dest = match src.parent() {
        Some(p) => p.join(to),
        None => return respond_text(request, 400, "bad path"),
    };
    if dest.exists() {
        return respond_text(request, 409, "目标已存在");
    }
    match std::fs::rename(&src, &dest) {
        Ok(_) => respond_text(request, 200, "ok"),
        Err(e) => respond_text(request, 500, &format!("重命名失败：{e}")),
    }
}

/// 删除文件/文件夹（删除）。
fn handle_share_delete(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(id), Some(rel)) = (q.get("id"), q.get("path")) else {
        return respond_text(request, 400, "missing params");
    };
    if rel.is_empty() {
        return respond_text(request, 400, "不能删除共享根"); // 防止误删整个共享根
    }
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (root, perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    if !perms.2 {
        return respond_text(request, 403, "无删除权限");
    }
    let Some(target) = resolve_share_path(&root, rel) else {
        return respond_text(request, 403, "bad path");
    };
    let res = if target.is_dir() {
        std::fs::remove_dir_all(&target)
    } else {
        std::fs::remove_file(&target)
    };
    match res {
        Ok(_) => respond_text(request, 200, "ok"),
        Err(e) => respond_text(request, 500, &format!("删除失败：{e}")),
    }
}

/// 递归把目录 src 写入 zip，条目路径相对 base（base = src 的父目录，使压缩包内含顶层文件夹名）。
fn zip_add_dir(
    zw: &mut zip::ZipWriter<File>,
    base: &std::path::Path,
    dir: &std::path::Path,
    opts: zip::write::SimpleFileOptions,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let name = rel.to_string_lossy().replace('\\', "/");
        if path.is_dir() {
            let _ = zw.add_directory(format!("{name}/"), opts);
            zip_add_dir(zw, base, &path, opts)?;
        } else {
            zw.start_file(name, opts)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let mut f = File::open(&path)?;
            std::io::copy(&mut f, zw)?;
        }
    }
    Ok(())
}

/// GET /share/zip?id=&path= —— 把某文件夹打包成 zip 流式返回（文件夹获取，读取权限即可）。
fn handle_share_zip(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let Some(id) = q.get("id") else {
        return respond_text(request, 400, "missing id");
    };
    let rel = q.get("path").cloned().unwrap_or_default();
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (root, _perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    let Some(dir) = resolve_share_path(&root, &rel) else {
        return respond_text(request, 403, "bad path");
    };
    if !dir.is_dir() {
        return respond_text(request, 400, "不是文件夹");
    }
    let base = dir.parent().unwrap_or(&dir);
    let tmp = std::env::temp_dir().join(format!("baibao_share_{}.zip", rand_hex(8)));
    let build = (|| -> std::io::Result<()> {
        let file = File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        // 先显式加入顶层文件夹条目，保证空文件夹也能被还原
        if let Some(top) = dir.file_name().and_then(|n| n.to_str()) {
            let _ = zw.add_directory(format!("{top}/"), opts);
        }
        zip_add_dir(&mut zw, base, &dir, opts)?;
        zw.finish()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        Ok(())
    })();
    if build.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return respond_text(request, 500, "打包失败");
    }
    match File::open(&tmp) {
        Ok(f) => {
            let resp = tiny_http::Response::from_file(f);
            let _ = request.respond(resp);
        }
        Err(_) => respond_text(request, 500, "打包读取失败"),
    }
    let _ = std::fs::remove_file(&tmp);
}

fn handle_prepare_upload(app: &AppHandle, state: &LanState, mut request: tiny_http::Request) {
    let remote_ip = request
        .remote_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let req: PrepareUploadRequest = match read_json(&mut request) {
        Ok(v) => v,
        Err(_) => return respond_text(request, 400, "bad request"),
    };

    // compat 关闭时：只接受百宝箱设备
    let is_baibao = req.info.app.as_deref() == Some("baibao");
    if !is_baibao && !state.0.lock().unwrap().compat {
        return respond_text(request, 403, "rejected: 未开启 LocalSend 兼容");
    }
    // 用连接来源 IP 自动登记发送方（含其完整设备信息），实现单边「+ IP」即可双向。
    upsert_peer(app, state, &req.info, remote_ip);

    let session_id = rand_hex(12);
    let (tx, rx) = mpsc::channel::<Decision>();
    {
        let mut g = state.0.lock().unwrap();
        g.decisions.insert(session_id.clone(), tx);
    }

    // 通知前端弹确认框
    let files_for_ui: Vec<&FileMeta> = req.files.values().collect();
    let _ = app.emit(
        "lan://incoming",
        serde_json::json!({
            "sessionId": session_id,
            "alias": req.info.alias,
            "fingerprint": req.info.fingerprint,
            "isBaibao": is_baibao,
            "files": files_for_ui,
        }),
    );

    // 等用户确认（会话里点击文件后才确认；超时视为拒绝）
    let decision = rx.recv_timeout(Duration::from_secs(300));
    state.0.lock().unwrap().decisions.remove(&session_id);

    let decision = match decision {
        Ok(d) if d.accept => d,
        Ok(_) => return respond_text(request, 403, "declined"),
        Err(_) => {
            // 超时：通知前端把这些待接收文件标记为已过期
            let _ = app.emit("lan://offer-timeout", serde_json::json!({ "sessionId": session_id }));
            return respond_text(request, 403, "timeout");
        }
    };

    // 建立会话：只为被接受的文件发 token
    let mut tokens = HashMap::new();
    let mut slots = HashMap::new();
    for (fid, meta) in &req.files {
        if decision.file_ids.is_empty() || decision.file_ids.contains(fid) {
            let token = rand_hex(12);
            tokens.insert(fid.clone(), token.clone());
            slots.insert(
                fid.clone(),
                FileSlot {
                    token,
                    file_name: meta.file_name.clone(),
                    size: meta.size,
                    sha256: meta.sha256.clone(),
                },
            );
        }
    }
    if slots.is_empty() {
        return respond_text(request, 403, "declined");
    }
    state.0.lock().unwrap().sessions.insert(
        session_id.clone(),
        Session {
            peer: req.info.fingerprint.clone(),
            files: slots,
        },
    );

    respond_json(
        request,
        200,
        &PrepareUploadResponse {
            session_id,
            files: tokens,
        },
    );
}

/// 部分文件存放目录（断点续传用），完成后再移动到接收目录。
fn partial_path(dir: &std::path::Path, session_id: &str, file_id: &str) -> PathBuf {
    dir.join(".baibao_partial")
        .join(format!("{session_id}_{file_id}.part"))
}

/// 断点续传：发送方先查询接收方已落盘多少字节，再从该偏移续传。
/// 百宝箱自有扩展端点；LocalSend 不认（查询失败时发送方按从 0 开始处理）。
fn handle_partial(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(session_id), Some(file_id), Some(token)) =
        (q.get("sessionId"), q.get("fileId"), q.get("token"))
    else {
        return respond_text(request, 400, "missing params");
    };
    let dir = {
        let g = state.0.lock().unwrap();
        match g.sessions.get(session_id).and_then(|s| s.files.get(file_id)) {
            Some(slot) if &slot.token == token => g.download_dir.clone(),
            _ => return respond_text(request, 403, "invalid token"),
        }
    };
    let received = std::fs::metadata(partial_path(&dir, session_id, file_id))
        .map(|m| m.len())
        .unwrap_or(0);
    respond_json(request, 200, &serde_json::json!({ "received": received }));
}

fn handle_upload(app: &AppHandle, state: &LanState, mut request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(session_id), Some(file_id), Some(token)) =
        (q.get("sessionId"), q.get("fileId"), q.get("token"))
    else {
        return respond_text(request, 400, "missing params");
    };
    // 本次从哪个偏移续传（断点续传；缺省 0 = 从头）
    let offset: u64 = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);

    // 校验 token，取出文件名/大小/期望哈希/对端与下载目录
    let (file_name, size, expect_sha, peer, dir) = {
        let g = state.0.lock().unwrap();
        let sess = g.sessions.get(session_id);
        let slot = match sess.and_then(|s| s.files.get(file_id)) {
            Some(s) if &s.token == token => s,
            _ => {
                drop(g);
                return respond_text(request, 403, "invalid token");
            }
        };
        (
            slot.file_name.clone(),
            slot.size,
            slot.sha256.clone(),
            sess.map(|s| s.peer.clone()).unwrap_or_default(),
            g.download_dir.clone(),
        )
    };

    // 写入临时 .part 文件，从 offset 处续写
    let part = partial_path(&dir, session_id, file_id);
    let _ = std::fs::create_dir_all(part.parent().unwrap_or(&dir));
    let mut file = match std::fs::OpenOptions::new().create(true).write(true).open(&part) {
        Ok(f) => f,
        Err(e) => return respond_text(request, 500, &format!("无法写入：{e}")),
    };
    // 丢弃 offset 之后可能存在的脏数据，再定位到 offset
    let _ = file.set_len(offset);
    use std::io::Seek;
    if file.seek(std::io::SeekFrom::Start(offset)).is_err() {
        return respond_text(request, 500, "seek 失败");
    }

    let reader = request.as_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut received: u64 = offset;
    let mut last_emit: u64 = 0;
    let mut cancelled = false;
    let mut check_at: u64 = offset;
    loop {
        if received - check_at > 1024 * 1024 {
            check_at = received;
            if state.is_cancelled(session_id) {
                cancelled = true;
                break;
            }
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if file.write_all(&buf[..n]).is_err() {
                    break;
                }
                received += n as u64;
                if received - last_emit > 256 * 1024 || received == size {
                    last_emit = received;
                    let _ = app.emit(
                        "lan://progress",
                        serde_json::json!({
                            "direction": "in", "sessionId": session_id, "fileId": file_id,
                            "fileName": file_name, "transferred": received, "size": size, "peer": peer,
                        }),
                    );
                }
            }
            Err(_) => break, // 连接中断：保留 .part，等待续传，不删
        }
    }
    drop(file);

    if cancelled {
        let _ = std::fs::remove_file(&part);
        cleanup_session_file(state, session_id, file_id);
        let _ = app.emit(
            "lan://cancelled",
            serde_json::json!({ "direction": "in", "sessionId": session_id, "fileId": file_id, "fileName": file_name, "peer": peer }),
        );
        return respond_text(request, 400, "cancelled");
    }

    // 未收满：本次续传到此为止，保留 .part 等待下次续传
    if received < size {
        return respond_text(request, 200, "partial");
    }

    // 收满：校验 sha256，移动到接收目录
    let actual = sha256_file(part.to_string_lossy().as_ref());
    let verified = match (&expect_sha, &actual) {
        (Some(e), Some(a)) => Some(e.eq_ignore_ascii_case(a)),
        _ => None,
    };
    let dest = unique_path(&dir, &file_name);
    let _ = std::fs::rename(&part, &dest);
    cleanup_session_file(state, session_id, file_id);
    let _ = app.emit(
        "lan://received",
        serde_json::json!({
            "fileName": file_name, "path": dest.to_string_lossy(),
            "size": received, "verified": verified,
            "sessionId": session_id, "fileId": file_id, "peer": peer,
        }),
    );
    respond_text(request, 200, "ok");
}

fn cleanup_session_file(state: &LanState, session_id: &str, file_id: &str) {
    let mut g = state.0.lock().unwrap();
    if let Some(s) = g.sessions.get_mut(session_id) {
        s.files.remove(file_id);
        if s.files.is_empty() {
            g.sessions.remove(session_id);
        }
    }
}

fn sha256_file(path: &str) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ── 命令：信息 / 设置 ─────────────────────────────────────────

fn local_ip() -> String {
    // 不发实际流量：connect 一个公网地址后读本地地址，得到出网网卡 IP
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(a) = s.local_addr() {
                return a.ip().to_string();
            }
        }
    }
    String::new()
}

fn my_info_value(state: &LanState) -> serde_json::Value {
    let g = state.0.lock().unwrap();
    serde_json::json!({
        "alias": g.alias,
        "fingerprint": g.fingerprint,
        "port": PORT,
        "ip": local_ip(),
        "running": g.started,
        "compat": g.compat,
        "invisible": g.invisible,
        "downloadDir": g.download_dir.to_string_lossy(),
    })
}

#[tauri::command]
pub fn lan_my_info(state: State<'_, LanState>) -> serde_json::Value {
    my_info_value(state.inner())
}

#[tauri::command]
pub fn lan_peers(state: State<'_, LanState>) -> Vec<Peer> {
    let g = state.0.lock().unwrap();
    visible_peers(&g)
}

#[tauri::command]
pub fn lan_set_alias(state: State<'_, LanState>, alias: String) {
    let a = alias.trim();
    if !a.is_empty() {
        let mut g = state.0.lock().unwrap();
        g.alias = a.to_string();
        persist(&g);
    }
}

#[tauri::command]
pub fn lan_set_compat(app: AppHandle, state: State<'_, LanState>, enabled: bool) {
    {
        let mut g = state.0.lock().unwrap();
        g.compat = enabled;
        persist(&g);
    }
    emit_peers(&app, state.inner()); // 切换后立即按新规则刷新设备列表
}

/// 切换隐身模式：隐身时停止广播/应答；取消隐身时立即广播一次让对端尽快重新发现本机。
#[tauri::command]
pub fn lan_set_invisible(state: State<'_, LanState>, enabled: bool) {
    let sock = {
        let mut g = state.0.lock().unwrap();
        g.invisible = enabled;
        persist(&g);
        g.socket.clone()
    };
    if !enabled {
        if let Some(sock) = sock {
            let info = state.device_info(Some(true));
            if let Ok(buf) = serde_json::to_vec(&info) {
                announce_to_all(&sock, &buf);
            }
        }
    }
}

/// 取消一次传输（收发双向，按 sessionId）。
#[tauri::command]
pub fn lan_cancel(state: State<'_, LanState>, session_id: String) {
    state.0.lock().unwrap().cancelled.insert(session_id);
}

/// 发送方撤销尚未被接受的发送（按 fileId）。握手返回后发送流程会据此跳过上传。
#[tauri::command]
pub fn lan_cancel_send(state: State<'_, LanState>, file_id: String) {
    state.cancel_send(&file_id);
}

/// 发现失败时的兜底：手动按 IP 拉取对端信息并加入设备表。
#[tauri::command]
pub async fn lan_add_peer(
    app: AppHandle,
    state: State<'_, LanState>,
    ip: String,
    port: Option<u16>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let port = port.unwrap_or(PORT);
    let ip2 = ip.clone();
    let info: DeviceInfo = tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(format!("http://{ip2}:{port}/api/localsend/v2/info"))
            .send()
            .map_err(|e| format!("连不上 {ip2}:{port}：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("对方返回 {}", resp.status()));
        }
        resp.json::<DeviceInfo>().map_err(|e| format!("对方不是有效设备：{e}"))
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))??;
    upsert_peer(&app, &state, &info, ip);
    Ok(())
}

#[tauri::command]
pub fn lan_set_dir(state: State<'_, LanState>, dir: String) {
    if !dir.trim().is_empty() {
        let mut g = state.0.lock().unwrap();
        g.download_dir = PathBuf::from(dir);
        persist(&g);
    }
}

// ── 命令：共享磁盘（主机端：管理「我」共享出去的目录）─────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareView {
    id: String,
    name: String,
    path: String,
    locked: bool,
    password: Option<String>, // 明文，供主机端展示/复制告知对端
    can_create: bool,
    can_modify: bool,
    can_delete: bool,
}

fn share_views(g: &Inner) -> Vec<ShareView> {
    g.shares
        .iter()
        .map(|s| ShareView {
            id: s.id.clone(),
            name: s.name.clone(),
            path: s.path.clone(),
            locked: s.password.is_some(),
            password: s.password.clone(),
            can_create: s.can_create,
            can_modify: s.can_modify,
            can_delete: s.can_delete,
        })
        .collect()
}

#[tauri::command]
pub fn lan_list_shares(state: State<'_, LanState>) -> Vec<ShareView> {
    share_views(&state.0.lock().unwrap())
}

#[tauri::command]
pub fn lan_add_share(
    state: State<'_, LanState>,
    path: String,
    name: Option<String>,
    password: Option<String>,
) -> Result<Vec<ShareView>, String> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err("不是有效目录".into());
    }
    let nm = name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "共享".into())
        });
    let mut g = state.0.lock().unwrap();
    if g.shares.iter().any(|s| s.path == path) {
        return Err("该目录已在共享列表".into());
    }
    // 默认生成 21 位随机密码（大小写字母+数字，区分大小写）；显式传了密码则用传入的。
    let pw = password
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| rand_password(21));
    g.shares.push(ShareCfg {
        id: rand_hex(8),
        name: nm,
        path,
        password: Some(pw),
        // 默认只读，写权限由用户在配置里勾选授予
        can_create: false,
        can_modify: false,
        can_delete: false,
    });
    persist(&g);
    Ok(share_views(&g))
}

/// 设置某共享授予访问方的写权限（新增/修改/删除）。
#[tauri::command]
pub fn lan_set_share_perms(
    state: State<'_, LanState>,
    id: String,
    can_create: bool,
    can_modify: bool,
    can_delete: bool,
) -> Vec<ShareView> {
    let mut g = state.0.lock().unwrap();
    if let Some(s) = g.shares.iter_mut().find(|s| s.id == id) {
        s.can_create = can_create;
        s.can_modify = can_modify;
        s.can_delete = can_delete;
    }
    persist(&g);
    share_views(&g)
}

#[tauri::command]
pub fn lan_remove_share(state: State<'_, LanState>, id: String) -> Vec<ShareView> {
    let mut g = state.0.lock().unwrap();
    g.shares.retain(|s| s.id != id);
    persist(&g);
    share_views(&g)
}

/// 设置/清除某共享的密码（password 为空或 None = 取消密码）。
#[tauri::command]
pub fn lan_set_share_password(
    state: State<'_, LanState>,
    id: String,
    password: Option<String>,
) -> Vec<ShareView> {
    let mut g = state.0.lock().unwrap();
    if let Some(s) = g.shares.iter_mut().find(|s| s.id == id) {
        s.password = password.filter(|p| !p.is_empty());
    }
    persist(&g);
    share_views(&g)
}

// ── 命令：共享磁盘（客户端：浏览「对方」共享的目录，走 HTTP 经隧道也可达）──

/// 与对端共享相关的 HTTP 客户端：固定较短超时，避免界面卡住。
fn share_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

/// 列出对端的共享根。
#[tauri::command]
pub async fn lan_share_roots(
    state: State<'_, LanState>,
    fingerprint: String,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    let (base, _) = peer_base_url(&state, &fingerprint)?;
    tauri::async_runtime::spawn_blocking(move || {
        let client = share_client()?;
        let resp = client
            .get(format!("{base}/api/baibao/v1/shares"))
            .send()
            .map_err(|e| format!("连接失败：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("对方返回 {}", resp.status()));
        }
        resp.json::<serde_json::Value>().map_err(|e| format!("解析失败：{e}"))
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 列出对端某共享下某目录。auth = sha256(密码) 的十六进制；401 时返回 Err("auth") 让前端弹密码框。
#[tauri::command]
pub async fn lan_share_list(
    state: State<'_, LanState>,
    fingerprint: String,
    id: String,
    path: String,
    auth: Option<String>,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    let (base, _) = peer_base_url(&state, &fingerprint)?;
    tauri::async_runtime::spawn_blocking(move || {
        let client = share_client()?;
        let mut req = client
            .get(format!("{base}/api/baibao/v1/share/list"))
            .query(&[("id", id.as_str()), ("path", path.as_str())]);
        if let Some(a) = auth {
            req = req.header("X-Share-Auth", a);
        }
        let resp = req.send().map_err(|e| format!("连接失败：{e}"))?;
        match resp.status().as_u16() {
            200 => resp.json::<serde_json::Value>().map_err(|e| format!("解析失败：{e}")),
            401 => Err("auth".into()),
            429 => Err("密码错误次数过多，请稍后再试".into()),
            403 => Err("路径无效或无权限（对方百宝箱可能需更新到最新版）".into()),
            c => Err(format!("对方返回 {c}")),
        }
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 下载对端某共享里的一个文件到本机接收目录，返回保存路径。401 时返回 Err("auth")。
#[tauri::command]
pub async fn lan_share_download(
    state: State<'_, LanState>,
    fingerprint: String,
    id: String,
    path: String,
    auth: Option<String>,
    is_dir: bool,
) -> Result<String, String> {
    let state = state.inner().clone();
    let (base, _) = peer_base_url(&state, &fingerprint)?;
    let dir = state.0.lock().unwrap().download_dir.clone();
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let client = share_client()?;
        let _ = std::fs::create_dir_all(&dir);
        let endpoint = if is_dir { "share/zip" } else { "share/download" };
        let mut req = client
            .get(format!("{base}/api/baibao/v1/{endpoint}"))
            .query(&[("id", id.as_str()), ("path", path.as_str())]);
        if let Some(a) = auth {
            req = req.header("X-Share-Auth", a);
        }
        let mut resp = req.send().map_err(|e| format!("连接失败：{e}"))?;
        match resp.status().as_u16() {
            200 => {}
            401 => return Err("auth".into()),
            429 => return Err("密码错误次数过多，请稍后再试".into()),
            403 => return Err("路径无效或无权限（对方百宝箱可能需更新到最新版）".into()),
            c => return Err(format!("对方返回 {c}")),
        }
        if is_dir {
            // 文件夹：下载 zip → 解压到接收目录 → 删除 zip（对用户无感知）
            let zip_path = dir.join(format!(".baibao_dl_{}.zip", rand_hex(6)));
            {
                let mut f = File::create(&zip_path).map_err(|e| format!("无法写入：{e}"))?;
                resp.copy_to(&mut f).map_err(|e| format!("下载中断：{e}"))?;
            }
            let extract = (|| -> Result<(), String> {
                let zf = File::open(&zip_path).map_err(|e| e.to_string())?;
                let mut ar = zip::ZipArchive::new(zf).map_err(|e| e.to_string())?;
                ar.extract(&dir).map_err(|e| e.to_string())
            })();
            let _ = std::fs::remove_file(&zip_path);
            extract.map_err(|e| format!("解压失败：{e}"))?;
            Ok(dir.join(&name).to_string_lossy().to_string())
        } else {
            let dest = unique_path(&dir, &name);
            let mut f = File::create(&dest).map_err(|e| format!("无法写入：{e}"))?;
            resp.copy_to(&mut f).map_err(|e| format!("下载中断：{e}"))?;
            Ok(dest.to_string_lossy().to_string())
        }
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 上传时按字节上报进度的读取包装：每读一段就 emit 一次「lan://share-upload」。
struct ShareUploadReader {
    inner: File,
    app: AppHandle,
    name: String,
    sent: u64,
    size: u64,
    last_emit: u64,
}
impl Read for ShareUploadReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.sent += n as u64;
            if self.sent - self.last_emit > 256 * 1024 || self.sent >= self.size {
                self.last_emit = self.sent;
                let _ = self.app.emit(
                    "lan://share-upload",
                    serde_json::json!({ "name": self.name, "transferred": self.sent, "size": self.size }),
                );
            }
        }
        Ok(n)
    }
}

/// 上传本地文件到对端共享目录（新增）。dest_path = 目标相对路径（含文件名）。带进度上报。
#[tauri::command]
pub async fn lan_share_upload(
    app: AppHandle,
    state: State<'_, LanState>,
    fingerprint: String,
    id: String,
    dest_path: String,
    local_path: String,
    auth: Option<String>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, _) = peer_base_url(&state, &fingerprint)?;
    let name = dest_path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .to_string();
    tauri::async_runtime::spawn_blocking(move || {
        // 不设总超时（大文件可能传很久），仅限制建连
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .build()
            .map_err(|e| e.to_string())?;
        let size = std::fs::metadata(&local_path)
            .map_err(|e| format!("读取本地文件失败：{e}"))?
            .len();
        let f = File::open(&local_path).map_err(|e| format!("打开本地文件失败：{e}"))?;
        let reader = ShareUploadReader {
            inner: f,
            app: app.clone(),
            name: name.clone(),
            sent: 0,
            size,
            last_emit: 0,
        };
        let body = reqwest::blocking::Body::sized(reader, size);
        let mut req = client
            .post(format!("{base}/api/baibao/v1/share/upload"))
            .query(&[("id", id.as_str()), ("path", dest_path.as_str())])
            .body(body);
        if let Some(a) = auth {
            req = req.header("X-Share-Auth", a);
        }
        let resp = req.send().map_err(|e| format!("连接失败：{e}"))?;
        // 收尾：补一条 100% 进度，确保前端进度条收满
        let _ = app.emit(
            "lan://share-upload",
            serde_json::json!({ "name": name, "transferred": size, "size": size }),
        );
        share_op_result(resp.status().as_u16())
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 共享写操作（mkdir/rename/delete）的统一 POST 客户端。
#[tauri::command]
pub async fn lan_share_op(
    state: State<'_, LanState>,
    fingerprint: String,
    id: String,
    op: String, // "mkdir" | "rename" | "delete"
    path: String,
    to: Option<String>,
    auth: Option<String>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, _) = peer_base_url(&state, &fingerprint)?;
    tauri::async_runtime::spawn_blocking(move || {
        let client = share_client()?;
        let mut query: Vec<(&str, &str)> = vec![("id", id.as_str()), ("path", path.as_str())];
        if let Some(t) = &to {
            query.push(("to", t.as_str()));
        }
        let mut req = client
            .post(format!("{base}/api/baibao/v1/share/{op}"))
            .query(&query);
        if let Some(a) = auth {
            req = req.header("X-Share-Auth", a);
        }
        let resp = req.send().map_err(|e| format!("连接失败：{e}"))?;
        share_op_result(resp.status().as_u16())
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 把共享写操作的 HTTP 状态码翻译成统一结果（401→auth 让前端弹密码框）。
fn share_op_result(code: u16) -> Result<(), String> {
    match code {
        200 => Ok(()),
        401 => Err("auth".into()),
        403 => Err("对方未授予该权限".into()),
        429 => Err("密码错误次数过多，请稍后再试".into()),
        409 => Err("目标已存在".into()),
        c => Err(format!("操作失败（{c}）")),
    }
}

/// 接收方对一次文件请求作出决定。
#[tauri::command]
pub fn lan_respond(
    state: State<'_, LanState>,
    session_id: String,
    accept: bool,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let tx = state
        .0
        .lock()
        .unwrap()
        .decisions
        .remove(&session_id)
        .ok_or("会话不存在或已超时")?;
    tx.send(Decision { accept, file_ids })
        .map_err(|_| "对方已取消".to_string())
}

// ── 命令：原生选择器 ─────────────────────────────────────────

#[tauri::command]
pub async fn lan_pick_files(app: AppHandle) -> Result<Vec<String>, String> {
    // 原生跨平台文件选择器（macOS / Windows / Linux 通用）。
    // 仅支持选单个或多个文件，不支持文件夹。blocking_* 不能在主线程调用，
    // 这里在阻塞线程池里调用，对话框本身仍由主线程弹出。
    tauri::async_runtime::spawn_blocking(move || {
        let picked = app
            .dialog()
            .file()
            .set_title("选择要发送的文件")
            .blocking_pick_files();
        let paths = picked
            .unwrap_or_default()
            .into_iter()
            .filter_map(|fp| fp.into_path().ok())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        Ok(paths)
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

#[tauri::command]
pub async fn lan_pick_dir(app: AppHandle) -> Result<Option<String>, String> {
    // 原生跨平台目录选择器：选「接收文件的保存目录」。
    tauri::async_runtime::spawn_blocking(move || {
        let picked = app
            .dialog()
            .file()
            .set_title("选择接收文件的保存目录")
            .blocking_pick_folder();
        Ok(picked
            .and_then(|fp| fp.into_path().ok())
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

#[tauri::command]
pub async fn lan_reveal(app: AppHandle, path: String) -> Result<(), String> {
    // 在系统文件管理器中定位文件（Finder / 资源管理器 / 文件管理器）。
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| format!("打开失败：{e}"))
}

// ── 命令：发送 ───────────────────────────────────────────────

fn peer_base_url(state: &LanState, fingerprint: &str) -> Result<(String, String), String> {
    let g = state.0.lock().unwrap();
    let p = g.peers.get(fingerprint).ok_or("设备不在线或已离开")?;
    Ok((
        format!("{}://{}:{}", p.protocol, p.ip, p.port),
        g.alias.clone(),
    ))
}

#[tauri::command]
pub async fn lan_send_message(
    state: State<'_, LanState>,
    fingerprint: String,
    text: String,
    msg_id: Option<String>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, my_alias) = peer_base_url(&state, &fingerprint)?;
    let my_fp = state.0.lock().unwrap().fingerprint.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let body = serde_json::json!({ "alias": my_alias, "fingerprint": my_fp, "text": text, "msgId": msg_id });
        let resp = client
            .post(format!("{base}/api/baibao/v1/message"))
            .json(&body)
            .send()
            .map_err(|e| format!("发送失败：{e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("对方返回 {}", resp.status()))
        }
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 撤回一条已发送的文本消息：通知对端把同一条（共享 msgId）标记为「已撤回」。
#[tauri::command]
pub async fn lan_recall_message(
    state: State<'_, LanState>,
    fingerprint: String,
    msg_id: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, _my_alias) = peer_base_url(&state, &fingerprint)?;
    let my_fp = state.0.lock().unwrap().fingerprint.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let body = serde_json::json!({ "fingerprint": my_fp, "msgId": msg_id });
        let resp = client
            .post(format!("{base}/api/baibao/v1/recall"))
            .json(&body)
            .send()
            .map_err(|e| format!("撤回失败：{e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("对方返回 {}", resp.status()))
        }
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 发文件读取时上报进度的包装，支持中途取消与断点续传（base = 已传偏移）。
struct ProgressReader {
    inner: File,
    app: AppHandle,
    state: LanState,
    session_id: String,
    file_id: String,
    file_name: String,
    peer: String,
    base: u64,
    size: u64,
    sent: u64,
    last_emit: u64,
    check_at: u64,
}
impl Read for ProgressReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.sent - self.check_at > 1024 * 1024 || self.check_at == 0 {
            self.check_at = self.sent;
            if self.state.is_cancelled(&self.session_id) {
                return Err(std::io::Error::new(std::io::ErrorKind::Other, "cancelled"));
            }
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.sent += n as u64;
            let abs = self.base + self.sent;
            if abs - self.last_emit > 256 * 1024 || abs >= self.size {
                self.last_emit = abs;
                let _ = self.app.emit(
                    "lan://progress",
                    serde_json::json!({
                        "direction": "out", "sessionId": self.session_id, "fileId": self.file_id,
                        "fileName": self.file_name, "transferred": abs, "size": self.size, "peer": self.peer,
                    }),
                );
            }
        }
        Ok(n)
    }
}

/// 向接收方查询某文件已落盘的字节数（断点续传偏移）。非百宝箱对端会失败 → 返回 0。
fn query_offset(client: &reqwest::blocking::Client, base: &str, sid: &str, fid: &str, token: &str) -> u64 {
    let r = client
        .get(format!("{base}/api/baibao/v1/partial"))
        .query(&[("sessionId", sid), ("fileId", fid), ("token", token)])
        .timeout(Duration::from_secs(8))
        .send();
    if let Ok(resp) = r {
        if resp.status().is_success() {
            if let Ok(v) = resp.json::<serde_json::Value>() {
                return v.get("received").and_then(|x| x.as_u64()).unwrap_or(0);
            }
        }
    }
    0
}

#[tauri::command]
pub async fn lan_send_files(
    app: AppHandle,
    state: State<'_, LanState>,
    fingerprint: String,
    paths: Vec<String>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, my_alias) = peer_base_url(&state, &fingerprint)?;
    let info = state.device_info(None);
    if paths.is_empty() {
        return Err("没有要发送的文件".into());
    }

    tauri::async_runtime::spawn_blocking(move || {
        let _ = my_alias;
        // 不设总超时（大文件可能传很久），仅限制建连耗时；中断由断点续传重试兜底
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .build()
            .map_err(|e| e.to_string())?;

        // 每个文件各自作为一个独立会话、并发发送：接收方可逐个确认/拒绝，互不影响。
        // 并发是关键——prepare-upload 会阻塞等对方做决定，串行会导致后面的文件迟迟发不出、
        // 发送方会话框里也看不到其余「待发送」气泡。只支持单文件，不支持文件夹。
        // 单个文件的成功/失败（被拒、上传失败等）都通过对话里的文件卡片反映，不冒泡成顶部错误条；
        // 只有「还没生成卡片」的前置问题（文件夹/读不出）才用错误条提示。
        // 同一次发送共用一个批次号；文件按选择顺序编号 order=0,1,2…
        // 发送方：在主循环里按序立刻发「待发送」气泡 → 自己看到的顺序 = 选择顺序。
        // 接收方：order + batch 随 prepare-upload 带过去，据此排序 → 看到的顺序 = 发送顺序。
        let batch = rand_hex(6);
        let mut preflight_errs: Vec<String> = Vec::new();
        let mut handles = Vec::new();
        let mut order: u32 = 0;
        for path in &paths {
            let pb = PathBuf::from(path);
            let meta = match std::fs::metadata(&pb) {
                Ok(m) => m,
                Err(e) => {
                    preflight_errs.push(format!("读取 {path} 失败：{e}"));
                    continue;
                }
            };
            if meta.is_dir() {
                preflight_errs.push(format!("暂不支持发送文件夹：{path}"));
                continue;
            }
            let name = pb
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            let size = meta.len();
            let id = rand_hex(8);
            let idx = order;
            order += 1;
            // 本地立即按序显示「待发送」气泡（带 order，前端据此保证发送方一侧的顺序）
            let _ = app.emit(
                "lan://send-pending",
                serde_json::json!({ "fileId": id, "fileName": name, "size": size, "peer": fingerprint, "order": idx }),
            );
            // 每个文件单开线程并发发送（握手会阻塞等对方决定，必须并发，否则后面的文件发不出）。
            // reqwest::blocking::Client / AppHandle / LanState 都可廉价 clone 跨线程。
            let (app, state, client) = (app.clone(), state.clone(), client.clone());
            let (base, info, fingerprint, path, batch) =
                (base.clone(), info.clone(), fingerprint.clone(), path.clone(), batch.clone());
            handles.push(std::thread::spawn(move || {
                // 失败已由 send_one_file 通过 lan://send-rejected 标记到文件卡片，这里忽略返回值
                let _ = send_one_file(
                    &app, &state, &client, &base, &info, &fingerprint, &name, &path, size, &id, idx,
                    &batch,
                );
            }));
        }
        for h in handles {
            let _ = h.join();
        }
        if !preflight_errs.is_empty() {
            return Err(preflight_errs.join("；"));
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

/// 发送单个文件（一个独立会话）：走握手 + 上传；「待发送」气泡已在主循环里按序发好。
/// 任何失败都只把这条文件卡片标记为失败（lan://send-rejected），不冒泡成顶部错误条。
#[allow(clippy::too_many_arguments)]
fn send_one_file(
    app: &AppHandle,
    state: &LanState,
    client: &reqwest::blocking::Client,
    base: &str,
    info: &DeviceInfo,
    fingerprint: &str,
    name: &str,
    path: &str,
    size: u64,
    id: &str,
    order: u32,
    batch: &str,
) -> Result<(), String> {
    let r = send_one_file_inner(
        app, state, client, base, info, fingerprint, name, path, size, id, order, batch,
    );
    // 用户在对方接受前撤销了发送：标记为「已取消」（而非失败），优先于失败原因
    if state.take_send_cancelled(id) {
        let _ = app.emit(
            "lan://send-cancelled",
            serde_json::json!({ "fileId": id, "peer": fingerprint }),
        );
        return Ok(());
    }
    if let Err(reason) = &r {
        // 失败（被拒 / 上传失败 / 打开失败 / 未被接受）：把这条卡片标记为失败并带上原因，仅卡片可见
        let _ = app.emit(
            "lan://send-rejected",
            serde_json::json!({ "fileId": id, "peer": fingerprint, "reason": reason }),
        );
    }
    r
}

/// prepare-upload 握手 → 流式上传（带断点续传重试）。失败用 Err 返回，由 send_one_file 统一标记卡片。
#[allow(clippy::too_many_arguments)]
fn send_one_file_inner(
    app: &AppHandle,
    state: &LanState,
    client: &reqwest::blocking::Client,
    base: &str,
    info: &DeviceInfo,
    fingerprint: &str,
    name: &str,
    path: &str,
    size: u64,
    id: &str,
    order: u32,
    batch: &str,
) -> Result<(), String> {
    // 还没开始就被撤销
    if state.is_send_cancelled(id) {
        return Ok(());
    }
    // 组装单文件清单（含 sha256 校验完整性；order/batch 让接收端按发送顺序展示）
    let mut fj = serde_json::json!({
        "id": id, "fileName": name, "size": size, "fileType": ext_mime(name),
        "order": order, "batch": batch,
    });
    if let Some(h) = sha256_file(path) {
        fj["sha256"] = serde_json::Value::String(h);
    }
    let mut files = serde_json::Map::new();
    files.insert(id.to_string(), fj);

    // 握手：prepare-upload
    let prep = client
        .post(format!("{base}/api/localsend/v2/prepare-upload"))
        .json(&serde_json::json!({ "info": info, "files": files }))
        .send()
        .map_err(|_| "无法连接对方".to_string())?;
    let status = prep.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err("对方拒绝接收".to_string());
    }
    if !status.is_success() {
        return Err(format!("对方未接受（{status}）"));
    }
    let resp: PrepareUploadResponse = prep.json().map_err(|_| "握手响应异常".to_string())?;

    let Some(token) = resp.files.get(id) else {
        return Err("对方未接受此文件".to_string());
    };

    // 对方刚接受，但在等待期间用户已撤销 → 不再上传
    if state.is_send_cancelled(id) {
        return Ok(());
    }

    // 通知前端：文件已被接受、开始发送（把「待发送」气泡升级为传输中）
    let _ = app.emit(
        "lan://outgoing",
        serde_json::json!({
            "sessionId": resp.session_id, "peer": fingerprint,
            "files": [serde_json::json!({ "fileId": id, "fileName": name, "size": size })],
        }),
    );

    // 上传；断点续传：失败后查偏移重试，覆盖网络抖动/中断
    const MAX_RETRY: u32 = 6;
    let mut attempt = 0u32;
    loop {
        if state.is_cancelled(&resp.session_id) {
            let _ = app.emit(
                "lan://cancelled",
                serde_json::json!({ "direction": "out", "sessionId": resp.session_id, "fileId": id, "fileName": name, "peer": fingerprint }),
            );
            return Ok(());
        }
        let offset = query_offset(client, base, &resp.session_id, id, token).min(size);
        if offset >= size {
            break; // 对端已收满
        }
        let mut f = File::open(path).map_err(|_| "读取文件失败".to_string())?;
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(offset))
            .map_err(|_| "读取文件失败".to_string())?;
        let remaining = size - offset;
        let reader = ProgressReader {
            inner: f,
            app: app.clone(),
            state: state.clone(),
            session_id: resp.session_id.clone(),
            file_id: id.to_string(),
            file_name: name.to_string(),
            peer: fingerprint.to_string(),
            base: offset,
            size,
            sent: 0,
            last_emit: offset,
            check_at: 0,
        };
        let body = reqwest::blocking::Body::sized(reader, remaining);
        let res = client
            .post(format!("{base}/api/localsend/v2/upload"))
            .query(&[
                ("sessionId", resp.session_id.as_str()),
                ("fileId", id),
                ("token", token.as_str()),
                ("offset", offset.to_string().as_str()),
            ])
            .body(body)
            .send();
        match res {
            Ok(r) if r.status().is_success() => break, // 整段发完
            Ok(r) => return Err(format!("对方中断接收（{}）", r.status())),
            Err(_) if state.is_cancelled(&resp.session_id) => {
                let _ = app.emit(
                    "lan://cancelled",
                    serde_json::json!({ "direction": "out", "sessionId": resp.session_id, "fileId": id, "fileName": name, "peer": fingerprint }),
                );
                return Ok(());
            }
            Err(e) => {
                attempt += 1;
                if attempt > MAX_RETRY {
                    let _ = e;
                    return Err("传输失败（网络中断）".to_string());
                }
                std::thread::sleep(Duration::from_millis(800));
                // 回到循环开头：重新查偏移、从断点续传
            }
        }
    }
    let _ = app.emit(
        "lan://sent",
        serde_json::json!({ "fileName": name, "fingerprint": fingerprint, "sessionId": resp.session_id, "fileId": id }),
    );
    Ok(())
}

// ── HTTP 小工具 ──────────────────────────────────────────────

fn read_json<T: serde::de::DeserializeOwned>(req: &mut tiny_http::Request) -> Result<T, String> {
    let mut body = String::new();
    req.as_reader()
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

fn respond_json<T: Serialize>(req: tiny_http::Request, code: u16, body: &T) {
    let json = serde_json::to_string(body).unwrap_or_else(|_| "{}".into());
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("valid header");
    let resp = tiny_http::Response::from_string(json)
        .with_status_code(code)
        .with_header(header);
    let _ = req.respond(resp);
}

fn respond_text(req: tiny_http::Request, code: u16, body: &str) {
    let resp = tiny_http::Response::from_string(body).with_status_code(code);
    let _ = req.respond(resp);
}

/// 解码 URL 查询值：`+`→空格、`%XX`→字节（application/x-www-form-urlencoded，reqwest .query() 用的就是它）。
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hexv = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexv(b[i + 1]), hexv(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            // 必须解码：reqwest 会把 path 里的 `/`、空格、中文等编码，不解码会导致目录/文件找不到
            let k = url_decode(it.next()?);
            let v = url_decode(it.next().unwrap_or(""));
            Some((k, v))
        })
        .collect()
}

/// 目标目录里取一个不冲突的文件名：foo.txt → foo (1).txt …
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    for i in 1..10_000 {
        let cand = dir.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    p
}
