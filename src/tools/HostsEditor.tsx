import { useState, useEffect, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Line {
  id: number;
  kind: "entry" | "other"; // entry = IP→域名映射；other = 注释/空行，原样保留
  enabled: boolean;
  ip: string;
  hosts: string;
  comment: string;
  raw: string;
}

let _uid = 0;
const uid = () => ++_uid;

function parseLine(raw: string): Line {
  const id = uid();
  if (raw.trim() === "")
    return { id, kind: "other", enabled: true, ip: "", hosts: "", comment: "", raw };

  // 行首单个 # 视为「停用的条目」候选
  let enabled = true;
  let work = raw;
  const m = raw.match(/^\s*#\s?(.*)$/);
  if (m) {
    enabled = false;
    work = m[1];
  }

  // 拆出行尾备注
  let comment = "";
  let body = work;
  const hashIdx = work.indexOf("#");
  if (hashIdx >= 0) {
    comment = work.slice(hashIdx + 1).trim();
    body = work.slice(0, hashIdx);
  }

  const tokens = body.trim().split(/\s+/).filter(Boolean);
  const looksIp =
    tokens.length >= 2 &&
    /^[0-9a-fA-F:.]+$/.test(tokens[0]) &&
    (tokens[0].includes(".") || tokens[0].includes(":"));

  if (looksIp) {
    return {
      id,
      kind: "entry",
      enabled,
      ip: tokens[0],
      hosts: tokens.slice(1).join(" "),
      comment,
      raw,
    };
  }
  // 纯注释 / 无法识别 → 原样保留
  return { id, kind: "other", enabled: true, ip: "", hosts: "", comment: "", raw };
}

const parse = (text: string): Line[] =>
  text.replace(/\r\n/g, "\n").split("\n").map(parseLine);

function serializeLine(l: Line): string {
  if (l.kind === "other") return l.raw;
  const prefix = l.enabled ? "" : "# ";
  const cmt = l.comment ? `  # ${l.comment}` : "";
  return `${prefix}${l.ip}\t${l.hosts}${cmt}`;
}

// 一条映射是否完整：IP 和域名都不能为空，否则写进 hosts 就是非法行
const isEntryComplete = (l: Line) => l.ip.trim() !== "" && l.hosts.trim() !== "";

// IP 看起来是否像 IP（含 . 或 :），用于软提示（不阻断）
const looksLikeIp = (ip: string) => ip.includes(".") || ip.includes(":");

const serialize = (lines: Line[]): string =>
  lines
    // 不完整的 entry 直接丢弃，避免写出 "  域名" 这种无 IP 的坏行；注释/空行保留
    .filter((l) => l.kind === "other" || isEntryComplete(l))
    .map(serializeLine)
    .join("\n");

const isSystem = (l: Line) =>
  (l.ip === "127.0.0.1" || l.ip === "::1" || l.ip === "255.255.255.255") &&
  /\b(localhost|broadcasthost)\b/.test(l.hosts);

export default function HostsEditor({
  onDirty,
}: {
  onDirty?: (dirty: boolean) => void;
} = {}) {
  const [lines, setLines] = useState<Line[]>([]);
  const [original, setOriginal] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [raw, setRaw] = useState(false);
  const [rawText, setRawText] = useState("");
  const [flushDns, setFlushDns] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    setNotice(null);
    try {
      const text = await invoke<string>("read_hosts");
      const parsed = parse(text);
      setLines(parsed);
      // 用归一化后的文本作基线，dirty 判断才准确（否则一打开就被判为"脏"）
      setOriginal(serialize(parsed));
      setRaw(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const currentText = raw ? rawText : serialize(lines);
  const dirty = currentText !== original;

  // 把"有未保存改动"上报给外层，用于 tab 上的指示灯
  useEffect(() => {
    onDirty?.(dirty);
  }, [dirty, onDirty]);

  const enterRaw = () => {
    setRawText(serialize(lines));
    setRaw(true);
  };
  const exitRaw = () => {
    setLines(parse(rawText));
    setRaw(false);
  };

  const update = (id: number, patch: Partial<Line>) =>
    setLines((ls) => ls.map((l) => (l.id === id ? { ...l, ...patch } : l)));
  const remove = (id: number) => setLines((ls) => ls.filter((l) => l.id !== id));
  const add = () =>
    setLines((ls) => [
      ...ls,
      { id: uid(), kind: "entry", enabled: true, ip: "", hosts: "", comment: "", raw: "" },
    ]);

  const save = async () => {
    const content = raw ? rawText : serialize(lines);
    // 不再用 window.confirm —— 它在 Tauri WebView 里不可靠（会静默返回导致保存无效）。
    // 写入 /etc/hosts 时系统会弹出管理员密码框，那本身就是确认与授权。
    setSaving(true);
    setError(null);
    setNotice(null);
    const skipped = raw
      ? 0
      : lines.filter((l) => l.kind === "entry" && !isEntryComplete(l)).length;
    try {
      const msg = await invoke<string>("write_hosts", { content, flushDns });
      setOriginal(content);
      if (raw) setLines(parse(content));
      setNotice(
        msg + (skipped ? `（已忽略 ${skipped} 条不完整记录：缺 IP 或域名）` : "")
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const entries = useMemo(() => lines.filter((l) => l.kind === "entry"), [lines]);
  const incomplete = useMemo(
    () => entries.filter((e) => !isEntryComplete(e)).length,
    [entries]
  );
  const visible = useMemo(() => {
    if (!filter) return entries;
    const q = filter.toLowerCase();
    return entries.filter(
      (e) =>
        e.ip.toLowerCase().includes(q) ||
        e.hosts.toLowerCase().includes(q) ||
        e.comment.toLowerCase().includes(q)
    );
  }, [entries, filter]);

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>
          Hosts 编辑器
          {dirty && <span className="dirty-dot" title="有未保存改动" />}
        </h2>
        <div className="tool-actions">
          {!raw && (
            <input
              className="search-input"
              placeholder="搜索 IP、域名、备注…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          )}
          <label className="inline-check" title="保存后刷新系统 DNS 缓存">
            <input
              type="checkbox"
              checked={flushDns}
              onChange={(e) => setFlushDns(e.target.checked)}
            />
            刷新 DNS
          </label>
          <div className="seg-toggle" title="结构化表格 / 原始文本">
            <button
              className={`seg${!raw ? " active" : ""}`}
              onClick={() => raw && exitRaw()}
            >
              结构
            </button>
            <button
              className={`seg${raw ? " active" : ""}`}
              onClick={() => !raw && enterRaw()}
            >
              原文
            </button>
          </div>
          <button className="btn btn-ghost" onClick={load} disabled={loading || saving}>
            重载
          </button>
          <button className="btn btn-primary" onClick={save} disabled={saving || !dirty}>
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>

      {error && <div className="error-banner">⚠ {error}</div>}
      {notice && <div className="notice-banner">✓ {notice}</div>}

      <div className="table-wrap">
        {raw ? (
          <textarea
            className="code-area"
            value={rawText}
            onChange={(e) => setRawText(e.target.value)}
            spellCheck={false}
          />
        ) : (
          <table>
            <thead>
              <tr>
                <th style={{ width: 56 }}>启用</th>
                <th style={{ width: 160 }}>IP</th>
                <th>域名（空格分隔多个）</th>
                <th style={{ width: 180 }}>备注</th>
                <th style={{ width: 60 }}>操作</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((l) => (
                <tr key={l.id} className={l.enabled ? "" : "row-off"}>
                  <td>
                    <input
                      type="checkbox"
                      checked={l.enabled}
                      onChange={(e) => update(l.id, { enabled: e.target.checked })}
                    />
                  </td>
                  <td>
                    <input
                      className={`cell-input mono${
                        !l.ip.trim() || !looksLikeIp(l.ip) ? " cell-warn" : ""
                      }`}
                      value={l.ip}
                      placeholder="127.0.0.1"
                      title={
                        !l.ip.trim()
                          ? "缺少 IP，此行不会被保存"
                          : !looksLikeIp(l.ip)
                          ? "这看起来不像 IP"
                          : ""
                      }
                      onChange={(e) => update(l.id, { ip: e.target.value })}
                    />
                  </td>
                  <td>
                    <input
                      className={`cell-input mono${!l.hosts.trim() ? " cell-warn" : ""}`}
                      value={l.hosts}
                      placeholder="example.com"
                      title={!l.hosts.trim() ? "缺少域名，此行不会被保存" : ""}
                      onChange={(e) => update(l.id, { hosts: e.target.value })}
                    />
                    {isSystem(l) && <span className="tag-sys">系统</span>}
                  </td>
                  <td>
                    <input
                      className="cell-input dim"
                      value={l.comment}
                      placeholder="—"
                      onChange={(e) => update(l.id, { comment: e.target.value })}
                    />
                  </td>
                  <td>
                    <button
                      className="btn btn-danger btn-sm"
                      onClick={() => remove(l.id)}
                    >
                      删除
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {!raw && (
          <div style={{ padding: "10px 14px" }}>
            <button className="btn btn-ghost btn-sm" onClick={add}>
              + 新增一条
            </button>
          </div>
        )}
      </div>

      <div className="tool-footer">
        <span>
          {loading ? "读取中…" : `${entries.length} 条映射`}
          {dirty ? " · 有未保存改动" : ""}
          {incomplete > 0 && (
            <span className="warn-text">
              {" "}
              · {incomplete} 条不完整不会被保存
            </span>
          )}
        </span>
        <span className="dim" style={{ marginLeft: "auto" }}>
          注释行与空行会原样保留
        </span>
      </div>
    </div>
  );
}
