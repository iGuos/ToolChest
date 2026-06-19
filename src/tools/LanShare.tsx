import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { useLan, type Peer } from "./lanContext";

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function deviceIcon(p: Peer): string {
  if (!p.isBaibao) return "📱";
  const t = p.deviceType ?? "";
  if (t === "mobile") return "📱";
  if (t === "web") return "🌐";
  return "💻";
}

export default function LanShare() {
  const lan = useLan();
  const {
    me,
    peers,
    messages,
    transfers,
    received,
    unread,
    selected,
    error,
    setSelected,
    setError,
    refreshPeers,
    sendMessage,
    sendFiles,
    cancelTransfer,
    clearFinishedTransfers,
    setAlias,
    setCompat,
    pickDir,
    addPeerByIp,
  } = lan;

  const [aliasDraft, setAliasDraft] = useState("");
  const [draft, setDraft] = useState("");
  const [dragOver, setDragOver] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [addIp, setAddIp] = useState("");
  const [adding, setAdding] = useState(false);

  const bodyRef = useRef<HTMLDivElement>(null);
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;

  useEffect(() => {
    if (me) setAliasDraft((d) => (d ? d : me.alias));
  }, [me]);

  const selectedPeer = peers.find((p) => p.fingerprint === selected) ?? null;
  const chat = messages.filter((m) => m.fingerprint === selected);

  // 拖拽发送：把文件拖到窗口松手，发给当前选中的设备（仅本面板可见时生效）
  useEffect(() => {
    let un: UnlistenFn | undefined;
    let alive = true;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        const visible = bodyRef.current && bodyRef.current.offsetParent !== null;
        if (!visible) return;
        const p = e.payload;
        if (p.type === "over") {
          setDragOver(!!selectedRef.current);
        } else if (p.type === "drop") {
          setDragOver(false);
          if (selectedRef.current && p.paths?.length) {
            sendFiles(selectedRef.current, p.paths);
          }
        } else {
          setDragOver(false);
        }
      })
      .then((u) => {
        if (alive) un = u;
        else u();
      });
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

  const activeTransfers = transfers.filter((t) => !t.done);

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>局域网互传</h2>
        <div className="tool-actions lan-actions">
          <span className="dim">我是</span>
          <input
            className="kv-input"
            style={{ width: 150 }}
            value={aliasDraft}
            onChange={(e) => setAliasDraft(e.target.value)}
            onBlur={() => aliasDraft.trim() && aliasDraft.trim() !== me?.alias && setAlias(aliasDraft)}
            onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
          />
          <label className="inline-check" title="开启后可与手机/电脑上的 LocalSend 互传">
            <input
              type="checkbox"
              checked={!!me?.compat}
              onChange={(e) => setCompat(e.target.checked)}
            />
            兼容 LocalSend
          </label>
        </div>
      </div>

      {/* 状态条 */}
      <div className="lan-statusbar">
        <span className={`lan-dot${me?.running ? " on" : ""}`} />
        {me?.running ? "服务运行中" : "未运行"}
        {me?.ip && <span className="dim">· 本机 {me.ip}:{me.port}</span>}
        <span className="dim">· 接收目录</span>
        <span className="lan-dir" title={me?.downloadDir}>{me?.downloadDir}</span>
        <button className="btn btn-ghost btn-sm" onClick={pickDir}>更改</button>
      </div>

      {error && (
        <div className="error-banner">
          ⚠ {error}
          <button className="lan-err-x" onClick={() => setError(null)}>×</button>
        </div>
      )}

      <div className="lan-body" ref={bodyRef}>
        {/* 设备列表 */}
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
              <br />网络隔离多播时，可用右上「+ IP」手动添加。
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
              {unread[p.fingerprint] > 0 && (
                <span className="lan-badge">{unread[p.fingerprint]}</span>
              )}
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
                <button className="btn btn-primary btn-sm" onClick={() => sendFiles(selectedPeer.fingerprint)}>
                  📎 发送文件
                </button>
              </div>

              <div className="lan-chat">
                {chat.length === 0 && (
                  <div className="dim">还没有消息。可发消息，或把文件拖进来发送。</div>
                )}
                {chat.map((m) => (
                  <div key={m.id} className={`lan-msg${m.incoming ? " in" : " out"}`}>
                    <div className={`lan-msg-bubble${m.failed ? " failed" : ""}`}>
                      {m.text}
                      {m.failed && <span className="lan-msg-fail" title="发送失败"> ⚠</span>}
                    </div>
                  </div>
                ))}
              </div>

              <div className="lan-input-row">
                <input
                  className="url-input"
                  placeholder={`给 ${selectedPeer.alias} 发消息…`}
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && send()}
                />
                <button className="btn btn-primary" onClick={send} disabled={!draft.trim()}>
                  发送
                </button>
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

      {/* 传输 / 已接收 */}
      {(transfers.length > 0 || received.length > 0) && (
        <div className="lan-foot">
          {transfers.length > 0 && (
            <div className="lan-foot-col">
              <div className="lan-section-title">
                传输（{activeTransfers.length} 进行中）
                <button className="btn btn-ghost btn-sm" onClick={clearFinishedTransfers}>
                  清理已完成
                </button>
              </div>
              {transfers.map((t) => {
                const pct = t.size ? Math.min(100, (t.transferred / t.size) * 100) : 0;
                return (
                  <div key={t.key} className="lan-xfer">
                    <span className="lan-xfer-dir">{t.direction === "in" ? "↓" : "↑"}</span>
                    <span className="lan-xfer-name" title={t.fileName}>{t.fileName}</span>
                    <span className="lan-xfer-bar">
                      <span
                        className={`lan-xfer-fill${t.cancelled ? " cancelled" : ""}`}
                        style={{ width: `${pct}%` }}
                      />
                    </span>
                    <span className="dim lan-xfer-pct">
                      {t.cancelled
                        ? "已取消"
                        : t.done
                        ? "完成"
                        : `${fmtBytes(t.transferred)}/${fmtBytes(t.size)} · ${fmtBytes(t.speed)}/s`}
                    </span>
                    {!t.done && (
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => cancelTransfer(t.sessionId)}
                      >
                        取消
                      </button>
                    )}
                  </div>
                );
              })}
            </div>
          )}
          {received.length > 0 && (
            <div className="lan-foot-col">
              <div className="lan-section-title">已接收</div>
              {received.map((r, i) => (
                <div key={i} className="lan-recv">
                  <span className="lan-xfer-name" title={r.path}>
                    {r.fileName}
                    {r.verified === false && <span className="lan-verify-warn" title="校验不一致"> ⚠</span>}
                    {r.verified === true && <span className="lan-verify-ok" title="校验通过"> ✓</span>}
                  </span>
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

      {/* 手动按 IP 添加 */}
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
