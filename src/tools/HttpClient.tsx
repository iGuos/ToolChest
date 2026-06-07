import { useState, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { parseRequest } from "./importRequest";

interface KV {
  key: string;
  value: string;
  enabled: boolean;
}

interface HttpResponse {
  status: number;
  statusText: string;
  durationMs: number;
  sizeBytes: number;
  headers: [string, string][];
  body: string;
  bodyIsBinary: boolean;
  truncated: boolean;
  finalUrl: string;
}

interface HistoryItem {
  method: string;
  url: string;
  status: number;
  ms: number;
}

// 实际发出的请求（解析 query、注入 auth/content-type 之后的样子）
interface SentReq {
  method: string;
  url: string;
  headers: { key: string; value: string }[];
  body: string;
}

const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const RAW_TYPES: Record<string, string> = {
  json: "application/json",
  text: "text/plain",
  xml: "application/xml",
};

const emptyRow = (): KV => ({ key: "", value: "", enabled: true });

function KvEditor({
  rows,
  onChange,
}: {
  rows: KV[];
  onChange: (rows: KV[]) => void;
}) {
  const set = (i: number, patch: Partial<KV>) =>
    onChange(rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  return (
    <div className="kv-editor">
      {rows.map((r, i) => (
        <div className="kv-row" key={i}>
          <input
            type="checkbox"
            checked={r.enabled}
            onChange={(e) => set(i, { enabled: e.target.checked })}
          />
          <input
            className="kv-input mono"
            placeholder="key"
            value={r.key}
            onChange={(e) => set(i, { key: e.target.value })}
          />
          <input
            className="kv-input mono"
            placeholder="value"
            value={r.value}
            onChange={(e) => set(i, { value: e.target.value })}
          />
          <button
            className="kv-del"
            title="删除"
            onClick={() => onChange(rows.filter((_, idx) => idx !== i))}
          >
            ×
          </button>
        </div>
      ))}
      <button className="btn btn-ghost btn-sm" onClick={() => onChange([...rows, emptyRow()])}>
        + 添加
      </button>
    </div>
  );
}

const TABS = ["Params", "Headers", "Body", "Auth", "选项"] as const;
type Tab = (typeof TABS)[number];

export default function HttpClient() {
  const [method, setMethod] = useState("GET");
  const [url, setUrl] = useState("");
  const [params, setParams] = useState<KV[]>([emptyRow()]);
  const [headers, setHeaders] = useState<KV[]>([emptyRow()]);
  const [bodyKind, setBodyKind] = useState<"none" | "raw" | "form">("none");
  const [rawType, setRawType] = useState<"json" | "text" | "xml">("json");
  const [bodyRaw, setBodyRaw] = useState("");
  const [form, setForm] = useState<KV[]>([emptyRow()]);
  const [authType, setAuthType] = useState<"none" | "bearer" | "basic">("none");
  const [bearer, setBearer] = useState("");
  const [basicUser, setBasicUser] = useState("");
  const [basicPass, setBasicPass] = useState("");
  const [timeoutMs, setTimeoutMs] = useState(30000);
  const [followRedirects, setFollowRedirects] = useState(true);
  const [verifyTls, setVerifyTls] = useState(true);

  const [tab, setTab] = useState<Tab>("Params");
  const [respTab, setRespTab] = useState<"request" | "body" | "headers">("body");
  const [sentReq, setSentReq] = useState<SentReq | null>(null);
  const [pretty, setPretty] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [resp, setResp] = useState<HttpResponse | null>(null);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [importOpen, setImportOpen] = useState(false);
  const [importText, setImportText] = useState("");
  const [importErr, setImportErr] = useState<string | null>(null);

  const applyImport = () => {
    let p;
    try {
      p = parseRequest(importText);
    } catch (e) {
      setImportErr(String(e instanceof Error ? e.message : e));
      return;
    }
    setMethod(p.method || "GET");
    setUrl(p.url);
    setParams([emptyRow()]); // query 已含在 URL 里
    setHeaders(
      p.headers.length ? p.headers.map((h) => ({ ...h, enabled: true })) : [emptyRow()]
    );
    if (p.body) {
      setBodyKind("raw");
      setBodyRaw(p.body);
      const ct = (
        p.contentType ||
        p.headers.find((h) => h.key.toLowerCase() === "content-type")?.value ||
        ""
      ).toLowerCase();
      setRawType(ct.includes("json") ? "json" : ct.includes("xml") ? "xml" : "text");
    } else {
      setBodyKind("none");
      setBodyRaw("");
    }
    setImportOpen(false);
    setImportText("");
    setImportErr(null);
    setError(null);
  };

  const finalUrl = useMemo(() => {
    const qs = params
      .filter((p) => p.enabled && p.key)
      .map((p) => `${encodeURIComponent(p.key)}=${encodeURIComponent(p.value)}`)
      .join("&");
    if (!qs) return url;
    return url + (url.includes("?") ? "&" : "?") + qs;
  }, [url, params]);

  const hasBody = method !== "GET" && method !== "HEAD";

  const send = async () => {
    if (!url.trim()) {
      setError("请输入 URL");
      return;
    }
    setLoading(true);
    setError(null);
    setResp(null);

    const hdrs: KV[] = headers
      .filter((h) => h.enabled && h.key)
      .map((h) => ({ ...h }));
    if (authType === "bearer" && bearer)
      hdrs.push({ key: "Authorization", value: `Bearer ${bearer}`, enabled: true });
    if (authType === "basic")
      hdrs.push({
        key: "Authorization",
        value: "Basic " + btoa(`${basicUser}:${basicPass}`),
        enabled: true,
      });

    const req = {
      method,
      url: finalUrl,
      headers: hdrs,
      bodyKind: hasBody ? bodyKind : "none",
      bodyRaw,
      contentType: RAW_TYPES[rawType],
      form: form.filter((f) => f.enabled && f.key),
      timeoutMs,
      followRedirects,
      verifyTls,
    };

    // 记录"真实请求"：把会被实际发送的 Content-Type、form 编码后的 body 也算进去
    const effKind = hasBody ? bodyKind : "none";
    const dispHeaders = hdrs.map((h) => ({ key: h.key, value: h.value }));
    const hasCT = () => dispHeaders.some((h) => h.key.toLowerCase() === "content-type");
    let sentBody = "";
    if (effKind === "raw") {
      sentBody = bodyRaw;
      if (sentBody && !hasCT()) dispHeaders.push({ key: "Content-Type", value: RAW_TYPES[rawType] });
    } else if (effKind === "form") {
      sentBody = form
        .filter((f) => f.enabled && f.key)
        .map((f) => `${encodeURIComponent(f.key)}=${encodeURIComponent(f.value)}`)
        .join("&");
      if (!hasCT())
        dispHeaders.push({ key: "Content-Type", value: "application/x-www-form-urlencoded" });
    }
    setSentReq({ method, url: finalUrl, headers: dispHeaders, body: sentBody });

    try {
      const r = await invoke<HttpResponse>("http_send", { req });
      setResp(r);
      setRespTab("body");
      setHistory((h) =>
        [{ method, url: finalUrl, status: r.status, ms: r.durationMs }, ...h].slice(0, 20)
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const statusClass = (s: number) =>
    s >= 500 ? "st-5xx" : s >= 400 ? "st-4xx" : s >= 300 ? "st-3xx" : "st-2xx";

  const displayBody = useMemo(() => {
    if (!resp) return "";
    if (resp.bodyIsBinary)
      return `（二进制响应，${resp.sizeBytes} 字节，已省略显示）`;
    if (!pretty) return resp.body;
    try {
      return JSON.stringify(JSON.parse(resp.body), null, 2);
    } catch {
      return resp.body;
    }
  }, [resp, pretty]);

  const requestText = sentReq
    ? `${sentReq.method} ${sentReq.url}\n` +
      sentReq.headers.map((h) => `${h.key}: ${h.value}`).join("\n") +
      (sentReq.body ? `\n\n${sentReq.body}` : "")
    : "";

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>HTTP 请求测试</h2>
        <div className="tool-actions">
          <button
            className="btn btn-ghost"
            onClick={() => {
              setImportErr(null);
              setImportOpen(true);
            }}
            title="从 cURL / fetch / PowerShell 导入"
          >
            导入
          </button>
          {history.length > 0 && (
            <select
              className="search-input"
              style={{ width: 240 }}
              value=""
              onChange={(e) => {
                const it = history[Number(e.target.value)];
                if (it) {
                  setMethod(it.method);
                  setUrl(it.url);
                  setParams([emptyRow()]);
                }
              }}
            >
              <option value="">历史记录…</option>
              {history.map((h, i) => (
                <option key={i} value={i}>
                  {h.status} · {h.method} {h.url.slice(0, 48)}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      {/* 请求行 */}
      <div className="req-bar">
        <select
          className="method-select"
          value={method}
          onChange={(e) => setMethod(e.target.value)}
        >
          {METHODS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
        <input
          className="url-input mono"
          placeholder="https://api.example.com/path"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && send()}
        />
        <button className="btn btn-primary" onClick={send} disabled={loading}>
          {loading ? "发送中…" : "发送"}
        </button>
      </div>

      {/* 请求配置 tabs */}
      <div className="sub-tabs">
        {TABS.map((t) => (
          <button
            key={t}
            className={`sub-tab${tab === t ? " active" : ""}`}
            onClick={() => setTab(t)}
          >
            {t}
            {t === "Body" && !hasBody ? "" : ""}
          </button>
        ))}
      </div>

      <div className="req-panel">
        {tab === "Params" && <KvEditor rows={params} onChange={setParams} />}
        {tab === "Headers" && <KvEditor rows={headers} onChange={setHeaders} />}
        {tab === "Body" && (
          <div>
            {!hasBody ? (
              <div className="dim">{method} 请求通常不带 body</div>
            ) : (
              <>
                <div className="body-kind">
                  {(["none", "raw", "form"] as const).map((k) => (
                    <label key={k} className="inline-check">
                      <input
                        type="radio"
                        name="bodyKind"
                        checked={bodyKind === k}
                        onChange={() => setBodyKind(k)}
                      />
                      {k === "none" ? "无" : k === "raw" ? "Raw" : "Form 表单"}
                    </label>
                  ))}
                  {bodyKind === "raw" && (
                    <select
                      className="mini-select"
                      value={rawType}
                      onChange={(e) => setRawType(e.target.value as typeof rawType)}
                    >
                      <option value="json">JSON</option>
                      <option value="text">Text</option>
                      <option value="xml">XML</option>
                    </select>
                  )}
                </div>
                {bodyKind === "raw" && (
                  <textarea
                    className="code-area"
                    style={{ minHeight: 160 }}
                    value={bodyRaw}
                    placeholder={rawType === "json" ? '{\n  "key": "value"\n}' : ""}
                    onChange={(e) => setBodyRaw(e.target.value)}
                    spellCheck={false}
                  />
                )}
                {bodyKind === "form" && <KvEditor rows={form} onChange={setForm} />}
              </>
            )}
          </div>
        )}
        {tab === "Auth" && (
          <div className="auth-panel">
            <div className="body-kind">
              {(["none", "bearer", "basic"] as const).map((k) => (
                <label key={k} className="inline-check">
                  <input
                    type="radio"
                    name="auth"
                    checked={authType === k}
                    onChange={() => setAuthType(k)}
                  />
                  {k === "none" ? "无" : k === "bearer" ? "Bearer Token" : "Basic"}
                </label>
              ))}
            </div>
            {authType === "bearer" && (
              <input
                className="kv-input mono"
                style={{ width: "100%", marginTop: 10 }}
                placeholder="token"
                value={bearer}
                onChange={(e) => setBearer(e.target.value)}
              />
            )}
            {authType === "basic" && (
              <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
                <input
                  className="kv-input"
                  placeholder="用户名"
                  value={basicUser}
                  onChange={(e) => setBasicUser(e.target.value)}
                />
                <input
                  className="kv-input"
                  type="password"
                  placeholder="密码"
                  value={basicPass}
                  onChange={(e) => setBasicPass(e.target.value)}
                />
              </div>
            )}
          </div>
        )}
        {tab === "选项" && (
          <div className="opts-panel">
            <label className="opt-row">
              <span>超时（毫秒）</span>
              <input
                type="number"
                className="kv-input"
                style={{ width: 100 }}
                value={timeoutMs}
                onChange={(e) => setTimeoutMs(Math.max(0, parseInt(e.target.value) || 0))}
              />
            </label>
            <label className="opt-row">
              <input
                type="checkbox"
                checked={followRedirects}
                onChange={(e) => setFollowRedirects(e.target.checked)}
              />
              <span>跟随重定向</span>
            </label>
            <label className="opt-row">
              <input
                type="checkbox"
                checked={!verifyTls}
                onChange={(e) => setVerifyTls(!e.target.checked)}
              />
              <span>忽略 TLS 证书错误（仅调试自签名时用）</span>
            </label>
          </div>
        )}
      </div>

      {error && <div className="error-banner">⚠ {error}</div>}

      {/* 导入 cURL / fetch / PowerShell */}
      {importOpen && (
        <div className="modal-overlay" onClick={() => setImportOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>导入请求</h3>
            <div className="dim" style={{ fontSize: 12 }}>
              粘贴浏览器「Copy as cURL / fetch / fetch (Node.js) / PowerShell」的内容，自动识别解析
            </div>
            <textarea
              className="code-area"
              style={{ minHeight: 180 }}
              placeholder={`curl 'https://api.example.com' -H 'accept: application/json' ...`}
              value={importText}
              onChange={(e) => setImportText(e.target.value)}
              spellCheck={false}
              autoFocus
            />
            {importErr && <div className="error-banner" style={{ margin: 0 }}>⚠ {importErr}</div>}
            <div className="modal-actions">
              <button className="btn btn-ghost" onClick={() => setImportOpen(false)}>
                取消
              </button>
              <button
                className="btn btn-primary"
                onClick={applyImport}
                disabled={!importText.trim()}
              >
                解析导入
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 请求 / 响应 */}
      {sentReq && (
        <div className="resp-panel">
          <div className="resp-meta">
            {resp ? (
              <>
                <span className={`status-pill ${statusClass(resp.status)}`}>
                  {resp.status} {resp.statusText}
                </span>
                <span className="dim">{resp.durationMs} ms</span>
                <span className="dim">{(resp.sizeBytes / 1024).toFixed(1)} KB</span>
                {resp.truncated && <span className="warn-text">（响应过大，已截断）</span>}
              </>
            ) : (
              <span className="dim">
                {loading ? "请求中…" : "请求已发送（响应见上方错误 / 见「请求」标签）"}
              </span>
            )}
            {respTab === "body" && resp && !resp.bodyIsBinary && (
              <>
                <label className="inline-check" style={{ marginLeft: 8 }}>
                  <input
                    type="checkbox"
                    checked={pretty}
                    onChange={(e) => setPretty(e.target.checked)}
                  />
                  美化
                </label>
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() => navigator.clipboard.writeText(resp.body)}
                >
                  复制
                </button>
              </>
            )}
            <div className="resp-tabs">
              <button
                className={`sub-tab${respTab === "request" ? " active" : ""}`}
                onClick={() => setRespTab("request")}
              >
                请求
              </button>
              <button
                className={`sub-tab${respTab === "body" ? " active" : ""}`}
                onClick={() => setRespTab("body")}
              >
                响应 Body
              </button>
              <button
                className={`sub-tab${respTab === "headers" ? " active" : ""}`}
                onClick={() => setRespTab("headers")}
              >
                响应 Headers ({resp ? resp.headers.length : 0})
              </button>
            </div>
          </div>
          <div className="resp-body">
            {respTab === "request" ? (
              <pre className="code-area mono">{requestText}</pre>
            ) : respTab === "body" ? (
              resp ? (
                <pre className="code-area mono">{displayBody}</pre>
              ) : (
                <div className="dim">无响应（请求可能失败，见顶部错误）</div>
              )
            ) : resp ? (
              <table>
                <tbody>
                  {resp.headers.map(([k, v], i) => (
                    <tr key={i}>
                      <td className="mono bold" style={{ width: 220, verticalAlign: "top" }}>
                        {k}
                      </td>
                      <td className="mono dim" style={{ wordBreak: "break-all" }}>
                        {v}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : (
              <div className="dim">无响应</div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
