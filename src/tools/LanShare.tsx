import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { useLan, type Peer, type FileMsg, type ChatItem } from "./lanContext";

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  if (d.toDateString() === now.toDateString()) return hm;
  return `${d.getMonth() + 1}-${d.getDate()} ${hm}`;
}

function deviceIcon(p: Peer): string {
  if (!p.isBaibao) return "📱";
  const t = p.deviceType ?? "";
  if (t === "mobile") return "📱";
  if (t === "web") return "🌐";
  return "💻";
}

function fileExtLabel(name: string): string {
  const e = name.split(".").pop()?.toLowerCase() ?? "";
  return e && e.length <= 4 ? e.toUpperCase() : "FILE";
}

// 微信式文件气泡：左侧文件名+状态，右侧文件类型图标
function FileBubble({
  f,
  onCancel,
  onAccept,
}: {
  f: FileMsg;
  onCancel: (sid: string) => void;
  onAccept: (sid: string) => void;
}) {
  const pct = f.size ? Math.min(100, (f.transferred / f.size) * 100) : 0;
  const clickable = f.status === "pending";
  const sub =
    f.status === "pending"
      ? "点击接收"
      : f.status === "cancelled"
      ? "已取消 / 已过期"
      : f.status === "failed"
      ? "传输失败"
      : f.status === "done"
      ? fmtBytes(f.size)
      : `${fmtBytes(f.transferred)} / ${fmtBytes(f.size)} · ${fmtBytes(f.speed)}/s`;

  return (
    <div
      className={`lan-filecard${clickable ? " clickable" : ""}`}
      onClick={clickable ? () => onAccept(f.sessionId) : undefined}
    >
      <div className="lan-file-main">
        <div className="lan-file-name" title={f.fileName}>
          {f.fileName}
          {f.verified === true && <span className="lan-verify-ok" title="校验通过"> ✓</span>}
          {f.verified === false && <span className="lan-verify-warn" title="校验不一致"> ⚠</span>}
        </div>
        {f.status === "active" && (
          <div className="lan-file-bar">
            <span className="lan-file-fill" style={{ width: `${pct}%` }} />
          </div>
        )}
        <div className="lan-file-sub dim">
          <span className={f.status === "pending" ? "lan-file-accept" : ""}>{sub}</span>
          {f.status === "active" && (
            <button
              className="lan-file-act"
              onClick={(e) => {
                e.stopPropagation();
                onCancel(f.sessionId);
              }}
            >
              取消
            </button>
          )}
          {f.status === "done" && f.direction === "in" && f.path && (
            <button
              className="lan-file-act"
              onClick={(e) => {
                e.stopPropagation();
                invoke("lan_reveal", { path: f.path });
              }}
            >
              打开位置
            </button>
          )}
        </div>
      </div>
      <span className="lan-file-ic">{fileExtLabel(f.fileName)}</span>
    </div>
  );
}

const TIME_GAP = 5 * 60 * 1000; // 超过 5 分钟显示一次时间分隔

export default function LanShare() {
  const {
    me, peers, items, unread, selected, error,
    setSelected, setError, refreshPeers, sendMessage, sendFiles,
    cancelTransfer, requestConfirm, addPeerByIp,
  } = useLan();

  const [draft, setDraft] = useState("");
  const [dragOver, setDragOver] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [addIp, setAddIp] = useState("");
  const [adding, setAdding] = useState(false);

  const bodyRef = useRef<HTMLDivElement>(null);
  const chatRef = useRef<HTMLDivElement>(null);
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;

  const selectedPeer = peers.find((p) => p.fingerprint === selected) ?? null;
  const thread = useMemo(
    () => items.filter((x) => x.fingerprint === selected).sort((a, b) => a.ts - b.ts),
    [items, selected]
  );

  useEffect(() => {
    if (chatRef.current) chatRef.current.scrollTop = chatRef.current.scrollHeight;
  }, [thread.length, selected]);

  // 拖拽发送：把文件拖进窗口松手发给当前选中设备（仅本面板可见时生效）
  useEffect(() => {
    let un: UnlistenFn | undefined;
    let alive = true;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        const visible = bodyRef.current && bodyRef.current.offsetParent !== null;
        if (!visible) return;
        const p = e.payload;
        if (p.type === "over") setDragOver(!!selectedRef.current);
        else if (p.type === "drop") {
          setDragOver(false);
          if (selectedRef.current && p.paths?.length) sendFiles(selectedRef.current, p.paths);
        } else setDragOver(false);
      })
      .then((u) => (alive ? (un = u) : u()));
    return () => {
      alive = false;
      un?.();
    };
  }, [sendFiles]);

  const send = () => {
    if (selected && draft.trim()) {
      sendMessage(selected, draft);
      setDraft("");
    }
  };

  const doAdd = async () => {
    if (!addIp.trim()) return;
    setAdding(true);
    try {
      await addPeerByIp(addIp.trim());
      setAddOpen(false);
      setAddIp("");
    } catch {
      /* 错误已进 error 条 */
    } finally {
      setAdding(false);
    }
  };

  // 渲染对话项：每条都带发送时间；间隔较大时再额外插一条居中时间分隔
  let lastTs = 0;
  const renderItem = (m: ChatItem) => {
    const incoming = m.kind === "msg" ? m.incoming : m.direction === "in";
    const showSep = m.ts - lastTs > TIME_GAP;
    lastTs = m.ts;
    return (
      <div key={m.id}>
        {showSep && <div className="lan-time-sep">{fmtTime(m.ts)}</div>}
        <div className={`lan-msg${incoming ? " in" : " out"}`}>
          <div className="lan-msg-col">
            {m.kind === "msg" ? (
              <div className={`lan-msg-bubble${m.failed ? " failed" : ""}`}>
                {m.text}
                {m.failed && <span className="lan-msg-fail" title="发送失败"> ⚠</span>}
              </div>
            ) : (
              <FileBubble f={m} onCancel={cancelTransfer} onAccept={requestConfirm} />
            )}
            <div className="lan-msg-time">{fmtTime(m.ts)}</div>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>局域网互传</h2>
        <div className="lan-status-inline">
          <span className={`lan-dot${me?.running ? " on" : ""}`} />
          {me?.running ? "服务运行中" : "未运行"}
          {me?.ip && <span className="dim">· 本机 {me.ip}:{me.port}</span>}
        </div>
      </div>

      {error && (
        <div className="error-banner">
          ⚠ {error}
          <button className="lan-err-x" onClick={() => setError(null)}>×</button>
        </div>
      )}

      <div className="lan-body" ref={bodyRef}>
        <div className="lan-peers">
          <div className="lan-section-title">
            设备（{peers.length}）
            <span>
              <button className="btn btn-ghost btn-sm" onClick={() => setAddOpen(true)}>+ IP</button>
              <button className="btn btn-ghost btn-sm" onClick={refreshPeers}>刷新</button>
            </span>
          </div>
          {peers.length === 0 && (
            <div className="dim lan-empty">
              暂未发现设备。<br />
              确保对方也开着本工具、在同一局域网，防火墙放行 UDP/TCP {me?.port ?? 53317}。
              <br />网络隔离多播时，可用「+ IP」手动添加。
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
              {unread[p.fingerprint] > 0 && <span className="lan-badge">{unread[p.fingerprint]}</span>}
            </button>
          ))}
        </div>

        <div className="lan-main">
          {!selectedPeer ? (
            <div className="dim lan-placeholder">← 从左侧选择一台设备开始</div>
          ) : (
            <>
              <div className="lan-chat-head">
                <b>{selectedPeer.alias}</b>
                <span className="dim">{selectedPeer.ip}</span>
              </div>

              <div className="lan-chat" ref={chatRef}>
                {thread.length === 0 && (
                  <div className="dim">还没有消息。可发消息，或用下方 📎 / 拖拽发送文件。</div>
                )}
                {thread.map(renderItem)}
              </div>

              {/* 输入区：上方工具条 + 文本框 */}
              <div className="lan-composer">
                <div className="lan-toolbar">
                  <button
                    className="lan-tool-btn"
                    title="发送文件"
                    onClick={() => sendFiles(selectedPeer.fingerprint)}
                  >
                    📎
                  </button>
                </div>
                <div className="lan-input-row">
                  <input
                    className="lan-input"
                    placeholder={`给 ${selectedPeer.alias} 发消息…`}
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && send()}
                  />
                  <button className="btn btn-primary" onClick={send} disabled={!draft.trim()}>
                    发送
                  </button>
                </div>
              </div>
            </>
          )}

          {dragOver && (
            <div className="lan-drop-mask">
              松手发送给 <b>{selectedPeer?.alias}</b>
            </div>
          )}
        </div>
      </div>

      {addOpen && (
        <div className="modal-overlay" onClick={() => setAddOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>按 IP 添加设备</h3>
            <div className="dim" style={{ fontSize: 12 }}>
              多播被网络隔离时使用。输入对方本机 IP（端口默认 {me?.port ?? 53317}）。
            </div>
            <input
              className="kv-input"
              style={{ width: "100%", marginTop: 10 }}
              placeholder="192.168.1.23"
              value={addIp}
              autoFocus
              onChange={(e) => setAddIp(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && doAdd()}
            />
            <div className="modal-actions">
              <button className="btn btn-ghost" onClick={() => setAddOpen(false)}>取消</button>
              <button className="btn btn-primary" onClick={doAdd} disabled={!addIp.trim() || adding}>
                {adding ? "连接中…" : "添加"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
