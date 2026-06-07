use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 响应体超过此大小则截断显示，避免一次性把巨大响应塞进 UI。
const MAX_BODY: usize = 2_000_000;

#[derive(Deserialize)]
pub struct Kv {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<Kv>,
    /// "none" | "raw" | "form"
    #[serde(default)]
    pub body_kind: String,
    #[serde(default)]
    pub body_raw: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub form: Vec<Kv>,
    #[serde(default)]
    pub timeout_ms: u64,
    #[serde(default)]
    pub follow_redirects: bool,
    #[serde(default)]
    pub verify_tls: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub duration_ms: u128,
    pub size_bytes: usize,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub body_is_binary: bool,
    pub truncated: bool,
    pub final_url: String,
}

#[tauri::command]
pub async fn http_send(req: HttpRequest) -> Result<HttpResponse, String> {
    tauri::async_runtime::spawn_blocking(move || send_blocking(req))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

fn send_blocking(req: HttpRequest) -> Result<HttpResponse, String> {
    let method = reqwest::Method::from_bytes(req.method.to_uppercase().as_bytes())
        .map_err(|_| format!("不支持的方法: {}", req.method))?;

    let redirect = if req.follow_redirects {
        reqwest::redirect::Policy::limited(10)
    } else {
        reqwest::redirect::Policy::none()
    };
    let timeout = if req.timeout_ms == 0 {
        Duration::from_secs(30)
    } else {
        Duration::from_millis(req.timeout_ms)
    };

    let client = reqwest::blocking::Client::builder()
        .redirect(redirect)
        .timeout(timeout)
        .danger_accept_invalid_certs(!req.verify_tls)
        .build()
        .map_err(|e| format!("构建客户端失败: {e}"))?;

    let mut rb = client.request(method, &req.url);
    for h in &req.headers {
        if h.enabled && !h.key.is_empty() {
            rb = rb.header(&h.key, &h.value);
        }
    }

    match req.body_kind.as_str() {
        "raw" => {
            if let Some(ct) = req.content_type.as_deref() {
                if !ct.is_empty() {
                    rb = rb.header(reqwest::header::CONTENT_TYPE, ct);
                }
            }
            rb = rb.body(req.body_raw.clone());
        }
        "form" => {
            let pairs: Vec<(&str, &str)> = req
                .form
                .iter()
                .filter(|k| k.enabled && !k.key.is_empty())
                .map(|k| (k.key.as_str(), k.value.as_str()))
                .collect();
            rb = rb.form(&pairs);
        }
        _ => {}
    }

    let start = Instant::now();
    let resp = rb.send().map_err(|e| format!("请求失败: {e}"))?;

    let status = resp.status();
    let final_url = resp.url().to_string();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let bytes = resp.bytes().map_err(|e| format!("读取响应失败: {e}"))?;
    let duration_ms = start.elapsed().as_millis();
    let size_bytes = bytes.len();

    let body_is_binary = std::str::from_utf8(&bytes).is_err();
    let truncated = size_bytes > MAX_BODY;
    let body = if body_is_binary {
        String::new()
    } else {
        let slice = if truncated { &bytes[..MAX_BODY] } else { &bytes[..] };
        String::from_utf8_lossy(slice).to_string()
    };

    Ok(HttpResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        duration_ms,
        size_bytes,
        headers,
        body,
        body_is_binary,
        truncated,
        final_url,
    })
}
