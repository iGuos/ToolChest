// 局域网互传：设备发现（UDP 多播）+ 收件 HTTP 服务（tiny_http）+ 发送（reqwest）。
// wire 格式与 LocalSend v2 一致（HTTP 明文）：
//   POST /api/localsend/v2/register        交换设备信息
//   POST /api/localsend/v2/prepare-upload   发文件前握手，接收方需确认
//   POST /api/localsend/v2/upload           流式上传单个文件
//   GET  /api/localsend/v2/info             查询设备信息
// 百宝箱自有扩展：
//   POST /api/baibao/v1/message             即时文本消息（无需确认）
// announce/info 里带 app:"baibao" 标记区分百宝箱设备；compat 关闭时只认百宝箱、
// 拒绝外部 LocalSend 设备的发送，开启后才与真正的 LocalSend 互通。

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
const PORT: u16 = 53317;
const PROTOCOL_VERSION: &str = "2.1";
const ANNOUNCE_EVERY: Duration = Duration::from_secs(4);
const PEER_TTL_MS: u128 = 15_000;

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
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Peer {
    alias: String,
    fingerprint: String,
    ip: String,
    port: u16,
    protocol: String,
    device_type: Option<String>,
    is_baibao: bool,
    last_seen_ms: u128,
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
}

struct Inner {
    started: bool,
    alias: String,
    fingerprint: String,
    download_dir: PathBuf,
    compat: bool,
    peers: HashMap<String, Peer>,
    decisions: HashMap<String, Sender<Decision>>,
    sessions: HashMap<String, HashMap<String, FileSlot>>, // sessionId -> fileId -> slot
    socket: Option<Arc<UdpSocket>>,
}

impl Default for Inner {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        Inner {
            started: false,
            alias: default_alias(),
            fingerprint: rand_hex(16),
            download_dir: PathBuf::from(home).join("Downloads"),
            compat: false,
            peers: HashMap::new(),
            decisions: HashMap::new(),
            sessions: HashMap::new(),
            socket: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct LanState(Arc<Mutex<Inner>>);

fn default_alias() -> String {
    // macOS：取「电脑名称」；失败回退用户名 / 通用名
    if let Ok(out) = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    std::env::var("USER")
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
        }
    }
}

fn emit_peers(app: &AppHandle, state: &LanState) {
    let peers: Vec<Peer> = {
        let g = state.0.lock().unwrap();
        g.peers.values().cloned().collect()
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
        };
        g.peers.insert(info.fingerprint.clone(), peer);
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

    // 启动即主动 announce 一次
    let info = state.device_info(Some(true));
    if let Ok(buf) = serde_json::to_vec(&info) {
        let _ = udp.send_to(&buf, SocketAddr::from((MULTICAST_ADDR, PORT)));
    }

    Ok(my_info_value(&state))
}

fn build_multicast_socket() -> std::io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    let bind: SocketAddr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT));
    sock.bind(&bind.into())?;
    let udp: UdpSocket = sock.into();
    udp.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)?;
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
        // 对方在 announce → 回应自己的信息（announce=false），单播避免风暴
        if info.announce == Some(true) {
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
        let info = state.device_info(Some(true));
        if let Ok(buf) = serde_json::to_vec(&info) {
            let _ = sock.send_to(&buf, SocketAddr::from((MULTICAST_ADDR, PORT)));
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
                let _ = app.emit(
                    "lan://message",
                    serde_json::json!({
                        "fingerprint": msg.fingerprint,
                        "alias": msg.alias,
                        "text": msg.text,
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
        ("POST", "/api/localsend/v2/prepare-upload") => {
            handle_prepare_upload(&app, &state, request);
            Ok(())
        }
        ("POST", "/api/localsend/v2/upload") => {
            handle_upload(&app, &state, request, &query);
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

fn handle_prepare_upload(app: &AppHandle, state: &LanState, mut request: tiny_http::Request) {
    let req: PrepareUploadRequest = match read_json(&mut request) {
        Ok(v) => v,
        Err(_) => return respond_text(request, 400, "bad request"),
    };

    // compat 关闭时：只接受百宝箱设备
    let is_baibao = req.info.app.as_deref() == Some("baibao");
    if !is_baibao && !state.0.lock().unwrap().compat {
        return respond_text(request, 403, "rejected: 未开启 LocalSend 兼容");
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

    // 等用户确认（超时视为拒绝）
    let decision = rx.recv_timeout(Duration::from_secs(70));
    state.0.lock().unwrap().decisions.remove(&session_id);

    let decision = match decision {
        Ok(d) if d.accept => d,
        _ => return respond_text(request, 403, "declined"),
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
                },
            );
        }
    }
    if slots.is_empty() {
        return respond_text(request, 403, "declined");
    }
    state.0.lock().unwrap().sessions.insert(session_id.clone(), slots);

    respond_json(
        request,
        200,
        &PrepareUploadResponse {
            session_id,
            files: tokens,
        },
    );
}

fn handle_upload(app: &AppHandle, state: &LanState, mut request: tiny_http::Request, query: &str) {
    let q = parse_query(query);
    let (Some(session_id), Some(file_id), Some(token)) =
        (q.get("sessionId"), q.get("fileId"), q.get("token"))
    else {
        return respond_text(request, 400, "missing params");
    };

    // 校验 token，取出文件名/大小与下载目录
    let (file_name, size, dir) = {
        let g = state.0.lock().unwrap();
        let slot = match g.sessions.get(session_id).and_then(|s| s.get(file_id)) {
            Some(s) if &s.token == token => s,
            _ => {
                drop(g);
                return respond_text(request, 403, "invalid token");
            }
        };
        (slot.file_name.clone(), slot.size, g.download_dir.clone())
    };

    let dest = unique_path(&dir, &file_name);
    let _ = std::fs::create_dir_all(&dir);
    let mut file = match File::create(&dest) {
        Ok(f) => f,
        Err(e) => return respond_text(request, 500, &format!("无法写入：{e}")),
    };

    let reader = request.as_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut received: u64 = 0;
    let mut last_emit: u64 = 0;
    loop {
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
                            "fileName": file_name, "transferred": received, "size": size,
                        }),
                    );
                }
            }
            Err(_) => break,
        }
    }

    // 收尾：清理会话槽位
    {
        let mut g = state.0.lock().unwrap();
        if let Some(s) = g.sessions.get_mut(session_id) {
            s.remove(file_id);
            if s.is_empty() {
                g.sessions.remove(session_id);
            }
        }
    }
    let _ = app.emit(
        "lan://received",
        serde_json::json!({
            "fileName": file_name, "path": dest.to_string_lossy(), "size": received,
        }),
    );
    respond_text(request, 200, "ok");
}

// ── 命令：信息 / 设置 ─────────────────────────────────────────

fn my_info_value(state: &LanState) -> serde_json::Value {
    let g = state.0.lock().unwrap();
    serde_json::json!({
        "alias": g.alias,
        "fingerprint": g.fingerprint,
        "port": PORT,
        "compat": g.compat,
        "downloadDir": g.download_dir.to_string_lossy(),
    })
}

#[tauri::command]
pub fn lan_my_info(state: State<'_, LanState>) -> serde_json::Value {
    my_info_value(state.inner())
}

#[tauri::command]
pub fn lan_peers(state: State<'_, LanState>) -> Vec<serde_json::Value> {
    let g = state.0.lock().unwrap();
    g.peers
        .values()
        .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null))
        .collect()
}

#[tauri::command]
pub fn lan_set_alias(state: State<'_, LanState>, alias: String) {
    let a = alias.trim();
    if !a.is_empty() {
        state.0.lock().unwrap().alias = a.to_string();
    }
}

#[tauri::command]
pub fn lan_set_compat(state: State<'_, LanState>, enabled: bool) {
    state.0.lock().unwrap().compat = enabled;
}

#[tauri::command]
pub fn lan_set_dir(state: State<'_, LanState>, dir: String) {
    if !dir.trim().is_empty() {
        state.0.lock().unwrap().download_dir = PathBuf::from(dir);
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
pub async fn lan_pick_files() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let out = std::process::Command::new("osascript")
            .args([
                "-e",
                "set fs to choose file with prompt \"选择要发送的文件\" with multiple selections allowed",
                "-e",
                "set t to \"\"",
                "-e",
                "repeat with f in fs",
                "-e",
                "set t to t & POSIX path of f & linefeed",
                "-e",
                "end repeat",
                "-e",
                "return t",
            ])
            .output()
            .map_err(|e| format!("打开选择器失败：{e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("-128") || err.contains("User canceled") {
                Ok(vec![])
            } else {
                Err(format!("选择失败：{}", err.trim()))
            }
        }
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

#[tauri::command]
pub async fn lan_pick_dir() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let out = std::process::Command::new("osascript")
            .args([
                "-e",
                "POSIX path of (choose folder with prompt \"选择接收文件的保存目录\")",
            ])
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
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
}

#[tauri::command]
pub async fn lan_reveal(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new("open")
            .args(["-R", &path])
            .status()
            .map(|_| ())
            .map_err(|e| format!("打开失败：{e}"))
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
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
) -> Result<(), String> {
    let state = state.inner().clone();
    let (base, my_alias) = peer_base_url(&state, &fingerprint)?;
    let my_fp = state.0.lock().unwrap().fingerprint.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let body = serde_json::json!({ "alias": my_alias, "fingerprint": my_fp, "text": text });
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

/// 发文件读取时上报进度的包装。
struct ProgressReader {
    inner: File,
    app: AppHandle,
    session_id: String,
    file_id: String,
    file_name: String,
    size: u64,
    sent: u64,
    last_emit: u64,
}
impl Read for ProgressReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.sent += n as u64;
            if self.sent - self.last_emit > 256 * 1024 || self.sent >= self.size {
                self.last_emit = self.sent;
                let _ = self.app.emit(
                    "lan://progress",
                    serde_json::json!({
                        "direction": "out", "sessionId": self.session_id, "fileId": self.file_id,
                        "fileName": self.file_name, "transferred": self.sent, "size": self.size,
                    }),
                );
            }
        }
        Ok(n)
    }
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
        // 组装文件清单
        let mut files = serde_json::Map::new();
        let mut metas: Vec<(String, String, u64, String)> = Vec::new(); // id, path, size, name
        for path in &paths {
            let pb = PathBuf::from(path);
            let meta = std::fs::metadata(&pb).map_err(|e| format!("读取 {path} 失败：{e}"))?;
            let name = pb
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            let id = rand_hex(8);
            let size = meta.len();
            files.insert(
                id.clone(),
                serde_json::json!({
                    "id": id, "fileName": name, "size": size, "fileType": ext_mime(&name),
                }),
            );
            metas.push((id, path.clone(), size, name));
        }

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .map_err(|e| e.to_string())?;

        // 握手：prepare-upload
        let prep = client
            .post(format!("{base}/api/localsend/v2/prepare-upload"))
            .json(&serde_json::json!({ "info": info, "files": files }))
            .send()
            .map_err(|e| format!("发起请求失败：{e}"))?;
        let status = prep.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err("对方拒绝了接收".into());
        }
        if !status.is_success() {
            return Err(format!("对方返回 {status}（可能未接受）"));
        }
        let resp: PrepareUploadResponse = prep
            .json()
            .map_err(|e| format!("解析响应失败：{e}"))?;
        let _ = my_alias;

        // 逐个上传被接受的文件
        for (id, path, size, name) in &metas {
            let Some(token) = resp.files.get(id) else {
                continue; // 该文件未被接受
            };
            let f = File::open(path).map_err(|e| format!("打开 {path} 失败：{e}"))?;
            let reader = ProgressReader {
                inner: f,
                app: app.clone(),
                session_id: resp.session_id.clone(),
                file_id: id.clone(),
                file_name: name.clone(),
                size: *size,
                sent: 0,
                last_emit: 0,
            };
            let body = reqwest::blocking::Body::sized(reader, *size);
            let up = client
                .post(format!("{base}/api/localsend/v2/upload"))
                .query(&[
                    ("sessionId", resp.session_id.as_str()),
                    ("fileId", id.as_str()),
                    ("token", token.as_str()),
                ])
                .body(body)
                .send()
                .map_err(|e| format!("上传 {name} 失败：{e}"))?;
            if !up.status().is_success() {
                return Err(format!("上传 {name} 被拒绝：{}", up.status()));
            }
            let _ = app.emit(
                "lan://sent",
                serde_json::json!({ "fileName": name, "fingerprint": fingerprint }),
            );
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("任务调度失败：{e}"))?
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

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
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
