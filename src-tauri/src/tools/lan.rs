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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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
    ip: String, // 主 IP（最近一次发现）
    #[serde(default)]
    ips: Vec<String>, // 同一设备被发现过的所有 IP（多网段），最近的在前；供请求代理指定走哪个 IP
    port: u16,
    protocol: String,
    device_type: Option<String>,
    is_baibao: bool,
    last_seen_ms: u128,
    shares: u32, // 对方对外共享的目录数量（0=无）
    #[serde(default)]
    sticky: bool, // 用户手动扫描/按 IP 添加：VPN 等多播不可见网段，不参与 TTL 过期清理
}

/// 把新发现的 IP 并入已知 IP 列表：去重、最近的放最前、上限 8 个。
fn merge_ips(existing: &[String], new_ip: &str) -> Vec<String> {
    let mut out = vec![new_ip.to_string()];
    for ip in existing {
        if ip != new_ip && !out.contains(ip) {
            out.push(ip.clone());
        }
    }
    out.truncate(8);
    out
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
    /// 百宝箱抗抖动扩展：发送端生成的稳定 offer 标识。带它即走「立即应答 + 幂等轮询」路径，
    /// 等待对方接收期间断连可后台重连重试，且接收端按它幂等、不会重复弹「点击接收」。
    /// LocalSend 等旧端不带此字段 → 走原阻塞式握手（兼容）。
    #[serde(default)]
    offer_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareUploadResponse {
    session_id: String,
    files: HashMap<String, String>,
}

/// 发送端解析握手应答。`status` 仅百宝箱接收端返回："pending"=对方还没点接收，需继续轮询；
/// 其余（含 LocalSend 旧端无此字段的情况）一律视为已接受，`files` 即各文件的上传 token。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareUploadReply {
    #[serde(default)]
    status: Option<String>,
    session_id: String,
    #[serde(default)]
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

/// 一次「待接收」请求的状态（抗抖动握手用）。接收端按 offerId 保存，发送端轮询读取：
/// 发送端可对同一 offerId 反复重试（网络抖动后后台重连），接收端据此幂等——既不重复弹窗，
/// 也不会因一次瞬断就丢掉这次请求。
enum OfferState {
    Pending,                           // 还没点「接收」
    Accepted(HashMap<String, String>), // 已接收：file_id -> token（Session 已写入 sessions）
    Declined,                          // 已拒绝
}

struct Offer {
    created: Instant,
    session_id: String,
    peer: String,                     // 发送方指纹
    files: HashMap<String, FileMeta>, // 接收时据此生成 token
    state: OfferState,
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
    decisions: HashMap<String, Sender<Decision>>, // 旧阻塞式握手（LocalSend 兼容）：sessionId -> 决定通道
    offers: HashMap<String, Offer>,     // 抗抖动握手：offerId -> 待接收/已决定请求
    sessions: HashMap<String, Session>, // sessionId -> 会话
    cancelled: HashSet<String>,         // 已取消的 sessionId（收发双向）
    cancelled_sends: HashSet<String>,   // 发送方取消的 fileId（对方还没接受前就撤销发送）
    share_uploads: HashMap<String, Arc<std::sync::atomic::AtomicBool>>, // taskId -> 中断标志（打包/传输都会检查）
    share_receives: HashMap<String, Arc<std::sync::atomic::AtomicBool>>, // token -> 拒绝标志（接收方点「拒绝」置位）
    share_rejected: HashSet<String>, // 已被接收方拒绝的 token（发送方据此判定「对方拒绝」并停止）
    socket: Option<Arc<UdpSocket>>,
    // ── 代理（SOCKS5 over 百宝箱 TLS）。默认关闭、不持久化（每次启动都是关）。──
    proxy_role: u8,                  // 0=关 / 1=服务端(出口,替对端访问) / 2=客户端(本地 SOCKS5 入口)
    proxy_socks_port: u16,           // 客户端模式下本地 SOCKS5 端口（0=未开）
    proxy_http_port: u16,            // 客户端模式下本地 HTTP 代理端口（0=未开）
    proxy_port: u16,                 // 隧道端口（服务端=监听端口 / 客户端=对方服务端端口）
    proxy_task: Option<tauri::async_runtime::JoinHandle<()>>, // 监听任务句柄，停止时 abort
    proxy_conns: std::sync::Arc<std::sync::atomic::AtomicUsize>, // 当前活跃隧道连接数（供状态展示）
    proxy_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>, // 服务端累计转发字节（前端算实时速率画折线）
    proxy_hosts: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (u64, u128)>>>, // "host:port" -> (次数, 最近时间ms)
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
    let out = crate::tools::hidden_command("reg")
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
            offers: HashMap::new(),
            sessions: HashMap::new(),
            cancelled: HashSet::new(),
            cancelled_sends: HashSet::new(),
            share_uploads: HashMap::new(),
            share_receives: HashMap::new(),
            share_rejected: HashSet::new(),
            proxy_role: 0,
            proxy_socks_port: 0,
            proxy_http_port: 0,
            proxy_port: 0,
            proxy_task: None,
            proxy_conns: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            proxy_bytes: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            proxy_hosts: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            socket: None,
        };
        // 落盘一次，确保（尤其是随机指纹）下次启动可复用
        persist(&inner);
        inner
    }
}

// ── 配置持久化（别名/指纹/兼容/接收目录）─────────────────────
#[derive(Serialize, Deserialize, Default, Clone)]
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

/// 由启动时(setup)用 Tauri 的 app_data_dir 注入的可写配置目录。
/// 移动端(iOS/Android)沙盒只有 app 专属目录可写——必须靠它,否则证书写不进去 →
/// 每次启动重新生成指纹 → 被当成新设备。桌面端也用它,统一且正确。
static CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn init_config_dir(dir: PathBuf) {
    let _ = CONFIG_DIR.set(dir);
}

fn config_path() -> Option<PathBuf> {
    // 优先用启动时注入的 app_data_dir（各平台都正确、可写）
    if let Some(dir) = CONFIG_DIR.get() {
        return Some(dir.join("lan.json"));
    }
    // 回退（setup 未注入时）：按平台默认。
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join("Library/Application Support/com.baibao.toolbox/lan.json"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("APPDATA").ok()?;
        Some(PathBuf::from(base).join("com.baibao.toolbox").join("lan.json"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "windows")))]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".config/com.baibao.toolbox/lan.json"))
    }
}

/// TLS 证书文件路径（与 lan.json 同目录）：(cert.pem, key.pem, cert.fp)。
fn cert_paths() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let base = config_path()?.parent()?.to_path_buf();
    Some((base.join("cert.pem"), base.join("key.pem"), base.join("cert.fp")))
}

/// 读取本机自签证书；不存在则用 rcgen 生成并持久化。返回 (cert_pem, key_pem, cert_fp)。
/// cert_fp = sha256(证书 DER) 十六进制——对端只按这个指纹固定，不走链校验，故 SAN/CA 都无所谓。
fn load_or_create_cert() -> Result<(String, String, String), String> {
    let gen = || -> Result<(String, String, String), String> {
        let params = rcgen::CertificateParams::new(vec!["baibao.local".to_string()])
            .map_err(|e| format!("生成证书失败：{e}"))?;
        let key_pair = rcgen::KeyPair::generate().map_err(|e| format!("生成密钥失败：{e}"))?;
        let cert = params.self_signed(&key_pair).map_err(|e| format!("自签失败：{e}"))?;
        let fp = sha256_hex_bytes(cert.der().as_ref());
        Ok((cert.pem(), key_pair.serialize_pem(), fp))
    };
    let Some((cp, kp, fpp)) = cert_paths() else {
        return gen(); // 拿不到配置目录 → 临时证书（本次运行有效）
    };
    if let (Ok(c), Ok(k), Ok(f)) = (
        std::fs::read_to_string(&cp),
        std::fs::read_to_string(&kp),
        std::fs::read_to_string(&fpp),
    ) {
        if !c.trim().is_empty() && !k.trim().is_empty() && !f.trim().is_empty() {
            return Ok((c, k, f.trim().to_string()));
        }
    }
    let (cert_pem, key_pem, fp) = gen()?;
    if let Some(dir) = cp.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&cp, &cert_pem);
    let _ = std::fs::write(&kp, &key_pem);
    let _ = std::fs::write(&fpp, &fp);
    Ok((cert_pem, key_pem, fp))
}

// ── 共享密码静态加密 ────────────────────────────────────────
// lan.json 里不存明文密码：内存里仍是明文（显示/鉴权/自动生成都照常），
// 仅在「写文件时加密、读文件时解密」。密钥从机器标识派生、不写进任何文件，
// 因此把 lan.json 拷到别的机器/备份泄露都解不开。
const PW_ENC_PREFIX: &str = "enc1:";

/// 派生共享密码加密密钥：sha256("域分隔" || machine_id)。machine_id 取不到时退回固定串
/// （此时等价于轻度混淆——仍非明文，但安全性下降；正常 mac/win 都能取到 machine_id）。
fn share_pw_key() -> [u8; 32] {
    let seed = machine_id().unwrap_or_else(|| "baibao-fallback-seed".to_string());
    let mut h = Sha256::new();
    h.update(b"baibao-share-pw-key-v1\0");
    h.update(seed.as_bytes());
    h.finalize().into()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// 加密明文密码 → "enc1:hex(nonce):hex(密文)"。
fn encrypt_pw(plain: &str) -> String {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};
    let key = share_pw_key();
    let cipher = ChaCha20Poly1305::new((&key).into());
    let mut nonce = [0u8; 12];
    let _ = getrandom::getrandom(&mut nonce);
    match cipher.encrypt(Nonce::from_slice(&nonce), plain.as_bytes()) {
        Ok(ct) => format!("{PW_ENC_PREFIX}{}:{}", hex_encode(&nonce), hex_encode(&ct)),
        Err(_) => plain.to_string(), // 加密失败极罕见；退回原值以免丢密码
    }
}

/// 解密；非本格式（老的明文）原样返回。解不开（换机器/损坏）返回 None。
fn decrypt_pw(stored: &str) -> Option<String> {
    let Some(rest) = stored.strip_prefix(PW_ENC_PREFIX) else {
        return Some(stored.to_string()); // 老明文：原样当作密码（下次保存会自动加密）
    };
    let (nh, ch) = rest.split_once(':')?;
    let (nonce, ct) = (hex_decode(nh)?, hex_decode(ch)?);
    if nonce.len() != 12 {
        return None;
    }
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};
    let cipher = ChaCha20Poly1305::new((&share_pw_key()).into());
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct.as_slice())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

fn load_config() -> LanConfig {
    let mut cfg: LanConfig = config_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // 解密共享密码：解不开的（换了机器/损坏）丢弃密码 → 该共享变"无密码"故不对外服务，安全失败
    if let Some(shares) = cfg.shares.as_mut() {
        for s in shares.iter_mut() {
            if let Some(stored) = s.password.take() {
                s.password = decrypt_pw(&stored);
            }
        }
    }
    cfg
}

fn save_config(cfg: &LanConfig) {
    if let Some(p) = config_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 加密共享密码后再落盘（lan.json 不含明文）
        let mut cfg = cfg.clone();
        if let Some(shares) = cfg.shares.as_mut() {
            for s in shares.iter_mut() {
                if let Some(pw) = s.password.as_ref() {
                    s.password = Some(encrypt_pw(pw));
                }
            }
        }
        if let Ok(s) = serde_json::to_vec_pretty(&cfg) {
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
            protocol: Some("https".into()),
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
fn upsert_peer(app: &AppHandle, state: &LanState, info: &DeviceInfo, ip: String, sticky: bool) {
    if info.fingerprint.is_empty() {
        return;
    }
    {
        let mut g = state.0.lock().unwrap();
        if info.fingerprint == g.fingerprint {
            return; // 自己
        }
        // 一旦被手动登记为 sticky，后续多播刷新不应清掉该标记
        let prev = g.peers.get(&info.fingerprint);
        let was_sticky = prev.map(|p| p.sticky).unwrap_or(false);
        let ips = merge_ips(prev.map(|p| p.ips.as_slice()).unwrap_or(&[]), &ip);
        let peer = Peer {
            alias: info.alias.clone(),
            fingerprint: info.fingerprint.clone(),
            ip,
            ips,
            port: info.port.unwrap_or(PORT),
            protocol: info.protocol.clone().unwrap_or_else(|| "https".into()),
            device_type: info.device_type.clone(),
            is_baibao: info.app.as_deref() == Some("baibao"),
            last_seen_ms: now_ms(),
            shares: info.shares.unwrap_or(0),
            sticky: sticky || was_sticky,
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
                p.ips = merge_ips(&p.ips, &ip);
                p.ip = ip;
                p.last_seen_ms = now;
            }
            None => {
                g.peers.insert(
                    fingerprint.to_string(),
                    Peer {
                        alias: alias.to_string(),
                        fingerprint: fingerprint.to_string(),
                        ips: vec![ip.clone()],
                        ip,
                        port: PORT,
                        protocol: "https".into(),
                        device_type: None,
                        is_baibao: true,
                        last_seen_ms: now,
                        shares: 0,
                        sticky: false,
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

    // 0) 准备本机自签 TLS 证书（生成/复用并持久化），全程 HTTPS。
    //    设备身份(fingerprint) = 证书指纹：身份与证书绑定，别人无法用别的证书冒充本设备。
    let (cert_pem, key_pem, cert_fp) = match load_or_create_cert() {
        Ok(v) => v,
        Err(e) => {
            state.0.lock().unwrap().started = false;
            return Err(e);
        }
    };
    {
        let mut g = state.0.lock().unwrap();
        g.fingerprint = cert_fp;
        // 启动即落盘一次：把可能残留的明文共享密码重新加密写回（迁移旧 lan.json）
        persist(&g);
    }

    // 1) HTTPS 收件服务（自签证书；对端通过发现广播拿到本证书并「固定」）
    let ssl = tiny_http::SslConfig {
        certificate: cert_pem.into_bytes(),
        private_key: key_pem.into_bytes(),
    };
    let server = tiny_http::Server::https(("0.0.0.0", PORT), ssl).map_err(|e| {
        // 端口被占用时 started 已置 true，回滚，让用户可以重试启动
        state.0.lock().unwrap().started = false;
        format!("端口 {PORT} 被占用，局域网服务无法启动（可能已有一个百宝箱在运行）。请关闭占用该端口的程序后点「重试」。详情：{e}")
    })?;
    // 绑定成功（确认是本机唯一实例）后，清理上次崩溃可能残留的临时 zip
    cleanup_stale_zips(&state.0.lock().unwrap().download_dir.clone());
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

/// 本机所有非回环 IPv4 接口（含接口名与子网掩码）。主动扫描据此按「真实网段」枚举候选 IP，
/// 并可凭接口名区分真实 LAN 与 VPN/虚拟网卡（默认扫描只扫真实 LAN，VPN 网段由用户手动触发）。
fn local_ipv4_ifaces_masked() -> Vec<(String, Ipv4Addr, Ipv4Addr)> {
    if_addrs::get_if_addrs()
        .map(|ifs| {
            ifs.into_iter()
                .filter_map(|i| match i.addr {
                    if_addrs::IfAddr::V4(ref v4) if !v4.ip.is_loopback() && !v4.ip.is_unspecified() => {
                        Some((i.name.clone(), v4.ip, v4.netmask))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 列出某网段内可探测的主机 IP（去掉网络号/广播地址）。
/// 按真实掩码枚举：/28 只扫 14 台、/24 扫 254 台、/22 扫 1022 台。
/// 可用主机数超过 MAX_SUBNET_HOSTS（约 /21）或掩码退化（/31、/32）时，退回本机所在 /24 兜底，
/// 避免对超大网段（如 /16 = 6.5 万台）发起海量探测拖死扫描。
fn subnet_scan_targets(ip: Ipv4Addr, netmask: Ipv4Addr) -> Vec<Ipv4Addr> {
    const MAX_SUBNET_HOSTS: u32 = 2048; // 覆盖到 /21；更大网段退回 /24
    let o = ip.octets();
    let host_24 = || (1u8..=254).map(|h| Ipv4Addr::new(o[0], o[1], o[2], h)).collect::<Vec<_>>();
    let mask = u32::from(netmask);
    let prefix = mask.count_ones();
    if prefix == 0 || prefix >= 31 {
        return host_24();
    }
    let count = 1u32 << (32 - prefix); // 子网地址总数
    if count.saturating_sub(2) > MAX_SUBNET_HOSTS {
        return host_24();
    }
    let network = u32::from(ip) & mask;
    let broadcast = network | !mask;
    ((network + 1)..broadcast).map(Ipv4Addr::from).collect()
}

/// 解析 CIDR（如 "192.168.56.0/22"）为 (网络地址, 掩码)。供「手动扫描指定网段」用。
fn parse_cidr(cidr: &str) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let (addr, plen) = cidr.split_once('/')?;
    let ip: Ipv4Addr = addr.trim().parse().ok()?;
    let prefix: u8 = plen.trim().parse().ok()?;
    if prefix > 32 {
        return None;
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    Some((ip, Ipv4Addr::from(mask)))
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
    let Ok(out) = crate::tools::hidden_command("route").args(["print", "-4"]).output() else {
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
        upsert_peer(&app, &state, &info, ip, false);
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
            // sticky（手动扫描/+IP，多播不可见）不过期；其余按 TTL 清理
            g.peers.retain(|_, p| p.sticky || now.saturating_sub(p.last_seen_ms) < PEER_TTL_MS);
            before != g.peers.len()
        };
        if removed {
            emit_peers(&app, &state);
        }
        // 清理抗抖动握手的 offer：pending 超 5 分钟 → 超时（通知前端标记过期）；其余（已接收/拒绝）超 10 分钟 → 丢弃。
        let timed_out: Vec<String> = {
            let mut g = state.0.lock().unwrap();
            let now = Instant::now();
            let timed_out: Vec<String> = g
                .offers
                .values()
                .filter(|o| {
                    matches!(o.state, OfferState::Pending)
                        && now.duration_since(o.created) >= Duration::from_secs(300)
                })
                .map(|o| o.session_id.clone())
                .collect();
            g.offers.retain(|_, o| {
                !(matches!(o.state, OfferState::Pending)
                    && now.duration_since(o.created) >= Duration::from_secs(300))
                    && now.duration_since(o.created) < Duration::from_secs(600)
            });
            timed_out
        };
        for sid in timed_out {
            let _ = app.emit("lan://offer-timeout", serde_json::json!({ "sessionId": sid }));
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
                upsert_peer(&app, &state, &info, remote_ip, false);
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
            handle_share_upload(&app, &state, request, &query);
            Ok(())
        }
        ("POST", "/api/baibao/v1/share/upload-zip") => {
            handle_share_upload_zip(&app, &state, request, &query);
            Ok(())
        }
        ("GET", "/api/baibao/v1/share/upload-offset") => {
            handle_share_upload_offset(&state, request, &query);
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
        // 发送方在对方接收前撤销发送：撤回接收端的「待接收」，使其无法再点「接收」。
        ("POST", "/api/baibao/v1/cancel-offer") => {
            handle_cancel_offer(&app, &state, request);
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

/// 通道绑定鉴权令牌：把「密码哈希」与「对方 TLS 证书指纹」绑定。
/// auth_sha256 = sha256(密码) 的十六进制（前端算好传来 / 服务端用密码现算）；
/// cert_fp = sha256(对方证书 PEM)。两端只有连到「同一张证书」时算出的令牌才一致，
/// 中间人用自己的证书终止 TLS → 指纹不同 → 拿到的令牌对真服务端无效（且无密码算不出）。
fn bind_auth(auth_sha256: &str, cert_fp: &str) -> String {
    sha256_hex(&format!("{auth_sha256}:{cert_fp}"))
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
    Ok(String),     // 有密码且令牌正确
    Denied,         // 密码缺失/错误，或共享未设密码（未设密码一律拒绝对外服务）
    NotFound,
}

/// 校验某共享：未设密码 → 一律拒绝（安全基线，见阶段 3）；
/// 有密码且 X-Share-Auth == bind_auth(sha256(密码), 本机证书指纹) → Ok；否则 Denied。
fn authorize_share(g: &Inner, id: &str, auth: Option<&str>) -> ShareAuth {
    let Some(share) = g.shares.iter().find(|s| s.id == id) else {
        return ShareAuth::NotFound;
    };
    match &share.password {
        None => ShareAuth::Denied, // 未设密码：任何网络下都不对外服务（阶段 3）
        Some(pw) => {
            // 通道绑定：令牌必须 = bind_auth(sha256(密码), 本机身份)。本机身份=本机证书指纹，
            // 对端连进来用的就是这个指纹固定的，所以两端算出的令牌一致；中间人证书不同 → 不一致。
            let expected = bind_auth(&sha256_hex(pw), &g.fingerprint);
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

/// GET /api/baibao/v1/shares —— 列出共享根（仅元数据：id/名称/是否加锁）。
/// 阶段 3：只对外暴露「已设密码」的共享；未设密码的共享一律不对外服务、也不出现在列表里。
fn handle_share_roots(state: &LanState, request: tiny_http::Request) {
    let list: Vec<serde_json::Value> = state
        .0
        .lock()
        .unwrap()
        .shares
        .iter()
        .filter(|s| s.password.is_some())
        .map(|s| {
            serde_json::json!({
                "id": s.id, "name": s.name, "locked": true,
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
    // 排空可能存在的请求体（上传类 POST），避免提前响应导致对方发送阶段被重置。
    respond_text_drained(request, code, msg);
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
fn handle_share_upload(app: &AppHandle, state: &LanState, mut request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(id), Some(rel)) = (q.get("id"), q.get("path")) else {
        return respond_text_drained(request, 400, "missing params");
    };
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (root, perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    if !perms.0 {
        return respond_text_drained(request, 403, "无新增权限");
    }
    let Some(dest) = resolve_create_path(&root, rel) else {
        return respond_text_drained(request, 403, "bad path");
    };
    // 覆盖已存在的同名文件（覆盖确认在客户端做）；但拒绝把已存在的「目录」当文件覆盖
    if dest.is_dir() {
        return respond_text_drained(request, 409, "同名目录已存在");
    }

    // 断点续传：带 token 时累积到临时文件、按 offset 续写、集齐 total 再落地。
    if let Some(token) = q.get("token").map(|t| sanitize_token(t)).filter(|t| !t.is_empty()) {
        // 已被本机拒绝过的 token：直接拒收（发送方据此判定「对方拒绝」并停止）
        if state.0.lock().unwrap().share_rejected.contains(&token) {
            return respond_text_drained(request, 403, "rejected");
        }
        let offset = q.get("offset").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let total = q.get("total").and_then(|s| s.parse::<u64>().ok());
        let total_emit = total.unwrap_or(0);
        let name = q.get("name").cloned().unwrap_or_else(|| {
            dest.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| token.clone())
        });
        let part = upload_part_path(&token, false);
        let cur = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset > cur {
            // 客户端 offset 超前于服务端已收字节，无法续写，让其重新查询
            return respond_text_drained(request, 409, &cur.to_string());
        }
        // 注册拒绝标志 + 让接收方的任务面板出现该任务
        let reject = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        state.0.lock().unwrap().share_receives.insert(token.clone(), reject.clone());
        let _ = app.emit(
            "lan://share-incoming",
            serde_json::json!({ "token": token, "name": name, "received": offset, "total": total_emit, "phase": "recv" }),
        );
        let outcome = recv_append_tracked(&mut request, &part, offset, app, &token, &name, total_emit, &reject);
        state.0.lock().unwrap().share_receives.remove(&token);
        let now = match outcome {
            Ok(n) => n,
            Err(true) => {
                // 接收方主动拒绝：删 part + 标记 rejected + 通知面板
                let _ = std::fs::remove_file(&part);
                state.0.lock().unwrap().share_rejected.insert(token.clone());
                let _ = app.emit("lan://share-incoming", serde_json::json!({ "token": token, "name": name, "phase": "rejected" }));
                return respond_text(request, 403, "rejected");
            }
            Err(false) => return respond_text(request, 500, "接收中断"), // 连接断开：保留 part 供续传，不发终态
        };
        if let Some(t) = total {
            if now < t {
                return respond_text(request, 200, "incomplete"); // 还没传完，保留 part
            }
        }
        return match finalize_part_to(&part, &dest) {
            Ok(_) => {
                let _ = app.emit("lan://share-incoming", serde_json::json!({ "token": token, "name": name, "received": now, "total": now, "phase": "done" }));
                respond_text(request, 200, "ok")
            }
            Err(e) => respond_text(request, 500, &format!("写入失败：{e}")),
        };
    }

    // 旧客户端（无 token）：直接覆盖写入。
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

/// 上传文件夹（新增）：接收一个 zip（内含顶层文件夹）并解压到目标目录 path（rel，空=共享根）。
fn handle_share_upload_zip(app: &AppHandle, state: &LanState, mut request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(id), Some(rel)) = (q.get("id"), q.get("path")) else {
        return respond_text_drained(request, 400, "missing params");
    };
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (root, perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    if !perms.0 {
        return respond_text_drained(request, 403, "无新增权限");
    }
    let Some(target) = resolve_share_path(&root, rel) else {
        return respond_text_drained(request, 403, "bad path");
    };
    if !target.is_dir() {
        return respond_text_drained(request, 400, "目标不是目录");
    }

    // 断点续传：带 token 时累积到临时 zip、按 offset 续写、集齐 total 再解压。
    if let Some(token) = q.get("token").map(|t| sanitize_token(t)).filter(|t| !t.is_empty()) {
        if state.0.lock().unwrap().share_rejected.contains(&token) {
            return respond_text_drained(request, 403, "rejected");
        }
        let offset = q.get("offset").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let total = q.get("total").and_then(|s| s.parse::<u64>().ok());
        let total_emit = total.unwrap_or(0);
        let name = q.get("name").cloned().unwrap_or_else(|| token.clone());
        let part = upload_part_path(&token, true);
        let cur = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset > cur {
            return respond_text_drained(request, 409, &cur.to_string());
        }
        let reject = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        state.0.lock().unwrap().share_receives.insert(token.clone(), reject.clone());
        let _ = app.emit(
            "lan://share-incoming",
            serde_json::json!({ "token": token, "name": name, "received": offset, "total": total_emit, "phase": "recv" }),
        );
        let outcome = recv_append_tracked(&mut request, &part, offset, app, &token, &name, total_emit, &reject);
        state.0.lock().unwrap().share_receives.remove(&token);
        let now = match outcome {
            Ok(n) => n,
            Err(true) => {
                let _ = std::fs::remove_file(&part);
                state.0.lock().unwrap().share_rejected.insert(token.clone());
                let _ = app.emit("lan://share-incoming", serde_json::json!({ "token": token, "name": name, "phase": "rejected" }));
                return respond_text(request, 403, "rejected");
            }
            Err(false) => return respond_text(request, 500, "接收中断"), // 连接断开：保留 part 供续传
        };
        if let Some(t) = total {
            if now < t {
                return respond_text(request, 200, "incomplete"); // 还没传完，保留 part
            }
        }
        // 解压阶段也通知面板（大文件夹解压可能耗时）
        let _ = app.emit("lan://share-incoming", serde_json::json!({ "token": token, "name": name, "received": now, "total": now, "phase": "extract" }));
        let res = extract_zip_into(&part, &target);
        let _ = std::fs::remove_file(&part);
        return match res {
            Ok(_) => {
                let _ = app.emit("lan://share-incoming", serde_json::json!({ "token": token, "name": name, "received": now, "total": now, "phase": "done" }));
                respond_text(request, 200, "ok")
            }
            Err(_) => respond_text(request, 500, "解压失败"),
        };
    }

    // 旧客户端（无 token）：一次性接收整包再解压。
    let tmp = std::env::temp_dir().join(format!("baibao_recv_{}.zip", rand_hex(8)));
    {
        let mut f = match File::create(&tmp) {
            Ok(f) => f,
            Err(e) => return respond_text(request, 500, &format!("写入失败：{e}")),
        };
        if std::io::copy(&mut request.as_reader(), &mut f).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return respond_text(request, 500, "接收中断");
        }
    }
    let res = extract_zip_into(&tmp, &target);
    let _ = std::fs::remove_file(&tmp);
    match res {
        Ok(_) => respond_text(request, 200, "ok"),
        Err(_) => respond_text(request, 500, "解压失败"),
    }
}

/// 查询某个续传 token 在服务端已收到多少字节（客户端据此从断点继续）。返回纯数字文本。
fn handle_share_upload_offset(state: &LanState, request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(id), Some(token)) = (q.get("id"), q.get("token")) else {
        return respond_text(request, 400, "missing params");
    };
    let auth = header_value(&request, "X-Share-Auth");
    let ip = request.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (_root, perms) = match share_authorize(state, id, auth.as_deref(), &ip) {
        Ok(v) => v,
        Err(c) => return share_err_respond(request, c),
    };
    if !perms.0 {
        return respond_text(request, 403, "无新增权限");
    }
    let token = sanitize_token(token);
    if token.is_empty() {
        return respond_text(request, 200, "0");
    }
    // 该 token 已被接收方拒绝 → 410，发送方据此停止上传
    if state.0.lock().unwrap().share_rejected.contains(&token) {
        return respond_text(request, 410, "rejected");
    }
    // 单文件 .bin 或文件夹 .zip，取存在的那个
    let sz = std::fs::metadata(upload_part_path(&token, false))
        .or_else(|_| std::fs::metadata(upload_part_path(&token, true)))
        .map(|m| m.len())
        .unwrap_or(0);
    respond_text(request, 200, &sz.to_string());
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

/// 递归统计目录下所有文件的总字节数（仅 metadata，不读取内容），用于打包进度的分母。
fn dir_total_size(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_total_size(&p);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

/// 递归把目录 src 写入 zip，条目路径相对 base（base = src 的父目录，使压缩包内含顶层文件夹名）。
/// on_bytes 每写入一块就被调用一次（增量字节数），用于上报打包进度。
fn zip_add_dir(
    zw: &mut zip::ZipWriter<File>,
    base: &std::path::Path,
    dir: &std::path::Path,
    opts: zip::write::SimpleFileOptions,
    cancel: &std::sync::atomic::AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let name = rel.to_string_lossy().replace('\\', "/");
        if path.is_dir() {
            let _ = zw.add_directory(format!("{name}/"), opts);
            zip_add_dir(zw, base, &path, opts, cancel, on_bytes)?;
        } else {
            zw.start_file(name, opts)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let mut f = File::open(&path)?;
            // 分块拷贝，每块检查一次中断 —— 否则拷大文件时点「中断」要等整文件拷完才生效
            let mut buf = [0u8; 64 * 1024];
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
                }
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                zw.write_all(&buf[..n])?;
                on_bytes(n as u64);
            }
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
        let no_cancel = std::sync::atomic::AtomicBool::new(false);
        zip_add_dir(&mut zw, base, &dir, opts, &no_cancel, &mut |_n| {})?;
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
    upsert_peer(app, state, &req.info, remote_ip, false);

    // 发送端带 offerId → 走「立即应答 + 幂等」的抗抖动握手：不挂起连接，发送端断连后可后台重连重试。
    // 不带（LocalSend 旧端 / 旧版百宝箱）→ 下面的原阻塞式握手（兼容）。
    if req.offer_id.is_some() {
        return handle_prepare_upload_poll(app, state, request, req, is_baibao);
    }

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

/// 抗抖动握手（发送端带 offerId 时）：**立即应答**，绝不挂起连接等用户点接收。
/// - 首次见到该 offerId：建一条 Pending offer、弹一次「点击接收」，返回 `{status:"pending"}`。
/// - 之后对同一 offerId 的重试（发送端网络抖动后后台重连）：直接按当前状态幂等应答，不再弹窗。
///   pending → `{status:"pending"}`；已接收 → 200 + tokens；拒绝 → 403 declined；超 5 分钟 → 403 timeout。
fn handle_prepare_upload_poll(
    app: &AppHandle,
    state: &LanState,
    request: tiny_http::Request,
    req: PrepareUploadRequest,
    is_baibao: bool,
) {
    let offer_id = req.offer_id.clone().unwrap_or_default();

    enum Reply {
        Pending(String),
        Accepted(String, HashMap<String, String>),
        Declined,
        Timeout(String),
    }

    let reply = {
        let mut g = state.0.lock().unwrap();
        if let Some(off) = g.offers.get(&offer_id) {
            // 已有此 offer：按当前状态幂等应答（不重复弹窗）
            let sid = off.session_id.clone();
            let r = match &off.state {
                OfferState::Accepted(tokens) => Reply::Accepted(sid.clone(), tokens.clone()),
                OfferState::Declined => Reply::Declined,
                OfferState::Pending if off.created.elapsed() >= Duration::from_secs(300) => {
                    Reply::Timeout(sid.clone())
                }
                OfferState::Pending => Reply::Pending(sid.clone()),
            };
            // 终态（拒绝/超时）：清理掉，后续重试不会再命中
            if matches!(r, Reply::Declined | Reply::Timeout(_)) {
                g.offers.remove(&offer_id);
            }
            r
        } else {
            // 新 offer：建状态 + 弹一次确认框
            let session_id = rand_hex(12);
            g.offers.insert(
                offer_id.clone(),
                Offer {
                    created: Instant::now(),
                    session_id: session_id.clone(),
                    peer: req.info.fingerprint.clone(),
                    files: req.files.clone(),
                    state: OfferState::Pending,
                },
            );
            drop(g);
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
            Reply::Pending(session_id)
        }
    };

    match reply {
        Reply::Pending(sid) => respond_json(
            request,
            200,
            &serde_json::json!({ "status": "pending", "sessionId": sid }),
        ),
        Reply::Accepted(session_id, files) => {
            respond_json(request, 200, &PrepareUploadResponse { session_id, files })
        }
        Reply::Declined => respond_text(request, 403, "declined"),
        Reply::Timeout(sid) => {
            let _ = app.emit("lan://offer-timeout", serde_json::json!({ "sessionId": sid }));
            respond_text(request, 403, "timeout")
        }
    }
}

/// 发送方撤销发送（对方还没点「接收」时）：按 offerId 撤回该待接收 offer。
/// offerId 是发送方生成的 12 位随机串，等同于一次性能力令牌——知道它即有权撤销，无需额外鉴权。
/// 移除该 offer（及竞态下已生成的 session），并通知接收端把对应「待接收」标记为已取消。
fn handle_cancel_offer(app: &AppHandle, state: &LanState, mut request: tiny_http::Request) {
    #[derive(Deserialize)]
    struct CancelOfferReq {
        #[serde(rename = "offerId")]
        offer_id: String,
    }
    let req: CancelOfferReq = match read_json(&mut request) {
        Ok(v) => v,
        Err(_) => return respond_text(request, 400, "bad request"),
    };
    let sid = {
        let mut g = state.0.lock().unwrap();
        match g.offers.remove(&req.offer_id) {
            Some(off) => {
                // 若在并发竞态下对方刚好已接收：把生成的会话也清掉，使后续 upload 无 token 可用
                g.sessions.remove(&off.session_id);
                Some(off.session_id)
            }
            None => None,
        }
    };
    if let Some(sid) = sid {
        // 通知接收端把这些「待接收」标记为已取消并移除（无法再点「接收」/已接收的也归为取消）
        let _ = app.emit("lan://offer-cancelled", serde_json::json!({ "sessionId": sid }));
    }
    respond_text(request, 200, "ok");
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
        // 手动按 IP 添加：此刻还没有对方证书，只能先以「接受任意证书」探一次 /info
        // （TOFU）拿到对方证书；之后真正的传输都用从 /info 学到的证书做固定。
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(format!("https://{ip2}:{port}/api/localsend/v2/info"))
            .send()
            .map_err(|e| format!("连不上 {ip2}:{port}：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("对方返回 {}", resp.status()));
        }
        resp.json::<DeviceInfo>().map_err(|e| format!("对方不是有效设备：{e}"))
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))??;
    upsert_peer(&app, &state, &info, ip, true); // 手动 +IP：sticky，不被 TTL 清掉
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

fn sha256_hex_bytes(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    h.finalize().iter().map(|x| format!("{x:02x}")).collect()
}

/// 自定义 TLS 校验器：只认「证书指纹」(sha256(DER))，不走 webpki 链校验、不校验主机名。
/// 这样自签证书也能稳定固定；同时仍校验握手签名（确认对方持有该证书私钥，防冒充）。
#[derive(Debug)]
struct PinnedFp {
    expected: String, // 期望的 sha256(对方证书 DER) 十六进制
    algs: rustls::crypto::WebPkiSupportedAlgorithms,
}
impl rustls::client::danger::ServerCertVerifier for PinnedFp {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if sha256_hex_bytes(end_entity.as_ref()).eq_ignore_ascii_case(&self.expected) {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("证书指纹不匹配（可能存在中间人）".into()))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// 构造一个「只固定指定证书指纹」的 HTTPS 客户端。
fn build_pinned_client(
    expected_fp: &str,
    overall_timeout: Option<Duration>,
) -> Result<reqwest::blocking::Client, String> {
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let algs = provider.signature_verification_algorithms;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS 配置失败：{e}"))?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(PinnedFp {
            expected: expected_fp.to_string(),
            algs,
        }))
        .with_no_client_auth();
    let mut b = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        // 我们的服务端(tiny_http)只会 HTTP/1.1：强制 h1，避免 reqwest 默认尝试 HTTP/2 协商，
        // 在「prepare-upload 长轮询(服务端最长 300s 不返回)」这种连接上引发异常。
        .http1_only()
        // 保活长时间挂起、无数据流动的连接（长轮询等待对方接受时）。
        .tcp_keepalive(Duration::from_secs(30))
        .use_preconfigured_tls(config);
    if let Some(t) = overall_timeout {
        b = b.timeout(t);
    }
    b.build().map_err(|e| e.to_string())
}

/// 取连接某对端所需的参数（**不**构造客户端）：返回 (base_url, 本机别名, 对方身份指纹)。
/// 注意：reqwest::blocking::Client 必须在 `spawn_blocking` 内部用 `build_pinned_client` 构造并使用——
/// blocking client 析构时会关闭其内部 runtime，若在异步(tokio)上下文里 drop 会 panic
/// （Cannot drop a runtime in a context where blocking is not allowed）。
/// 对方身份指纹（=证书指纹）既用于 TLS 固定，也用于通道绑定鉴权(bind_auth)。
fn peer_conn(state: &LanState, fingerprint: &str) -> Result<(String, String, String), String> {
    let (base, my_alias, is_https) = {
        let g = state.0.lock().unwrap();
        let p = g.peers.get(fingerprint).ok_or("设备不在线或已离开")?;
        (
            format!("{}://{}:{}", p.protocol, p.ip, p.port),
            g.alias.clone(),
            p.protocol == "https",
        )
    };
    // 全 TLS：只允许 HTTPS 对端，拒绝任何明文回退（旧版/不支持加密的设备一律不连）
    if !is_https {
        return Err("对方不支持加密连接，请让对方更新到最新版".into());
    }
    if fingerprint.trim().is_empty() {
        return Err("对方身份未知，请稍候重试（等待设备广播）".into());
    }
    Ok((base, my_alias, fingerprint.to_string()))
}

/// 列出对端的共享根。
#[tauri::command]
pub async fn lan_share_roots(
    state: State<'_, LanState>,
    fingerprint: String,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    let (base, _, peer_fp) = peer_conn(&state, &fingerprint)?;
    tauri::async_runtime::spawn_blocking(move || {
        let client = build_pinned_client(&peer_fp, Some(Duration::from_secs(15)))?;
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
    let (base, _, peer_fp) = peer_conn(&state, &fingerprint)?;
    tauri::async_runtime::spawn_blocking(move || {
        let client = build_pinned_client(&peer_fp, Some(Duration::from_secs(15)))?;
        let mut req = client
            .get(format!("{base}/api/baibao/v1/share/list"))
            .query(&[("id", id.as_str()), ("path", path.as_str())]);
        if let Some(a) = auth {
            req = req.header("X-Share-Auth", bind_auth(&a, &peer_fp));
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
    let (base, _, peer_fp) = peer_conn(&state, &fingerprint)?;
    let dir = state.0.lock().unwrap().download_dir.clone();
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let client = build_pinned_client(&peer_fp, None)?; // 下载可能很大，不设整体超时
        let _ = std::fs::create_dir_all(&dir);
        let endpoint = if is_dir { "share/zip" } else { "share/download" };
        let mut req = client
            .get(format!("{base}/api/baibao/v1/{endpoint}"))
            .query(&[("id", id.as_str()), ("path", path.as_str())]);
        if let Some(a) = auth {
            req = req.header("X-Share-Auth", bind_auth(&a, &peer_fp));
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
    task_id: String,
    name: String,
    sent: u64,
    size: u64,
    last_emit: u64,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}
impl Read for ShareUploadReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.sent += n as u64;
            if self.sent - self.last_emit > 256 * 1024 || self.sent >= self.size {
                self.last_emit = self.sent;
                let _ = self.app.emit(
                    "lan://share-upload",
                    serde_json::json!({ "taskId": self.task_id, "name": self.name, "transferred": self.sent, "size": self.size, "phase": "upload" }),
                );
            }
        }
        Ok(n)
    }
}

/// 向对端查询某 token 已收到多少字节的结果。
/// 用来「确认对方有没有收到」+ 判断对方是否为支持断点续传的新版本。
enum OffsetProbe {
    Bytes(u64),   // 对方已收到的字节数（>0 即确实收到了）
    Rejected,     // 对方已点「拒绝」（410）
    Unsupported,  // 对方没有该接口（旧版本，不保存已收数据、不支持续传）
    Unreachable,  // 连不上对方（网络问题）
}

fn probe_upload_offset(
    client: &reqwest::blocking::Client,
    base: &str,
    id: &str,
    token: &str,
    auth: Option<&String>,
    peer_fp: &str,
) -> OffsetProbe {
    let mut r = client
        .get(format!("{base}/api/baibao/v1/share/upload-offset"))
        .query(&[("id", id), ("token", token)]);
    if let Some(a) = auth {
        r = r.header("X-Share-Auth", bind_auth(a, peer_fp));
    }
    match r.send() {
        Ok(resp) if resp.status().is_success() => resp
            .text()
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .map(OffsetProbe::Bytes)
            .unwrap_or(OffsetProbe::Unsupported),
        Ok(resp) if resp.status().as_u16() == 410 => OffsetProbe::Rejected,
        Ok(_) => OffsetProbe::Unsupported, // 404 等：对方没有此接口
        Err(_) => OffsetProbe::Unreachable,
    }
}

/// 等待 secs 秒，期间每 100ms 检查一次取消标志；被取消立即返回 false。
fn wait_cancellable(secs: u64, cancel: &Arc<std::sync::atomic::AtomicBool>) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    for _ in 0..(secs * 10) {
        if cancel.load(Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if f >= KB * KB * KB {
        format!("{:.2} GB", f / (KB * KB * KB))
    } else if f >= KB * KB {
        format!("{:.1} MB", f / (KB * KB))
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

/// 上传本地文件或文件夹到对端共享目录（新增）。dest_dir = 目标目录相对路径（空=共享根）。
/// 文件夹会在本地打包成 zip 上传、对端解压（支持任意层级）。带进度上报。
#[tauri::command]
pub async fn lan_share_upload(
    app: AppHandle,
    state: State<'_, LanState>,
    fingerprint: String,
    id: String,
    task_id: String,
    dest_dir: String,
    local_path: String,
    auth: Option<String>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, _, peer_fp) = peer_conn(&state, &fingerprint)?;
    let name = local_path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .to_string();
    // 注册该任务的中断标志
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    state.0.lock().unwrap().share_uploads.insert(task_id.clone(), cancel.clone());

    let (capp, ctid, cname) = (app.clone(), task_id.clone(), name.clone());
    let res = tauri::async_runtime::spawn_blocking(move || {
        use std::sync::atomic::Ordering::Relaxed;
        let client = build_pinned_client(&peer_fp, None)?; // 上传可能很大，不设整体超时
        let lp = std::path::Path::new(&local_path);
        let is_dir = lp.is_dir();
        // 续传 token：用任务 id（服务端据此累积已收字节）。
        let token = sanitize_token(&ctid);
        let (send_path, endpoint, query_path, cleanup_zip): (PathBuf, &str, String, bool) = if is_dir {
            // 文件夹打包成确定路径的 zip；存在即代表上次已构建完成（出错时保留），
            // 续传时直接复用，保证字节一致（重新打包可能字节不同→续写会损坏）。
            let zip_final = std::env::temp_dir().join(format!("baibao_send_{token}.zip"));
            if zip_final.is_file() {
                let sz = std::fs::metadata(&zip_final).map(|m| m.len()).unwrap_or(0);
                let _ = capp.emit(
                    "lan://share-upload",
                    serde_json::json!({ "taskId": ctid, "name": cname, "transferred": sz, "size": sz, "phase": "zip" }),
                );
            } else {
                // 先统计总字节作为打包进度的分母，并发一条初始「打包中 0%」
                let total = dir_total_size(lp);
                let _ = capp.emit(
                    "lan://share-upload",
                    serde_json::json!({ "taskId": ctid, "name": cname, "transferred": 0u64, "size": total, "phase": "zip" }),
                );
                let building = std::env::temp_dir().join(format!("baibao_send_{token}.building"));
                let base_dir = lp.parent().unwrap_or(lp);
                let build = (|| -> std::io::Result<()> {
                    let file = File::create(&building)?;
                    let mut zw = zip::ZipWriter::new(file);
                    let opts = zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated);
                    let _ = zw.add_directory(format!("{cname}/"), opts);
                    // 打包进度：每累计 ~512KB 上报一次
                    let mut packed = 0u64;
                    let mut last = 0u64;
                    let mut on_bytes = |n: u64| {
                        packed += n;
                        if packed - last > 512 * 1024 || packed >= total {
                            last = packed;
                            let _ = capp.emit(
                                "lan://share-upload",
                                serde_json::json!({ "taskId": ctid, "name": cname, "transferred": packed, "size": total, "phase": "zip" }),
                            );
                        }
                    };
                    zip_add_dir(&mut zw, base_dir, lp, opts, &cancel, &mut on_bytes)?;
                    zw.finish()
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    Ok(())
                })();
                if build.is_err() {
                    let _ = std::fs::remove_file(&building);
                    if cancel.load(Relaxed) {
                        return Err("cancelled".to_string());
                    }
                    return Err("打包文件夹失败".to_string());
                }
                // 构建完成才落到最终路径——「存在即完整」，续传复用才安全
                if let Err(e) = std::fs::rename(&building, &zip_final) {
                    let _ = std::fs::remove_file(&building);
                    return Err(format!("打包文件夹失败：{e}"));
                }
            }
            (zip_final, "share/upload-zip", dest_dir.clone(), true)
        } else {
            let qp = if dest_dir.is_empty() {
                cname.clone()
            } else {
                format!("{dest_dir}/{cname}")
            };
            (lp.to_path_buf(), "share/upload", qp, false)
        };

        let size = std::fs::metadata(&send_path).map_err(|e| format!("读取失败：{e}"))?.len();
        let total_s = size.to_string();

        // 发送（带断点续传）：失败不立即放弃。
        // 规则：只要本次尝试相比开始时「有新数据送达对方」就算有进展、重置失败计数；
        // 只有「连续 2 次毫无进展」才判定中断。每次重试前等待 1 秒（可被取消打断）。
        const MAX_STALL_RETRIES: u32 = 2; // 连续无进展的最大重试次数
        const RETRY_WAIT_SECS: u64 = 1;
        let mut stalls = 0u32; // 连续无进展次数
        let outcome: Result<(), String> = loop {
            if cancel.load(Relaxed) {
                break Err("cancelled".to_string());
            }
            // 1) 探测对方已收字节，作为本次断点 + 进展基准
            let before: u64 = match probe_upload_offset(&client, &base, &id, &token, auth.as_ref(), &peer_fp) {
                OffsetProbe::Bytes(n) => n.min(size),
                OffsetProbe::Rejected => break Err("上传已停止：对方拒绝接收。".to_string()),
                OffsetProbe::Unreachable => {
                    // 连不上：直接计一次无进展，避免白等 8 秒连接超时
                    stalls += 1;
                    if stalls >= MAX_STALL_RETRIES {
                        break Err("上传中断：与对方网络连接已断开，请检查网络后点「继续」重试。".to_string());
                    }
                    if !wait_cancellable(RETRY_WAIT_SECS, &cancel) {
                        break Err("cancelled".to_string());
                    }
                    continue;
                }
                OffsetProbe::Unsupported => 0, // 旧版无 offset 接口：从 0 起发，靠失败分类兜底
            };

            // 2) 打开 + 定位到断点 + 发送
            let mut f = match File::open(&send_path) {
                Ok(f) => f,
                Err(e) => break Err(format!("打开失败：{e}")),
            };
            if before > 0 {
                use std::io::{Seek, SeekFrom};
                if let Err(e) = f.seek(SeekFrom::Start(before)) {
                    break Err(format!("定位失败：{e}"));
                }
            }
            let reader = ShareUploadReader {
                inner: f,
                app: capp.clone(),
                task_id: ctid.clone(),
                name: cname.clone(),
                sent: before,
                size,
                last_emit: before,
                cancel: cancel.clone(),
            };
            let body = reqwest::blocking::Body::sized(reader, size - before);
            let offset_s = before.to_string();
            let mut req = client
                .post(format!("{base}/api/baibao/v1/{endpoint}"))
                .query(&[
                    ("id", id.as_str()),
                    ("path", query_path.as_str()),
                    ("token", token.as_str()),
                    ("offset", offset_s.as_str()),
                    ("total", total_s.as_str()),
                    ("name", cname.as_str()), // 让接收方任务面板显示文件名
                ])
                .body(body);
            if let Some(a) = &auth {
                req = req.header("X-Share-Auth", bind_auth(a, &peer_fp));
            }

            // 3) 成功（HTTP 拿到响应）→ 按状态码定结果
            if let Ok(resp) = req.send() {
                break share_op_result(resp.status().as_u16());
            }
            if cancel.load(Relaxed) {
                break Err("cancelled".to_string());
            }

            // 4) 失败：再探测，判断本次相比开始时是否「有新数据送达」+ 分类
            let (made_progress, fail_msg) = match probe_upload_offset(&client, &base, &id, &token, auth.as_ref(), &peer_fp) {
                OffsetProbe::Rejected => break Err("上传中断：对方拒绝接收。".to_string()),
                OffsetProbe::Unsupported => {
                    break Err("上传中断：对方版本过旧、不保存已收数据，不支持断点续传，请让对方更新到最新版后重试。".to_string())
                }
                OffsetProbe::Unreachable => (
                    false,
                    "上传中断：与对方网络连接已断开，请检查网络后点「继续」重试。".to_string(),
                ),
                OffsetProbe::Bytes(after) => (
                    after > before, // 有新字节送达对方 = 有进展
                    format!(
                        "上传中断：对方已收到 {} / {}，点「继续」可从断点续传。",
                        human_bytes(after.min(size)),
                        human_bytes(size)
                    ),
                ),
            };

            // 5) 有进展 → 重置失败计数；无进展 → 累加，连续 2 次无进展才判定中断
            if made_progress {
                stalls = 0;
            } else {
                stalls += 1;
                if stalls >= MAX_STALL_RETRIES {
                    break Err(fail_msg);
                }
            }

            // 6) 等 1 秒再重试（可被取消打断）
            if !wait_cancellable(RETRY_WAIT_SECS, &cancel) {
                break Err("cancelled".to_string());
            }
        };

        if outcome == Err("cancelled".to_string()) && cleanup_zip {
            // 用户主动取消：删除本地 zip（不会再续传此任务）
            let _ = std::fs::remove_file(&send_path);
        }
        // 仅成功才清理本地 zip；失败（非取消）保留以便续传复用
        if outcome.is_ok() && cleanup_zip {
            let _ = std::fs::remove_file(&send_path);
        }
        outcome
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?;

    // 注销中断标志 + 广播终态（done/cancelled/error），让常驻任务面板更新
    state.0.lock().unwrap().share_uploads.remove(&task_id);
    let phase = match &res {
        Ok(_) => "done",
        Err(e) if e == "cancelled" => "cancelled",
        Err(_) => "error",
    };
    // 失败时把原因一并带上，让任务面板（不只是浏览弹框）也能显示「对方已收到 X」等
    let errmsg = match &res {
        Err(e) if e != "cancelled" => Some(e.clone()),
        _ => None,
    };
    let _ = app.emit(
        "lan://share-upload",
        serde_json::json!({ "taskId": task_id, "name": name, "phase": phase, "error": errmsg }),
    );
    res
}

/// 判断一批本地路径里哪些是目录（前端用于「上传文件夹需二次确认」）。
#[tauri::command]
pub fn lan_dir_flags(paths: Vec<String>) -> Vec<bool> {
    paths
        .iter()
        .map(|p| std::path::Path::new(p).is_dir())
        .collect()
}

/// 中断指定共享上传任务（打包/传输都会在下一次检查时停止）。
#[tauri::command]
pub fn lan_share_upload_cancel(state: State<'_, LanState>, task_id: String) {
    if let Some(c) = state.0.lock().unwrap().share_uploads.get(&task_id) {
        c.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 接收方「拒绝」某个上传 token：中断当前接收 + 标记拒绝 + 删除已收分片。
/// 标记后发送方查询 offset 会得到 410，从而判定「对方拒绝」并停止。
#[tauri::command]
pub fn lan_share_receive_reject(app: AppHandle, state: State<'_, LanState>, token: String) {
    let token = sanitize_token(&token);
    if token.is_empty() {
        return;
    }
    {
        let mut g = state.0.lock().unwrap();
        if let Some(r) = g.share_receives.get(&token) {
            r.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        g.share_rejected.insert(token.clone());
    }
    // 即使当前没有活跃接收（两次续传之间的空档），也删掉分片，阻止后续续传
    let _ = std::fs::remove_file(upload_part_path(&token, false));
    let _ = std::fs::remove_file(upload_part_path(&token, true));
    let _ = app.emit("lan://share-incoming", serde_json::json!({ "token": token, "phase": "rejected" }));
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
    let (base, _, peer_fp) = peer_conn(&state, &fingerprint)?;
    tauri::async_runtime::spawn_blocking(move || {
        let client = build_pinned_client(&peer_fp, Some(Duration::from_secs(15)))?;
        let mut query: Vec<(&str, &str)> = vec![("id", id.as_str()), ("path", path.as_str())];
        if let Some(t) = &to {
            query.push(("to", t.as_str()));
        }
        let mut req = client
            .post(format!("{base}/api/baibao/v1/share/{op}"))
            .query(&query);
        if let Some(a) = auth {
            req = req.header("X-Share-Auth", bind_auth(&a, &peer_fp));
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
        404 => Err("对方不支持该操作（百宝箱版本过旧，请更新到最新版）".into()),
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
    // 优先处理抗抖动握手的 offer（按 sessionId 反查 offerId）：把决定写进 offer 状态，
    // 由发送端的下一次轮询取走——这样即使确认时发送端那条连接正巧断着，重连后也能拿到结果。
    {
        let mut g = state.0.lock().unwrap();
        let oid = g
            .offers
            .iter()
            .find(|(_, o)| o.session_id == session_id)
            .map(|(k, _)| k.clone());
        if let Some(oid) = oid {
            if !accept {
                if let Some(o) = g.offers.get_mut(&oid) {
                    o.state = OfferState::Declined;
                }
                return Ok(());
            }
            // 接收：为选中的文件生成 token，写入 sessions，再把 offer 标记为已接收
            let (peer, files, sid) = {
                let o = g.offers.get(&oid).unwrap();
                (o.peer.clone(), o.files.clone(), o.session_id.clone())
            };
            let mut tokens = HashMap::new();
            let mut slots = HashMap::new();
            for (fid, meta) in &files {
                if file_ids.is_empty() || file_ids.contains(fid) {
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
                if let Some(o) = g.offers.get_mut(&oid) {
                    o.state = OfferState::Declined;
                }
                return Ok(());
            }
            g.sessions.insert(sid, Session { peer, files: slots });
            if let Some(o) = g.offers.get_mut(&oid) {
                o.state = OfferState::Accepted(tokens);
            }
            return Ok(());
        }
    }
    // 否则走旧的阻塞式 decisions 通道（LocalSend 兼容 / 旧版）
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
    // 原生目录选择器：选「接收文件的保存目录」。blocking_pick_folder 仅桌面有；
    // 移动端无系统目录选择器（走 SAF/文件 App），此命令暂不支持。
    #[cfg(desktop)]
    {
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
    #[cfg(not(desktop))]
    {
        let _ = app;
        Err("移动端暂不支持选择目录".into())
    }
}


#[tauri::command]
pub async fn lan_reveal(app: AppHandle, path: String) -> Result<(), String> {
    // 在系统文件管理器中定位文件（Finder / 资源管理器 / 文件管理器）。
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| format!("打开失败：{e}"))
}

/// 在系统文件管理器中「打开」目录本身（展示其内容），用于共享目录的快捷打开。
#[tauri::command]
pub async fn lan_open_path(app: AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| format!("打开失败：{e}"))
}

// ── 代理（SOCKS5 over 百宝箱 TLS）─────────────────────────────
// 用途：A 装了公司 VPN（如飞连），B 没装；B 把浏览器/工具的代理指向 B 本机的 SOCKS5，
// 流量经百宝箱 TLS 隧道发到 A，A 用「普通连接」(操作系统已经过飞连路由) 访问目标并回传。
// A=服务端(出口)，B=客户端(本地 SOCKS5 入口)；二者互斥；默认关闭、不持久化。
// A 端不开第二个 VPN（只发普通请求），故不会与飞连冲突。
const PROXY_PORT: u16 = 53318; // A 端 TLS 隧道监听端口
const SOCKS_PORT: u16 = 53319; // B 端本地 SOCKS5 端口
const HTTP_PROXY_PORT: u16 = 53320; // B 端本地 HTTP 代理端口（Windows 系统代理指向它）

/// 活跃连接计数的 RAII 守卫：建立隧道(进入 copy 阶段)时 +1，连接结束/被 abort 析构时 -1。
struct ConnGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl ConnGuard {
    fn new(c: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ConnGuard(c.clone())
    }
}
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 用 socket2 建可复用地址的监听器（避免快速重启 bind 冲突），转成 tokio 非阻塞监听。
fn bind_reuse(port: u16, loopback_only: bool) -> std::io::Result<std::net::TcpListener> {
    use socket2::{Domain, Socket, Type};
    let addr: std::net::SocketAddr = if loopback_only {
        ([127, 0, 0, 1], port).into()
    } else {
        ([0, 0, 0, 0], port).into()
    };
    let sock = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    sock.set_reuse_address(true)?;
    sock.bind(&addr.into())?;
    sock.listen(128)?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

fn proxy_server_config(cert_pem: &str, key_pem: &str) -> Result<std::sync::Arc<rustls::ServerConfig>, String> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer> = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("证书解析失败：{e}"))?;
    let key: PrivateKeyDer = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .map_err(|e| format!("私钥解析失败：{e}"))?
        .ok_or("私钥为空")?;
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS 版本错误：{e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("加载证书失败：{e}"))?;
    Ok(std::sync::Arc::new(cfg))
}

fn proxy_client_config(expected_fp: &str) -> std::sync::Arc<rustls::ClientConfig> {
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let algs = provider.signature_verification_algorithms;
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(PinnedFp {
            expected: expected_fp.to_string(),
            algs,
        }))
        .with_no_client_auth();
    std::sync::Arc::new(cfg)
}

/// 服务端转发的运行指标：活跃连接数 / 累计字节 / 访问过的目标（仅记录展示，不解码报文）。
#[derive(Clone)]
struct ServeCtx {
    conns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    hosts: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (u64, u128)>>>,
}

/// 双向转发并累计字节数（替代 copy_bidirectional，用于实时流量统计）。
async fn relay_counted(
    tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    upstream: tokio::net::TcpStream,
    bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut cr, mut cw) = tokio::io::split(tls);
    let (mut sr, mut sw) = tokio::io::split(upstream);
    let b_up = bytes.clone();
    let up = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match cr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    b_up.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
        let _ = sw.shutdown().await;
    };
    let down = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match sr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if cw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    bytes.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
        let _ = cw.shutdown().await;
    };
    tokio::join!(up, down);
}

/// A 端：TLS 隧道服务器。收到 [鉴权][目标host][目标port] 后，连接目标并双向转发。
/// 监听器由调用方(命令)同步绑定好再传入，便于把「端口被占用/被拦」的错误直接报给前端。
async fn run_proxy_server(
    listener: tokio::net::TcpListener,
    cfg: std::sync::Arc<rustls::ServerConfig>,
    expected_auth: String,
    ctx: ServeCtx,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
    // 用 JoinSet 持有所有子连接任务：停止时本任务被 abort → JoinSet 析构 → 所有连接一并被强制断开。
    let mut conns: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            r = listener.accept() => {
                if let Ok((tcp, _)) = r {
                    let (acceptor, auth, ctx) = (acceptor.clone(), expected_auth.clone(), ctx.clone());
                    conns.spawn(async move {
                        let _ = proxy_serve_conn(acceptor, tcp, &auth, &ctx).await;
                    });
                }
            }
            Some(_) = conns.join_next() => {} // 回收已结束的连接任务，避免堆积
        }
    }
}

async fn proxy_serve_conn(
    acceptor: tokio_rustls::TlsAcceptor,
    tcp: tokio::net::TcpStream,
    expected_auth: &str,
    ctx: &ServeCtx,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // 握手也加超时，防 TLS 半开连接堆积
    let mut tls = match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
        Ok(Ok(s)) => s,
        _ => return Ok(()),
    };
    // 读隧道头(鉴权+目标)加超时，防「只连不发」的慢速连接长期占用任务
    let header = tokio::time::timeout(Duration::from_secs(10), async {
        let alen = tls.read_u16().await? as usize;
        if alen == 0 || alen > 256 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad auth len"));
        }
        let mut abuf = vec![0u8; alen];
        tls.read_exact(&mut abuf).await?;
        let hlen = tls.read_u8().await? as usize;
        if hlen == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad host len"));
        }
        let mut hbuf = vec![0u8; hlen];
        tls.read_exact(&mut hbuf).await?;
        let port = tls.read_u16().await?;
        Ok::<_, std::io::Error>((abuf, hbuf, port))
    })
    .await;
    let (abuf, hbuf, port) = match header {
        Ok(Ok(v)) => v,
        _ => return Ok(()),
    };
    if String::from_utf8_lossy(&abuf) != expected_auth {
        tokio::time::sleep(Duration::from_millis(800)).await; // 拖慢在线爆破访问密码
        let _ = tls.write_u8(1).await; // 鉴权失败
        return Ok(());
    }
    let host = String::from_utf8_lossy(&hbuf).to_string();
    let upstream = match tokio::net::TcpStream::connect((host.as_str(), port)).await {
        Ok(s) => s,
        Err(_) => {
            let _ = tls.write_u8(2).await; // 连接目标失败
            return Ok(());
        }
    };
    tls.write_u8(0).await?; // ok
    tls.flush().await?;
    // 记录访问的目标（仅 host:port + 次数/时间，不看内容），供「请求走了哪些网址」展示
    if let Ok(mut h) = ctx.hosts.lock() {
        if h.len() >= 1000 {
            // 软上限：超量时清掉最旧的一个，避免无限增长
            if let Some(oldest) = h.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| k.clone()) {
                h.remove(&oldest);
            }
        }
        let e = h.entry(format!("{host}:{port}")).or_insert((0, 0));
        e.0 += 1;
        e.1 = now_ms();
    }
    let _guard = ConnGuard::new(&ctx.conns); // 进入转发阶段才计数（排除握手/鉴权失败/探测）
    relay_counted(tls, upstream, ctx.bytes.clone()).await;
    Ok(())
}

/// B 端：本地 SOCKS5 服务器。每个连接解析目标后，经 TLS 隧道发给 A。
async fn run_socks_server(
    listener: tokio::net::TcpListener,
    server_ip: String,
    server_port: u16,
    cfg: std::sync::Arc<rustls::ClientConfig>,
    auth: String,
    counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let connector = tokio_rustls::TlsConnector::from(cfg);
    // JoinSet 持有所有子连接：停止时本任务被 abort → 一并强制断开所有代理连接。
    let mut conns: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            r = listener.accept() => {
                if let Ok((tcp, _)) = r {
                    let (ip, conn, au, counter) = (server_ip.clone(), connector.clone(), auth.clone(), counter.clone());
                    conns.spawn(async move {
                        let _ = socks_handle(tcp, ip, server_port, conn, au, &counter).await;
                    });
                }
            }
            Some(_) = conns.join_next() => {} // 回收已结束的连接任务
        }
    }
}

/// B 端：本地 HTTP 代理（支持 CONNECT 隧道 + 普通 HTTP 转发），经 TLS 隧道发给 A。
/// Windows 系统代理对「HTTP 代理」支持可靠（SOCKS 经常不被网页流量采用），故系统代理模式指向它。
async fn run_http_proxy_server(
    listener: tokio::net::TcpListener,
    server_ip: String,
    server_port: u16,
    cfg: std::sync::Arc<rustls::ClientConfig>,
    auth: String,
    counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let connector = tokio_rustls::TlsConnector::from(cfg);
    let mut conns: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            r = listener.accept() => {
                if let Ok((tcp, _)) = r {
                    let (ip, conn, au, counter) = (server_ip.clone(), connector.clone(), auth.clone(), counter.clone());
                    conns.spawn(async move {
                        let _ = http_proxy_handle(tcp, ip, server_port, conn, au, &counter).await;
                    });
                }
            }
            Some(_) = conns.join_next() => {}
        }
    }
}

/// 从 "host:port" / "host" 解析主机与端口（无端口用默认）。
fn parse_authority(s: &str, default_port: u16) -> (String, u16) {
    if let Some(idx) = s.rfind(':') {
        if let Ok(port) = s[idx + 1..].parse::<u16>() {
            return (s[..idx].to_string(), port);
        }
    }
    (s.to_string(), default_port)
}

async fn http_proxy_handle(
    mut sock: tokio::net::TcpStream,
    server_ip: String,
    server_port: u16,
    connector: tokio_rustls::TlsConnector,
    auth: String,
    counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // 读到请求头结束(\r\n\r\n)或上限
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }
    let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(buf.len());
    let header = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = header.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut it = first.split_whitespace();
    let method = it.next().unwrap_or("");
    let target = it.next().unwrap_or("");
    let connect = method.eq_ignore_ascii_case("CONNECT");
    let host_hdr = lines
        .find(|l| l.to_ascii_lowercase().starts_with("host:"))
        .map(|l| l[5..].trim().to_string());

    let (host, port) = if connect {
        parse_authority(target, 443) // CONNECT host:port
    } else {
        // 普通 HTTP：请求行是绝对 URI（http://host[:port]/path），取其 authority；缺则用 Host 头
        let rest = target
            .strip_prefix("http://")
            .or_else(|| target.strip_prefix("https://"))
            .unwrap_or(target);
        let authority = rest.split('/').next().unwrap_or("");
        if authority.is_empty() {
            match host_hdr {
                Some(h) => parse_authority(&h, 80),
                None => return Ok(()),
            }
        } else {
            parse_authority(authority, 80)
        }
    };
    if host.is_empty() {
        let _ = sock.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        return Ok(());
    }

    match tunnel_open(&server_ip, server_port, &connector, &auth, &host, port).await {
        Ok(mut tls) => {
            if connect {
                sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
            } else {
                tls.write_all(&buf).await?; // 普通 HTTP：把已读请求原样转发给上游
            }
            let _guard = ConnGuard::new(counter);
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut tls).await;
        }
        Err(_) => {
            let _ = sock.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
        }
    }
    Ok(())
}

/// 从流里读到 NUL 为止（SOCKS4 的 USERID / SOCKS4a 的 hostname 都以 \0 结尾）。
async fn read_until_nul(sock: &mut tokio::net::TcpStream) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    loop {
        let b = sock.read_u8().await?;
        if b == 0 {
            break;
        }
        buf.push(b);
        if buf.len() > 255 {
            break; // 防御：异常超长输入
        }
    }
    Ok(buf)
}

/// 经百宝箱 TLS 隧道把 (host,port) 连到服务端。成功返回 TLS 流；失败返回错误码：
/// 1=连不上服务端/证书不符，2=访问密码错，5=服务端连不上目标。
async fn tunnel_open(
    server_ip: &str,
    server_port: u16,
    connector: &tokio_rustls::TlsConnector,
    auth: &str,
    host: &str,
    port: u16,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if host.as_bytes().len() > 255 || host.is_empty() {
        return Err(1);
    }
    let tcp = tokio::net::TcpStream::connect((server_ip, server_port)).await.map_err(|_| 1u8)?;
    let dns = rustls::pki_types::ServerName::try_from("baibao.local").map_err(|_| 1u8)?.to_owned();
    let mut tls = connector.connect(dns, tcp).await.map_err(|_| 1u8)?;
    let (ab, hb) = (auth.as_bytes(), host.as_bytes());
    tls.write_u16(ab.len() as u16).await.map_err(|_| 1u8)?;
    tls.write_all(ab).await.map_err(|_| 1u8)?;
    tls.write_u8(hb.len() as u8).await.map_err(|_| 1u8)?;
    tls.write_all(hb).await.map_err(|_| 1u8)?;
    tls.write_u16(port).await.map_err(|_| 1u8)?;
    tls.flush().await.map_err(|_| 1u8)?;
    match tls.read_u8().await {
        Ok(0) => Ok(tls),
        Ok(1) => Err(2), // 鉴权失败
        _ => Err(5),     // 目标连不上
    }
}

/// 本机 SOCKS 入口处理。同时支持 SOCKS5 和 SOCKS4/4a——
/// Windows 系统代理(注册表 socks=)会让 Chrome 用 SOCKS4，故必须兼容，否则系统代理模式不可用。
async fn socks_handle(
    mut sock: tokio::net::TcpStream,
    server_ip: String,
    server_port: u16,
    connector: tokio_rustls::TlsConnector,
    auth: String,
    counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ver = sock.read_u8().await?;
    // SOCKS4 应答 8 字节：VN=0, CD(0x5A 成功/0x5B 失败), DSTPORT(2), DSTIP(4)
    let socks4_reply = |ok: bool| -> [u8; 8] { [0, if ok { 0x5A } else { 0x5B }, 0, 0, 0, 0, 0, 0] };

    let (host, port) = match ver {
        5 => {
            // SOCKS5：版本 + 方法协商（只支持无认证）
            let nm = sock.read_u8().await? as usize;
            let mut methods = vec![0u8; nm];
            sock.read_exact(&mut methods).await?;
            sock.write_all(&[5, 0]).await?;
            // 请求：VER CMD RSV ATYP ...
            let mut head = [0u8; 4];
            sock.read_exact(&mut head).await?;
            if head[0] != 5 || head[1] != 1 {
                sock.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?; // 不支持的命令
                return Ok(());
            }
            let host = match head[3] {
                1 => {
                    let mut a = [0u8; 4];
                    sock.read_exact(&mut a).await?;
                    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
                }
                3 => {
                    let l = sock.read_u8().await? as usize;
                    let mut d = vec![0u8; l];
                    sock.read_exact(&mut d).await?;
                    String::from_utf8_lossy(&d).to_string()
                }
                4 => {
                    let mut a = [0u8; 16];
                    sock.read_exact(&mut a).await?;
                    std::net::Ipv6Addr::from(a).to_string()
                }
                _ => {
                    sock.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?; // 地址类型不支持
                    return Ok(());
                }
            };
            let port = sock.read_u16().await?;
            (host, port)
        }
        4 => {
            // SOCKS4 / 4a：CD, DSTPORT(2), DSTIP(4), USERID\0, [4a 时 hostname\0]
            let cd = sock.read_u8().await?;
            let port = sock.read_u16().await?;
            let mut ip = [0u8; 4];
            sock.read_exact(&mut ip).await?;
            let _userid = read_until_nul(&mut sock).await?;
            if cd != 1 {
                sock.write_all(&socks4_reply(false)).await?; // 只支持 CONNECT
                return Ok(());
            }
            // DSTIP 形如 0.0.0.x(x≠0) 表示 SOCKS4a：真正的主机名以 \0 结尾跟在后面
            let host = if ip[0] == 0 && ip[1] == 0 && ip[2] == 0 && ip[3] != 0 {
                String::from_utf8_lossy(&read_until_nul(&mut sock).await?).to_string()
            } else {
                format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
            };
            (host, port)
        }
        _ => return Ok(()), // 非 SOCKS4/5
    };

    match tunnel_open(&server_ip, server_port, &connector, &auth, &host, port).await {
        Ok(mut tls) => {
            if ver == 5 {
                sock.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            } else {
                sock.write_all(&socks4_reply(true)).await?;
            }
            let _guard = ConnGuard::new(counter); // 进入转发阶段才计数
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut tls).await;
        }
        Err(code) => {
            if ver == 5 {
                sock.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            } else {
                sock.write_all(&socks4_reply(false)).await?;
            }
        }
    }
    Ok(())
}

/// 停止当前代理（abort 监听任务，复位状态）。
fn proxy_stop_locked(g: &mut Inner) {
    if let Some(h) = g.proxy_task.take() {
        h.abort();
    }
    g.proxy_role = 0;
    g.proxy_socks_port = 0;
    g.proxy_http_port = 0;
    g.proxy_port = 0;
    // 换一批新指标，旧任务即便延迟析构也不会再影响展示
    g.proxy_conns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    g.proxy_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    g.proxy_hosts = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
}

#[tauri::command]
pub fn lan_proxy_status(state: State<'_, LanState>) -> serde_json::Value {
    let g = state.0.lock().unwrap();
    let conns = g.proxy_conns.load(std::sync::atomic::Ordering::Relaxed);
    let bytes = g.proxy_bytes.load(std::sync::atomic::Ordering::Relaxed);
    serde_json::json!({ "role": g.proxy_role, "socksPort": g.proxy_socks_port, "httpPort": g.proxy_http_port, "port": g.proxy_port, "conns": conns, "bytes": bytes })
}

/// 服务端：返回记录到的访问目标列表（host:port + 次数 + 最近时间），按最近时间倒序。
#[tauri::command]
pub fn lan_proxy_hosts(state: State<'_, LanState>) -> Vec<serde_json::Value> {
    let g = state.0.lock().unwrap();
    let map = g.proxy_hosts.lock().unwrap();
    let mut list: Vec<(&String, &(u64, u128))> = map.iter().collect();
    list.sort_by(|a, b| b.1 .1.cmp(&a.1 .1)); // 按最近时间倒序
    list.into_iter()
        .map(|(target, (count, last))| serde_json::json!({ "target": target, "count": count, "lastMs": *last as u64 }))
        .collect()
}

#[tauri::command]
pub fn lan_proxy_stop(state: State<'_, LanState>) {
    proxy_stop_locked(&mut state.0.lock().unwrap());
}

/// 开启代理服务端(出口)：替客户端访问网络。需设访问密码。
#[tauri::command]
pub async fn lan_proxy_start_server(
    state: State<'_, LanState>,
    password: String,
    port: Option<u16>,
) -> Result<(), String> {
    if password.trim().is_empty() {
        return Err("请设置访问密码（客户端需用它连接）".into());
    }
    let port = match port {
        Some(p) if p > 0 => p,
        _ => PROXY_PORT,
    };
    let st = state.inner().clone();
    let (cert_pem, key_pem, my_fp) = load_or_create_cert()?;
    let cfg = proxy_server_config(&cert_pem, &key_pem)?;
    let pw_hash = sha256_hex(password.trim());
    let expected_auth = bind_auth(&pw_hash, &my_fp);
    {
        let mut g = st.0.lock().unwrap();
        proxy_stop_locked(&mut g); // 互斥：先停掉之前的角色（释放端口）
    }
    // 同步绑定端口，绑定失败（被占用/被防火墙拦）直接报错，不再静默
    let listener = bind_reuse(port, false)
        .and_then(tokio::net::TcpListener::from_std)
        .map_err(|e| format!("监听端口 {port} 启动失败（可能被占用或被防火墙拦截）：{e}"))?;
    let ctx = ServeCtx {
        conns: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        bytes: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        hosts: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };
    let task_ctx = ctx.clone();
    let task = tauri::async_runtime::spawn(async move {
        run_proxy_server(listener, cfg, expected_auth, task_ctx).await;
    });
    let mut g = st.0.lock().unwrap();
    g.proxy_role = 1;
    g.proxy_port = port;
    g.proxy_conns = ctx.conns;
    g.proxy_bytes = ctx.bytes;
    g.proxy_hosts = ctx.hosts;
    g.proxy_task = Some(task);
    Ok(())
}

/// 开启代理客户端(本地 SOCKS5 入口)：把流量经隧道发给 fingerprint 指定的服务端。
/// 返回本地 SOCKS5 端口。把浏览器/工具代理设为 127.0.0.1:该端口 即可。
#[tauri::command]
pub async fn lan_proxy_start_client(
    state: State<'_, LanState>,
    fingerprint: String,
    password: String,
    port: Option<u16>,
    ip: Option<String>,
) -> Result<u16, String> {
    let st = state.inner().clone();
    // 取服务端 ip + 身份（用于固定证书 + 通道绑定鉴权）。
    // ip 指定时走该 IP（同设备多网段，用户选定走哪个）；否则用 peer 主 IP。
    let server_ip = match ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => {
            let g = st.0.lock().unwrap();
            g.peers.get(&fingerprint).ok_or("服务端设备不在线，请先扫描")?.ip.clone()
        }
    };
    if fingerprint.trim().is_empty() {
        return Err("服务端身份未知".into());
    }
    let server_port = match port {
        Some(p) if p > 0 => p,
        _ => PROXY_PORT,
    };
    let cfg = proxy_client_config(&fingerprint);
    let auth = bind_auth(&sha256_hex(password.trim()), &fingerprint);
    {
        let mut g = st.0.lock().unwrap();
        proxy_stop_locked(&mut g);
    }
    // 同步绑定本地 SOCKS5 + HTTP 代理端口，失败直接报错
    let socks_listener = bind_reuse(SOCKS_PORT, true)
        .and_then(tokio::net::TcpListener::from_std)
        .map_err(|e| format!("本地端口 {SOCKS_PORT} 启动失败（可能被占用）：{e}"))?;
    let http_listener = bind_reuse(HTTP_PROXY_PORT, true)
        .and_then(tokio::net::TcpListener::from_std)
        .map_err(|e| format!("本地端口 {HTTP_PROXY_PORT} 启动失败（可能被占用）：{e}"))?;
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // 一个任务里同时跑 SOCKS5 与 HTTP 代理；停止时 abort 本任务即一并关闭
    let (ip1, cfg1, auth1, c1) = (server_ip.clone(), cfg.clone(), auth.clone(), counter.clone());
    let (ip2, cfg2, auth2, c2) = (server_ip, cfg, auth, counter.clone());
    let task = tauri::async_runtime::spawn(async move {
        tokio::join!(
            run_socks_server(socks_listener, ip1, server_port, cfg1, auth1, c1),
            run_http_proxy_server(http_listener, ip2, server_port, cfg2, auth2, c2),
        );
    });
    let mut g = st.0.lock().unwrap();
    g.proxy_role = 2;
    g.proxy_socks_port = SOCKS_PORT;
    g.proxy_http_port = HTTP_PROXY_PORT;
    g.proxy_port = server_port;
    g.proxy_conns = counter;
    g.proxy_task = Some(task);
    Ok(SOCKS_PORT)
}

/// 客户端「开启」前的连通性自检：能否到达服务端 + 证书是否匹配 + 访问密码是否正确。
/// 用一个「连本不可达的目标(127.0.0.1:1)」的隧道请求来探测：
/// TLS 握手失败=连不上/非该设备；状态 1=密码错；状态 0/2=鉴权通过(目标被拒符合预期)。
#[tauri::command]
pub async fn lan_proxy_test(
    state: State<'_, LanState>,
    fingerprint: String,
    password: String,
    port: Option<u16>,
    ip: Option<String>,
) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let st = state.inner().clone();
    let server_ip = match ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => {
            let g = st.0.lock().unwrap();
            g.peers.get(&fingerprint).ok_or("服务端设备不在线，请先扫描")?.ip.clone()
        }
    };
    if fingerprint.trim().is_empty() {
        return Err("服务端身份未知".into());
    }
    let server_port = match port {
        Some(p) if p > 0 => p,
        _ => PROXY_PORT,
    };
    let cfg = proxy_client_config(&fingerprint);
    let auth = bind_auth(&sha256_hex(password.trim()), &fingerprint);

    let fut = async move {
        let tcp = tokio::net::TcpStream::connect((server_ip.as_str(), server_port))
            .await
            .map_err(|_| format!("连不上服务端 {server_ip}:{server_port}（服务端未开启或防火墙未放行）"))?;
        let connector = tokio_rustls::TlsConnector::from(cfg);
        let dns = rustls::pki_types::ServerName::try_from("baibao.local")
            .map_err(|_| "内部错误".to_string())?
            .to_owned();
        let mut tls = connector
            .connect(dns, tcp)
            .await
            .map_err(|_| "证书校验失败（对方不是该设备，或证书已变更，请重新扫描）".to_string())?;
        let host = b"127.0.0.1";
        tls.write_u16(auth.len() as u16).await.map_err(|e| e.to_string())?;
        tls.write_all(auth.as_bytes()).await.map_err(|e| e.to_string())?;
        tls.write_u8(host.len() as u8).await.map_err(|e| e.to_string())?;
        tls.write_all(host).await.map_err(|e| e.to_string())?;
        tls.write_u16(1u16).await.map_err(|e| e.to_string())?;
        tls.flush().await.map_err(|e| e.to_string())?;
        match tls.read_u8().await {
            Ok(1) => Err("访问密码错误".to_string()),
            Ok(_) => Ok(()), // 0/2：鉴权通过（目标 127.0.0.1:1 被拒属预期）
            Err(_) => Err("服务端无响应".to_string()),
        }
    };
    tokio::time::timeout(Duration::from_secs(8), fut)
        .await
        .map_err(|_| "连接超时".to_string())?
}

/// 设置 / 还原系统代理。enable=false 时还原。
/// macOS 用 SOCKS(networksetup，osascript 提权弹一次框)；
/// Windows 用 HTTP 代理(写 HKCU 注册表，免管理员)——Windows 系统代理对 SOCKS 网页流量支持不可靠。
#[tauri::command]
pub async fn lan_set_system_proxy(enable: bool, socks_port: u16, http_port: u16) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || apply_system_proxy(enable, socks_port, http_port))
        .await
        .map_err(|e| format!("任务调度失败：{e}"))?
}

#[cfg(target_os = "macos")]
fn apply_system_proxy(enable: bool, port: u16, http_port: u16) -> Result<(), String> {
    // 取所有网络服务（跳过首行说明 + 带 * 的已禁用项）
    let out = std::process::Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
        .map_err(|e| format!("读取网络服务失败：{e}"))?;
    let services: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter(|l| !l.starts_with('*') && !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();
    if services.is_empty() {
        return Err("未找到可用网络服务".into());
    }
    // 把命令写进临时 sh 文件，再用 osascript 一次性提权执行（只弹一次密码，避免嵌套引号问题）
    let mut script = String::from("#!/bin/sh\n");
    for s in &services {
        let q = format!("'{}'", s.replace('\'', "'\\''")); // 单引号 shell 转义
        if enable {
            // SOCKS 指向本地 SOCKS5；HTTP/HTTPS 指向本地 HTTP 代理——覆盖只认 web 代理的 App
            script += &format!("networksetup -setsocksfirewallproxy {q} 127.0.0.1 {port}\n");
            script += &format!("networksetup -setsocksfirewallproxystate {q} on\n");
            script += &format!("networksetup -setwebproxy {q} 127.0.0.1 {http_port}\n");
            script += &format!("networksetup -setwebproxystate {q} on\n");
            script += &format!("networksetup -setsecurewebproxy {q} 127.0.0.1 {http_port}\n");
            script += &format!("networksetup -setsecurewebproxystate {q} on\n");
        } else {
            script += &format!("networksetup -setsocksfirewallproxystate {q} off\n");
            script += &format!("networksetup -setwebproxystate {q} off\n");
            script += &format!("networksetup -setsecurewebproxystate {q} off\n");
        }
    }
    let path = std::env::temp_dir().join(format!("baibao_sysproxy_{}.sh", rand_hex(6)));
    std::fs::write(&path, script).map_err(|e| format!("写入脚本失败：{e}"))?;
    let apple = format!(
        "do shell script \"/bin/sh {}\" with administrator privileges",
        path.display()
    );
    let st = std::process::Command::new("osascript").arg("-e").arg(&apple).status();
    let _ = std::fs::remove_file(&path);
    match st {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("设置系统代理失败（已取消授权或权限不足）".into()),
        Err(e) => Err(format!("执行失败：{e}")),
    }
}

#[cfg(target_os = "windows")]
fn apply_system_proxy(enable: bool, _socks_port: u16, http_port: u16) -> Result<(), String> {
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let run = |args: &[&str]| -> Result<(), String> {
        crate::tools::hidden_command("reg")
            .args(args)
            .status()
            .map_err(|e| format!("写注册表失败：{e}"))
            .and_then(|s| if s.success() { Ok(()) } else { Err("写注册表失败".into()) })
    };
    if enable {
        // 用 HTTP 代理覆盖 http/https；浏览器对系统 HTTP 代理支持可靠
        let v = format!("http=127.0.0.1:{http_port};https=127.0.0.1:{http_port}");
        run(&["add", KEY, "/v", "ProxyServer", "/t", "REG_SZ", "/d", &v, "/f"])?;
        run(&["add", KEY, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "1", "/f"])?;
    } else {
        run(&["add", KEY, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "0", "/f"])?;
    }
    // 关键：广播刷新，否则 Chrome/Edge 等会用缓存的旧代理配置（不重启就不生效）
    notify_wininet_changed();
    Ok(())
}

/// 通知 WinINET 重新加载系统代理，让 Chrome/Edge 等无需重启即时生效。
#[cfg(target_os = "windows")]
fn notify_wininet_changed() {
    #[link(name = "wininet")]
    extern "system" {
        fn InternetSetOptionW(
            h: *mut core::ffi::c_void,
            opt: u32,
            buf: *mut core::ffi::c_void,
            len: u32,
        ) -> i32;
    }
    const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
    const INTERNET_OPTION_REFRESH: u32 = 37;
    unsafe {
        InternetSetOptionW(std::ptr::null_mut(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null_mut(), 0);
        InternetSetOptionW(std::ptr::null_mut(), INTERNET_OPTION_REFRESH, std::ptr::null_mut(), 0);
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn apply_system_proxy(_enable: bool, _socks_port: u16, _http_port: u16) -> Result<(), String> {
    Err("当前系统暂不支持自动设置系统代理，请手动配置 SOCKS5 127.0.0.1".into())
}

/// 检测系统代理是否被"遗留"指向本应用的本地端口（上次崩溃/被强杀没还原）。
/// 代理状态本应用从不持久化（每次启动都是关），所以一旦系统代理仍指向 127.0.0.1:<本应用端口>，
/// 必是遗留——此时本地端口没人服务，会导致全系统断网。检测只读、不需要管理员权限。
#[cfg(target_os = "macos")]
fn detect_leftover_system_proxy() -> bool {
    let Ok(out) = std::process::Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
    else {
        return false;
    };
    let services = String::from_utf8_lossy(&out.stdout);
    for s in services.lines().skip(1).filter(|l| !l.starts_with('*') && !l.trim().is_empty()) {
        if let Ok(o) = std::process::Command::new("networksetup")
            .args(["-getsocksfirewallproxy", s.trim()])
            .output()
        {
            let t = String::from_utf8_lossy(&o.stdout);
            let enabled = t.lines().any(|l| l.starts_with("Enabled:") && l.contains("Yes"));
            let local = t.lines().any(|l| l.starts_with("Server:") && l.contains("127.0.0.1"));
            let our_port = t.lines().any(|l| l.starts_with("Port:") && l.contains(&SOCKS_PORT.to_string()));
            if enabled && local && our_port {
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "windows")]
fn detect_leftover_system_proxy() -> bool {
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let on = crate::tools::hidden_command("reg")
        .args(["query", KEY, "/v", "ProxyEnable"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("0x1"))
        .unwrap_or(false);
    if !on {
        return false;
    }
    crate::tools::hidden_command("reg")
        .args(["query", KEY, "/v", "ProxyServer"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("127.0.0.1:{HTTP_PROXY_PORT}")))
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn detect_leftover_system_proxy() -> bool {
    false
}

/// 启动后由前端调用：系统代理是否被本应用遗留（需要清理）。
#[tauri::command]
pub async fn lan_proxy_leftover() -> bool {
    tauri::async_runtime::spawn_blocking(detect_leftover_system_proxy)
        .await
        .unwrap_or(false)
}

// ── 指定应用走代理（启动式）─────────────────────────────────
// 由百宝箱「带着代理设置」去启动选定的 App：浏览器(Chromium 系)用命令行 flag，
// 其他程序注入 ALL_PROXY/HTTPS_PROXY 等环境变量。
// 局限：只对「经此启动」的实例生效；已在运行的进程（尤其浏览器单实例）不接管。

/// 选择要通过代理启动的应用（跨平台）：macOS 选 .app，Windows 选 .exe。
#[tauri::command]
pub async fn lan_proxy_pick_app(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            let _ = &app; // macOS 走 osascript（.app 是「包」，原生文件框默认不可直接选中）
            let script = r#"POSIX path of (choose file with prompt "选择要通过代理启动的 App" of type {"com.apple.application-bundle"} default location (path to applications folder))"#;
            let out = std::process::Command::new("osascript")
                .arg("-e")
                .arg(script)
                .output()
                .map_err(|e| format!("打开选择器失败：{e}"))?;
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                Ok(if p.is_empty() { None } else { Some(p) })
            } else {
                let err = String::from_utf8_lossy(&out.stderr);
                if err.contains("-128") || err.contains("User canceled") {
                    Ok(None)
                } else {
                    Err(format!("选择失败：{}", err.trim()))
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let picked = app
                .dialog()
                .file()
                .set_title("选择要通过代理启动的程序")
                .add_filter("程序", &["exe"])
                .blocking_pick_file();
            Ok(picked
                .and_then(|fp| fp.into_path().ok())
                .map(|p| p.to_string_lossy().into_owned()))
        }
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

fn is_chromium(path: &str) -> bool {
    let p = path.to_lowercase();
    ["chrome", "chromium", "msedge", "microsoft edge", "edge", "brave", "vivaldi", "opera"]
        .iter()
        .any(|k| p.contains(k))
}

/// 带代理启动选定的应用。返回一句人类可读的结果说明。
#[tauri::command]
pub async fn lan_proxy_launch_app(path: String, port: u16) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if path.trim().is_empty() {
            return Err("未选择应用".into());
        }
        let socks = format!("socks5://127.0.0.1:{port}");
        let chromium = is_chromium(&path);

        // 解析真正要执行的可执行文件 + 展示用名称
        #[cfg(target_os = "macos")]
        let (exe, name): (std::path::PathBuf, String) = {
            let p = std::path::Path::new(&path);
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("应用").to_string();
            if path.ends_with(".app") {
                // 读 Info.plist 的 CFBundleExecutable，拿到 Contents/MacOS 下的真实可执行名
                let exe_name = std::process::Command::new("defaults")
                    .arg("read")
                    .arg(format!("{path}/Contents/Info"))
                    .arg("CFBundleExecutable")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| name.clone());
                (std::path::PathBuf::from(format!("{path}/Contents/MacOS/{exe_name}")), name)
            } else {
                (p.to_path_buf(), name)
            }
        };
        #[cfg(not(target_os = "macos"))]
        let (exe, name): (std::path::PathBuf, String) = {
            let p = std::path::Path::new(&path);
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("应用").to_string();
            (p.to_path_buf(), name)
        };

        let mut cmd = std::process::Command::new(&exe);
        if chromium {
            cmd.arg(format!("--proxy-server={socks}"));
        }
        // 代理环境变量（大小写都给，兼容不同程序的读取习惯）
        for k in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
            cmd.env(k, &socks);
        }
        cmd.spawn().map_err(|e| format!("启动失败：{e}（可能已在运行或路径无效）"))?;
        let how = if chromium {
            "已带 SOCKS5 代理参数启动"
        } else {
            "已用代理环境变量启动"
        };
        Ok(format!("{how}「{name}」"))
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningApp {
    name: String,
    path: String, // macOS=.app 路径 / Windows=.exe 路径，可直接交给 lan_proxy_launch_app
}

/// 列出当前正在运行的「有界面的」应用，供「指定应用」里直接挑选（免去文件浏览）。
#[cfg(target_os = "macos")]
fn list_running_apps() -> Vec<RunningApp> {
    // ps 的 comm 列给出可执行文件全路径；GUI 应用主程序路径含 .app/Contents/MacOS/
    let out = std::process::Command::new("ps").args(["-axo", "comm="]).output();
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Ok(o) = out {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let line = line.trim();
            if let Some(idx) = line.find(".app/Contents/MacOS/") {
                let app_path = &line[..idx + 4]; // 含 ".app"
                let name = std::path::Path::new(app_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    map.entry(name).or_insert_with(|| app_path.to_string());
                }
            }
        }
    }
    map.into_iter().map(|(name, path)| RunningApp { name, path }).collect()
}

#[cfg(target_os = "windows")]
fn list_running_apps() -> Vec<RunningApp> {
    // 取有主窗口的进程（即 GUI 应用）及其 exe 路径
    let out = crate::tools::hidden_command("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-Process | Where-Object {$_.MainWindowHandle -ne 0 -and $_.Path} | Select-Object ProcessName,Path -Unique | ConvertTo-Json -Compress",
        ])
        .output();
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Ok(o) = out {
        let txt = String::from_utf8_lossy(&o.stdout);
        let v: serde_json::Value = serde_json::from_str(txt.trim()).unwrap_or(serde_json::Value::Null);
        let arr = match v {
            serde_json::Value::Array(a) => a,
            serde_json::Value::Object(_) => vec![v],
            _ => vec![],
        };
        for item in arr {
            let name = item.get("ProcessName").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let path = item.get("Path").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if !name.is_empty() && !path.is_empty() {
                map.entry(name).or_insert(path);
            }
        }
    }
    map.into_iter().map(|(name, path)| RunningApp { name, path }).collect()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn list_running_apps() -> Vec<RunningApp> {
    Vec::new()
}

#[tauri::command]
pub async fn lan_proxy_running_apps() -> Result<Vec<RunningApp>, String> {
    tauri::async_runtime::spawn_blocking(list_running_apps)
        .await
        .map_err(|e| format!("任务调度失败：{e}"))
}

/// 扫描中断标志：前端点「中断」时置位，扫描线程据此提前退出（已发现的设备照常登记）。
static SCAN_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 中断正在进行的网段扫描。
#[tauri::command]
pub fn lan_scan_cancel() {
    SCAN_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// 主动扫描网段（探测 53317 的 /info），发现百宝箱设备并登记。返回发现总数。
/// prefix=None：只扫真实 LAN 网卡（跳过 VPN）。
/// prefix=Some("192.168.1")：只扫该指定 /24。
/// prefix=Some("192.168.56.0/22")：按 CIDR 整段扫（VPN 段手动触发用）。
/// 扫描过程中通过 `lan://scan-progress` 上报 {done,total}，可用 `lan_scan_cancel` 中断。
#[tauri::command]
pub async fn lan_scan_subnet(
    app: AppHandle,
    state: State<'_, LanState>,
    prefix: Option<String>,
) -> Result<u32, String> {
    let st = state.inner().clone();
    let my_fp = st.0.lock().unwrap().fingerprint.clone();
    // 候选 IP 三种来源：
    // - 含「/」：手动指定网段（CIDR，如 "192.168.56.0/22"），按真实掩码整段扫——VPN 网段由用户手动触发。
    // - 形如 "192.168.1"：单个 /24（请求代理「扫描本网段」用）。
    // - None：默认扫描，只扫真实 LAN 网卡（跳过 VPN/虚拟），避免每次都扫慢且大的 VPN 段。
    let mut targets: Vec<std::net::Ipv4Addr> = Vec::new();
    match prefix.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) if p.contains('/') => {
            let (ip, mask) = parse_cidr(p).ok_or("网段格式不正确（应形如 192.168.56.0/22）")?;
            targets.extend(subnet_scan_targets(ip, mask));
        }
        Some(p) => {
            let o: Vec<u8> = p.split('.').filter_map(|s| s.parse().ok()).collect();
            if o.len() != 3 {
                return Err("网段格式不正确（应形如 192.168.1）".into());
            }
            for h in 1u8..=254 {
                targets.push(std::net::Ipv4Addr::new(o[0], o[1], o[2], h));
            }
        }
        None => {
            for (name, ip, mask) in local_ipv4_ifaces_masked() {
                if looks_like_vpn(&name) {
                    continue; // 默认扫描跳过 VPN/虚拟网卡，VPN 段需用户在网段列表手动扫
                }
                targets.extend(subnet_scan_targets(ip, mask));
            }
        }
    }
    targets.sort();
    targets.dedup();
    let total = targets.len();
    SCAN_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed); // 每次扫描前清除中断标志
    let _ = app.emit("lan://scan-progress", serde_json::json!({ "done": 0, "total": total }));

    let app2 = app.clone();
    let found = tauri::async_runtime::spawn_blocking(move || -> Vec<(DeviceInfo, String)> {
        let client = match reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .timeout(Duration::from_millis(700))
            .build()
        {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let out = std::sync::Mutex::new(Vec::new());
        let next = std::sync::atomic::AtomicUsize::new(0);
        let done = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..48 {
                s.spawn(|| loop {
                    if SCAN_CANCEL.load(std::sync::atomic::Ordering::Relaxed) {
                        break; // 用户中断：停止取新任务，已发现的照常返回
                    }
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= targets.len() {
                        break;
                    }
                    let ip = targets[i].to_string();
                    if let Ok(resp) = client
                        .get(format!("https://{ip}:{PORT}/api/localsend/v2/info"))
                        .send()
                    {
                        if resp.status().is_success() {
                            if let Ok(info) = resp.json::<DeviceInfo>() {
                                if info.app.as_deref() == Some("baibao") {
                                    out.lock().unwrap().push((info, ip));
                                }
                            }
                        }
                    }
                    // 进度上报：每完成 16 个或扫到末尾时推一次，避免过于频繁
                    let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if d % 16 == 0 || d == total {
                        let _ = app2.emit("lan://scan-progress", serde_json::json!({ "done": d, "total": total }));
                    }
                });
            }
        });
        out.into_inner().unwrap()
    })
    .await
    .map_err(|e| format!("扫描失败：{e}"))?;

    let mut n = 0u32;
    for (info, ip) in found {
        if info.fingerprint != my_fp && !info.fingerprint.is_empty() {
            upsert_peer(&app, &st, &info, ip, true); // 手动扫描：sticky，不被 TTL 清掉
            n += 1;
        }
    }
    Ok(n)
}

// ── 命令：发送 ───────────────────────────────────────────────

#[tauri::command]
pub async fn lan_send_message(
    state: State<'_, LanState>,
    fingerprint: String,
    text: String,
    msg_id: Option<String>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, my_alias, peer_fp) = peer_conn(&state, &fingerprint)?;
    let my_fp = state.0.lock().unwrap().fingerprint.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = build_pinned_client(&peer_fp, Some(Duration::from_secs(10)))?;
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
    let (base, _my_alias, peer_fp) = peer_conn(&state, &fingerprint)?;
    let my_fp = state.0.lock().unwrap().fingerprint.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = build_pinned_client(&peer_fp, Some(Duration::from_secs(10)))?;
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
    let (base, my_alias, peer_fp) = peer_conn(&state, &fingerprint)?;
    let info = state.device_info(None);
    if paths.is_empty() {
        return Err("没有要发送的文件".into());
    }

    tauri::async_runtime::spawn_blocking(move || {
        let _ = my_alias;
        // 不设总超时（大文件可能传很久），仅限制建连耗时；中断由断点续传重试兜底
        let client = build_pinned_client(&peer_fp, None)?;

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

    // 握手：prepare-upload（抗网络抖动）。带稳定 offerId，对端「立即」应答而非挂起连接：
    // 对方还没点「接收」时返回 pending，下面就每 2 秒轮询一次；用户点「接收」后才返回 token。
    // 等待期间若网络瞬断/抖动，.send() 出错也不判失败——歇 2 秒在后台重连重试，同一 offerId
    // 在接收端是幂等的：既不会重复弹「点击接收」，也不会因一次抖动就丢掉这次发送请求。
    // 直到对方接收 / 拒绝 / 满 5 分钟超时。（真正的大文件上传另有断点续传重试，见下方循环。）
    let offer_id = rand_hex(12);
    let deadline = Instant::now() + Duration::from_secs(300);
    let resp: PrepareUploadResponse = loop {
        // 等待期间用户撤销了发送：通知接收端撤回「待接收」，否则对方仍能点「接收」
        if state.is_send_cancelled(id) {
            let _ = client
                .post(format!("{base}/api/baibao/v1/cancel-offer"))
                .timeout(Duration::from_secs(5))
                .json(&serde_json::json!({ "offerId": offer_id }))
                .send();
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("对方 5 分钟未接收，已超时".to_string());
        }
        let reply = client
            .post(format!("{base}/api/localsend/v2/prepare-upload"))
            .timeout(Duration::from_secs(15))
            .json(&serde_json::json!({ "info": info, "files": files, "offerId": offer_id }))
            .send();
        let reply = match reply {
            Ok(r) => r,
            // 网络抖动/瞬断：歇 2 秒后台重连重试，不报错、不打扰用户
            Err(_) => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let status = reply.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            // 403：拒绝 or 接收端 5 分钟超时（见 handle_prepare_upload_poll），用响应体区分
            let timed_out = reply.text().map(|b| b.contains("timeout")).unwrap_or(false);
            return Err(if timed_out {
                "对方 5 分钟未接收，已超时".to_string()
            } else {
                "对方拒绝接收".to_string()
            });
        }
        if !status.is_success() {
            return Err(format!("对方未接受（{status}）"));
        }
        let body: PrepareUploadReply = reply.json().map_err(|_| "握手响应异常".to_string())?;
        // 对方还没点「接收」：等 2 秒再轮询（offerId 幂等，接收端不会重复弹窗）
        if body.status.as_deref() == Some("pending") {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        // 已接受
        break PrepareUploadResponse {
            session_id: body.session_id,
            files: body.files,
        };
    };

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

/// 给「带请求体的 POST」回错误前，先把客户端还在推送的 body 读空再响应。
/// 否则我们提前返回（401/403/404…）会让对方在发送 body 阶段被连接重置，
/// 客户端只能拿到模糊的 "error sending request"，而不是干净的状态码。
fn respond_text_drained(mut req: tiny_http::Request, code: u16, body: &str) {
    let _ = std::io::copy(&mut req.as_reader(), &mut std::io::sink());
    respond_text(req, code, body);
}

// ── 断点续传支持 ──────────────────────────────────────────────
// 客户端用一个 token（=任务 id）标识一次逻辑上传；服务端把已收字节累积到
// 一个以 token 命名的临时文件，按 offset 续写，集齐 total 字节后再落地/解压。

/// 过滤 token，只留文件名安全字符，防路径穿越。
fn sanitize_token(t: &str) -> String {
    t.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

/// 续传临时文件路径（.bin=单文件，.zip=文件夹）。
fn upload_part_path(token: &str, zip: bool) -> PathBuf {
    let ext = if zip { "zip" } else { "bin" };
    std::env::temp_dir().join(format!("baibao_part_{token}.{ext}"))
}


/// 把续传完成的临时文件落地到目标路径（同盘 rename，跨盘退化为 copy）。
fn finalize_part_to(part: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    if let Some(p) = dest.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if dest.exists() {
        let _ = std::fs::remove_file(dest); // 覆盖同名文件
    }
    match std::fs::rename(part, dest) {
        Ok(_) => Ok(()),
        Err(_) => {
            std::fs::copy(part, dest)?;
            let _ = std::fs::remove_file(part);
            Ok(())
        }
    }
}

/// 把一个 zip 解压到 target（用 enclosed_name 防 zip-slip 穿越）。
fn extract_zip_into(zip_path: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    let zf = File::open(zip_path)?;
    let mut ar =
        zip::ZipArchive::new(zf).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    for i in 0..ar.len() {
        let mut entry = ar
            .by_index(i)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let Some(safe) = entry.enclosed_name() else { continue };
        let out = target.join(safe);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut of = File::create(&out)?;
            std::io::copy(&mut entry, &mut of)?;
        }
    }
    Ok(())
}

/// 接收方：把请求体按 offset 续写到 part，期间上报接收进度、并响应「拒绝」。
/// Ok(now)=写入后的总字节；Err(true)=接收方拒绝；Err(false)=连接中断。
fn recv_append_tracked(
    request: &mut tiny_http::Request,
    part: &std::path::Path,
    offset: u64,
    app: &AppHandle,
    token: &str,
    name: &str,
    total: u64,
    reject: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<u64, bool> {
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::Ordering::Relaxed;
    let mut f = match std::fs::OpenOptions::new().create(true).read(true).write(true).open(part) {
        Ok(f) => f,
        Err(_) => return Err(false),
    };
    if f.set_len(offset).is_err() || f.seek(SeekFrom::Start(offset)).is_err() {
        return Err(false);
    }
    let reader = request.as_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut received = offset;
    let mut last = offset;
    loop {
        if reject.load(Relaxed) {
            return Err(true);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return Err(false),
        };
        if f.write_all(&buf[..n]).is_err() {
            return Err(false);
        }
        received += n as u64;
        if received - last > 256 * 1024 || (total > 0 && received >= total) {
            last = received;
            let _ = app.emit(
                "lan://share-incoming",
                serde_json::json!({ "token": token, "name": name, "received": received, "total": total, "phase": "recv" }),
            );
        }
    }
    Ok(received)
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

/// 清理上次运行（含崩溃）残留的临时 zip：系统临时目录里的打包/收发临时包，
/// 以及接收目录里下载用的 `.baibao_dl_*.zip`。启动时调用一次，best-effort。
fn cleanup_stale_zips(download_dir: &std::path::Path) {
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let stale = (n.ends_with(".zip")
                && (n.starts_with("baibao_send_")
                    || n.starts_with("baibao_share_")
                    || n.starts_with("baibao_recv_")))
                // 断点续传残留：客户端打包中间文件 + 服务端累积分片
                || (n.starts_with("baibao_send_") && n.ends_with(".building"))
                || n.starts_with("baibao_part_");
            if stale {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(download_dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(".baibao_dl_") && n.ends_with(".zip") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
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
