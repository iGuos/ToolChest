import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface MyInfo {
  alias: string;
  fingerprint: string;
  port: number;
  compat: boolean;
  downloadDir: string;
}
interface Peer {
  alias: string;
  fingerprint: string;
  ip: string;
  port: number;
  protocol: string;
  deviceType?: string | null;
  isBaibao: boolean;
  lastSeenMs: number;
}
interface FileMeta {
  id: string;
  fileName: string;
  size: number;
  fileType: string;
}
interface Incoming {
  sessionId: string;
  alias: string;
  fingerprint: string;
  isBaibao: boolean;
  files: FileMeta[];
}
interface ChatMsg {
  fingerprint: string;
  alias: string;
  text: string;
  ts: number;
  incoming: boolean;
}
interface Transfer {
  key: string;
  fileName: string;
  transferred: number;
  size: number;
  direction: "in" | "out";
  done: boolean;
}
interface Received {
  fileName: string;
  path: string;
  size: number;
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function deviceIcon(p: Peer): string {
  if (!p.isBaibao) return "📱"; // 外部 LocalSend 设备
  const t = p.deviceType ?? "";
  if (t === "mobile") return "📱";
  if (t === "web") return "🌐";
  return "💻";
}

export default function LanShare() {
  const [me, setMe] = useState<MyInfo | null>(null);
  const [aliasDraft, setAliasDraft] = useState("");
  const [peers, setPeers] = useState<Peer[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const [received, setReceived] = useState<Received[]>([]);
  const [incoming, setIncoming] = useState<Incoming | null>(null);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const startedRef = useRef(false);

  const selectedPeer = peers.find((p) => p.fingerprint === selected) ?? null;

  const refreshPeers = useCallback(async () => {
    try {
      setPeers(await invoke<Peer[]>("lan_peers"));
    } catch {
      /* ignore */
    }
  }, []);

  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;
    const unlisteners: UnlistenFn[] = [];

    (async () => {
      try {
        const info = await invoke<MyInfo>("lan_start");
        setMe(info);
        setAliasDraft(info.alias);
        await refreshPeers();
      } catch (e) {
        setError(String(e));
      }

      unlisteners.push(
        await listen<Peer[]>("lan://peers", (e) => setPeers(e.payload)),
        await listen<Incoming>("lan://incoming", (e) => setIncoming(e.payload)),
        await listen<ChatMsg>("lan://message", (e) =>
          setMessages((m) => [...m, e.payload])
        ),
        await listen<Transfer & { fileId: string }>("lan://progress", (e) => {
          const p = e.payload;
          const key = `${p.direction}:${p.fileId}`;
          setTransfers((list) => {
            const idx = list.findIndex((t) => t.key === key);
            const next: Transfer = {
              key,
              fileName: p.fileName,
              transferred: p.transferred,
              size: p.size,
              direction: p.direction,
              done: p.transferred >= p.size && p.size > 0,
            };
            if (idx < 0) return [next, ...list].slice(0, 50);
            const copy = [...list];
            copy[idx] = next;
            return copy;
          });
        }),
        await listen<Received>("lan://received", (e) => {
          setReceived((r) => [e.payload, ...r].slice(0, 50));
        })
      );
    })();

    return () => {
      unlisteners.forEach((u) => u());
    };
  }, [refreshPeers]);

  const sendMessage = async () => {
    if (!selected || !draft.trim() || !me) return;
    const text = draft.trim();
    setDraft("");
    setMessages((m) => [
      ...m,
      { fingerprint: selected, alias: me.alias, text, ts: Date.now(), incoming: false },
    ]);
    try {
      await invoke("lan_send_message", { fingerprint: selected, text });
    } catch (e) {
      setError(String(e));
    }
  };

  const sendFiles = async () => {
    if (!selected) return;
    setError(null);
    try {
      const paths = await invoke<string[]>("lan_pick_files");
      if (!paths.length) return;
      setSending(true);
      await invoke("lan_send_files", { fingerprint: selected, paths });
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  };

  const respond = async (accept: boolean) => {
    if (!incoming) return;
    const sessionId = incoming.sessionId;
    setIncoming(null);
    try {
      await invoke("lan_respond", { sessionId, accept, fileIds: [] });
    } catch (e) {
      setError(String(e));
    }
  };

  const saveAlias = async () => {
    if (!me || aliasDraft.trim() === me.alias) return;
    await invoke("lan_set_alias", { alias: aliasDraft.trim() });
    setMe({ ...me, alias: aliasDraft.trim() });
  };

  const toggleCompat = async () => {
    if (!me) return;
    const next = !me.compat;
    await invoke("lan_set_compat", { enabled: next });
    setMe({ ...me, compat: next });
  };

  const pickDir = async () => {
    const dir = await invoke<string | null>("lan_pick_dir");
    if (dir && me) {
      await invoke("lan_set_dir", { dir });
      setMe({ ...me, downloadDir: dir });
    }
  };

  const chat = messages.filter((m) => m.fingerprint === selected);

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>局域网互传</h2>
        <div className="tool-actions lan-actions">
          <span className="dim">我是</span>
          <input
            className="kv-input"
            style={{ width: 160 }}
            value={aliasDraft}
            onChange={(e) => setAliasDraft(e.target.value)}
            onBlur={saveAlias}
            onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
          />
          <label className="inline-check" title="开启后可与手机/电脑上的 LocalSend 互传">
            <input type="checkbox" checked={!!me?.compat} onChange={toggleCompat} />
            兼容 LocalSend
          </label>
        </div>
      </div>

      {error && <div className="error-banner">⚠ {error}</div>}

      <div className="lan-body">
        {/* 设备列表 */}
        <div className="lan-peers">
          <div className="lan-section-title">
            局域网设备（{peers.length}）
            <button className="btn btn-ghost btn-sm" onClick={refreshPeers}>
              刷新
            </button>
          </div>
          {peers.length === 0 && (
            <div className="dim lan-empty">
              暂未发现设备。<br />
              确保对方也开着本工具、在同一局域网，且防火墙放行 UDP/TCP {me?.port ?? 53317}。
            </div>
          )}
          {peers.map((p) => (
            <button
              key={p.fingerprint}
              className={`lan-peer${selected === p.fingerprint ? " active" : ""}`}
              onClick={() => setSelected(p.fingerprint)}
            >
              <span className="lan-peer-icon">{deviceIcon(p)}</span>
              <span className="lan-peer-info">
                <span className="lan-peer-alias">{p.alias}</span>
                <span className="lan-peer-sub dim">
                  {p.ip} {p.isBaibao ? "· 百宝箱" : "· LocalSend"}
                </span>
              </span>
            </button>
          ))}
        </div>

        {/* 会话 / 发送 */}
        <div className="lan-main">
          {!selectedPeer ? (
            <div className="dim lan-placeholder">← 从左侧选择一台设备开始</div>
          ) : (
            <>
              <div className="lan-chat-head">
                <b>{selectedPeer.alias}</b>
                <span className="dim">{selectedPeer.ip}</span>
                <button
                  className="btn btn-primary btn-sm"
                  onClick={sendFiles}
                  disabled={sending}
                >
                  {sending ? "发送中…" : "📎 发送文件"}
                </button>
              </div>

              <div className="lan-chat">
                {chat.length === 0 && <div className="dim">还没有消息</div>}
                {chat.map((m, i) => (
                  <div
                    key={i}
                    className={`lan-msg${m.incoming ? " in" : " out"}`}
                  >
                    <div className="lan-msg-bubble">{m.text}</div>
                  </div>
                ))}
              </div>

              <div className="lan-input-row">
                <input
                  className="url-input"
                  placeholder={`给 ${selectedPeer.alias} 发消息…`}
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && sendMessage()}
                />
                <button className="btn btn-primary" onClick={sendMessage} disabled={!draft.trim()}>
                  发送
                </button>
              </div>
            </>
          )}
        </div>
      </div>

      {/* 传输 / 已接收 */}
      {(transfers.length > 0 || received.length > 0) && (
        <div className="lan-foot">
          {transfers.length > 0 && (
            <div className="lan-foot-col">
              <div className="lan-section-title">传输</div>
              {transfers.map((t) => (
                <div key={t.key} className="lan-xfer">
                  <span className="lan-xfer-dir">{t.direction === "in" ? "↓" : "↑"}</span>
                  <span className="lan-xfer-name" title={t.fileName}>{t.fileName}</span>
                  <span className="lan-xfer-bar">
                    <span
                      className="lan-xfer-fill"
                      style={{ width: `${t.size ? Math.min(100, (t.transferred / t.size) * 100) : 0}%` }}
                    />
                  </span>
                  <span className="dim lan-xfer-pct">
                    {t.done ? "完成" : `${fmtBytes(t.transferred)}/${fmtBytes(t.size)}`}
                  </span>
                </div>
              ))}
            </div>
          )}
          {received.length > 0 && (
            <div className="lan-foot-col">
              <div className="lan-section-title">
                已接收 → <span className="dim" title={me?.downloadDir}>{me?.downloadDir}</span>
              </div>
              {received.map((r, i) => (
                <div key={i} className="lan-recv">
                  <span className="lan-xfer-name" title={r.path}>{r.fileName}</span>
                  <span className="dim">{fmtBytes(r.size)}</span>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() => invoke("lan_reveal", { path: r.path })}
                  >
                    打开位置
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* 收到文件请求 → 确认 */}
      {incoming && (
        <div className="modal-overlay" onClick={() => respond(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>收到文件请求</h3>
            <div className="dim" style={{ fontSize: 13 }}>
              <b>{incoming.alias}</b>（{incoming.isBaibao ? "百宝箱" : "LocalSend"}）想发送 {incoming.files.length} 个文件：
            </div>
            <div className="lan-req-files">
              {incoming.files.map((f) => (
                <div key={f.id} className="lan-req-file">
                  <span className="lan-xfer-name" title={f.fileName}>{f.fileName}</span>
                  <span className="dim">{fmtBytes(f.size)}</span>
                </div>
              ))}
            </div>
            <div className="modal-actions">
              <button className="btn btn-ghost" onClick={() => respond(false)}>
                拒绝
              </button>
              <button className="btn btn-primary" onClick={() => respond(true)}>
                接受并保存
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
