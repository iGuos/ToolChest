import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
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

// 文字气泡：气泡内文本可鼠标选中，右键弹出菜单（复制 / 撤回 / 删除）。
// 发送失败时文字左侧显示红色警告三角 + 白色重试按钮：点击重试时三角消失、按钮旋转，
// 若再次失败则三角重新出现。
function TextBubble({
  text,
  failed,
  onContextMenu,
  onRetry,
}: {
  text: string;
  failed?: boolean;
  onContextMenu?: (e: React.MouseEvent) => void;
  onRetry?: () => Promise<void> | void;
}) {
  const [retrying, setRetrying] = useState(false);
  const handleRetry = async () => {
    if (retrying || !onRetry) return;
    setRetrying(true);
    try {
      await onRetry();
    } finally {
      setRetrying(false);
    }
  };
  return (
    <div className="lan-msg-textwrap">
      {onRetry && (
        <button
          className={`lan-msg-retry${retrying ? " spinning" : ""}`}
          title="重新发送"
          onClick={handleRetry}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 12a9 9 0 1 1-2.64-6.36" />
            <polyline points="21 3 21 9 15 9" />
          </svg>
        </button>
      )}
      <div
        className={`lan-msg-bubble${failed ? " failed" : ""}`}
        onContextMenu={onContextMenu}
      >
        {failed && !retrying && (
          <span className="lan-msg-fail" title="发送失败">⚠</span>
        )}
        {text}
      </div>
    </div>
  );
}

// 微信式文件气泡：左侧文件名+状态，右侧文件类型图标
function FileBubble({
  f,
  onCancel,
  onCancelSend,
  onAccept,
  onContextMenu,
}: {
  f: FileMsg;
  onCancel: (sid: string) => void;
  onCancelSend: (fileId: string) => void;
  onAccept: (sid: string) => void;
  onContextMenu?: (e: React.MouseEvent) => void;
}) {
  const pct = f.size ? Math.min(100, (f.transferred / f.size) * 100) : 0;
  // 只有「收到的待接收文件」才可点击接收；发出去的「待发送」气泡不可点
  const clickable = f.direction === "in" && f.status === "pending";
  const sub =
    f.status === "pending"
      ? f.direction === "in"
        ? "点击接收"
        : "等待对方接收…"
      : f.status === "cancelled"
      ? "已取消 / 已过期"
      : f.status === "failed"
      ? f.failReason ?? "传输失败"
      : f.status === "done"
      ? fmtBytes(f.size)
      : `${fmtBytes(f.transferred)} / ${fmtBytes(f.size)} · ${fmtBytes(f.speed)}/s`;

  return (
    <div
      className={`lan-filecard${clickable ? " clickable" : ""}`}
      onClick={clickable ? () => onAccept(f.sessionId) : undefined}
      onContextMenu={onContextMenu}
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
          <span className={clickable ? "lan-file-accept" : ""}>{sub}</span>
          {f.direction === "out" && f.status === "pending" && (
            <button
              className="lan-file-act"
              onClick={(e) => {
                e.stopPropagation();
                onCancelSend(f.fileId);
              }}
            >
              取消发送
            </button>
          )}
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
const RECALL_WINDOW = 5 * 60 * 1000; // 文本消息可撤回的时限（5 分钟，参考微信）

export default function LanShare() {
  const {
    me, peers, items, unread, selected, error,
    setSelected, setError, refreshPeers, sendMessage, resendMessage, recallMessage, deleteItem, sendFiles,
    cancelTransfer, cancelSend, requestConfirm, addPeerByIp, setAlias, setCompat, setInvisible, pickDir,
  } = useLan();

  const [draft, setDraft] = useState("");
  const [dragOver, setDragOver] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [addIp, setAddIp] = useState("");
  const [adding, setAdding] = useState(false);
  const [setOpen, setSetOpen] = useState(false);
  const [aliasDraft, setAliasDraft] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [statusOpen, setStatusOpen] = useState(false); // 在线/隐身下拉
  // 点击空白 / Esc 关闭在线状态下拉
  useEffect(() => {
    if (!statusOpen) return;
    const close = () => setStatusOpen(false);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setStatusOpen(false);
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [statusOpen]);
  // 每 20s 走一次时钟，用于让「撤回」在超过 5 分钟后从右键菜单消失
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 20000);
    return () => window.clearInterval(t);
  }, []);
  // 错误提示：顶部居中浮动框，5 秒后自动消失
  useEffect(() => {
    if (!error) return;
    const t = window.setTimeout(() => setError(null), 5000);
    return () => window.clearTimeout(t);
  }, [error, setError]);
  // 消息右键菜单：文本消息（复制 / 撤回 / 删除）或文件记录（删除）
  const [msgMenu, setMsgMenu] = useState<
    | {
        x: number;
        y: number;
        id: string;
        isFile: boolean;
        fingerprint?: string;
        text?: string;
        canRecall?: boolean;
      }
    | null
  >(null);
  // 打开菜单后：点击空白 / 再次右键 / 滚动 / Esc 关闭
  useEffect(() => {
    if (!msgMenu) return;
    const close = () => setMsgMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setMsgMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [msgMenu]);
  const copyMsg = async (text: string) => {
    const sel = window.getSelection()?.toString();
    try {
      await navigator.clipboard.writeText(sel || text);
    } catch {
      /* 复制失败静默 */
    }
    setMsgMenu(null);
  };

  // 刷新设备列表：点击时图标转起来，至少转 0.6s 让动效可见
  const handleRefresh = async () => {
    if (refreshing) return;
    setRefreshing(true);
    const started = Date.now();
    try {
      await refreshPeers();
    } finally {
      const wait = Math.max(0, 600 - (Date.now() - started));
      window.setTimeout(() => setRefreshing(false), wait);
    }
  };

  useEffect(() => {
    if (setOpen && me) setAliasDraft(me.alias);
  }, [setOpen, me]);

  const bodyRef = useRef<HTMLDivElement>(null);
  const chatRef = useRef<HTMLDivElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  // 输入框随内容自动增高（封顶 120px，超出滚动）
  useEffect(() => {
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  }, [draft, selected]);
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
    // 已撤回：整条改为居中小字提示（自己/对方区分文案）
    if (m.kind === "msg" && m.recalled) {
      return (
        <div key={m.id}>
          {showSep && <div className="lan-time-sep">{fmtTime(m.ts)}</div>}
          <div className="lan-recall-tip">
            {incoming ? "对方撤回了一条消息" : "你撤回了一条消息"}
          </div>
        </div>
      );
    }
    // 仅自己发出、未失败、5 分钟内的文本消息可撤回
    const canRecall =
      m.kind === "msg" && !m.incoming && !m.failed && now - m.ts <= RECALL_WINDOW;
    return (
      <div key={m.id}>
        {showSep && <div className="lan-time-sep">{fmtTime(m.ts)}</div>}
        <div className={`lan-msg${incoming ? " in" : " out"}`}>
          <div className="lan-msg-col">
            {m.kind === "msg" ? (
              <TextBubble
                text={m.text}
                failed={m.failed}
                onRetry={
                  m.failed && !m.incoming
                    ? () => resendMessage(m.fingerprint, m.id, m.text)
                    : undefined
                }
                onContextMenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  // 估算菜单尺寸并夹到视口内，避免贴边时被裁掉
                  const itemCount = 2 + (canRecall ? 1 : 0);
                  const MENU_W = 150;
                  const MENU_H = itemCount * 32 + 8;
                  setMsgMenu({
                    x: Math.min(e.clientX, window.innerWidth - MENU_W - 8),
                    y: Math.min(e.clientY, window.innerHeight - MENU_H - 8),
                    id: m.id,
                    isFile: false,
                    fingerprint: m.fingerprint,
                    text: m.text,
                    canRecall,
                  });
                }}
              />
            ) : (
              <FileBubble
                f={m}
                onCancel={cancelTransfer}
                onCancelSend={cancelSend}
                onAccept={requestConfirm}
                onContextMenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  const MENU_W = 150;
                  const MENU_H = 32 + 8; // 仅「删除」一项
                  setMsgMenu({
                    x: Math.min(e.clientX, window.innerWidth - MENU_W - 8),
                    y: Math.min(e.clientY, window.innerHeight - MENU_H - 8),
                    id: m.id,
                    isFile: true,
                  });
                }}
              />
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
        <div className="lan-title-row">
          <h2>局域网互传</h2>
          <div className="lan-presence">
            <button
              className="lan-presence-btn"
              title="在线状态"
              onClick={(e) => {
                e.stopPropagation();
                setStatusOpen((v) => !v);
              }}
            >
              <span className={`lan-presence-dot${me?.invisible ? "" : " on"}`} />
              {me?.invisible ? "隐身" : "在线"}
              <span className="lan-presence-caret">▾</span>
            </button>
            {statusOpen && (
              <div className="lan-presence-menu" onClick={(e) => e.stopPropagation()}>
                <button
                  className={`lan-presence-item${me?.invisible ? "" : " active"}`}
                  onClick={() => {
                    setInvisible(false);
                    setStatusOpen(false);
                  }}
                >
                  <span className="lan-presence-dot on" />
                  <span className="lan-presence-text">
                    在线
                    <span className="dim">其他设备可发现你</span>
                  </span>
                </button>
                <button
                  className={`lan-presence-item${me?.invisible ? " active" : ""}`}
                  onClick={() => {
                    setInvisible(true);
                    setStatusOpen(false);
                  }}
                >
                  <span className="lan-presence-dot" />
                  <span className="lan-presence-text">
                    隐身
                    <span className="dim">对方信号灯显示离线</span>
                  </span>
                </button>
              </div>
            )}
          </div>
        </div>
        <div className="lan-status-inline">
          <span className={`lan-dot${me?.running ? " on" : ""}`} />
          {me?.running ? "服务运行中" : "未运行"}
          {me?.ip && <span className="dim">· 本机 {me.ip}:{me.port}</span>}
          <button className="lan-gear" title="局域网互传设置" onClick={() => setSetOpen(true)}>
            ⚙️
          </button>
        </div>
      </div>

      {error &&
        createPortal(
          <div className="lan-toast">
            ⚠ {error}
            <button className="lan-toast-x" onClick={() => setError(null)}>×</button>
          </div>,
          document.body
        )}

      <div className="lan-body" ref={bodyRef}>
        <div className="lan-peers">
          <div className="lan-section-title">
            设备（{peers.length}）
            <span>
              <button
                className="lan-icon-btn"
                title="手动添加设备 IP"
                aria-label="手动添加设备 IP"
                onClick={() => setAddOpen(true)}
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <line x1="12" y1="5" x2="12" y2="19" />
                  <line x1="5" y1="12" x2="19" y2="12" />
                </svg>
              </button>
              <button
                className={`lan-icon-btn${refreshing ? " spinning" : ""}`}
                title="刷新设备列表"
                aria-label="刷新设备列表"
                onClick={handleRefresh}
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <polyline points="23 4 23 10 17 10" />
                  <polyline points="1 20 1 14 7 14" />
                  <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
                </svg>
              </button>
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
              className={`lan-peer${selected === p.fingerprint ? " active" : ""}${
                p.online === false ? " offline" : ""
              }`}
              onClick={() => setSelected(p.fingerprint)}
            >
              <span className="lan-peer-icon">{deviceIcon(p)}</span>
              <span className="lan-peer-info">
                <span className="lan-peer-alias">{p.alias}</span>
                <span className="lan-peer-sub dim">
                  {p.online === false ? "离线 · 仅查看记录" : `${p.ip} ${p.isBaibao ? "· 百宝箱" : "· LocalSend"}`}
                </span>
              </span>
              {unread[p.fingerprint] > 0 && <span className="lan-badge">{unread[p.fingerprint]}</span>}
              <span
                className={`lan-peer-dot${p.online === false ? "" : " on"}`}
                title={p.online === false ? "离线" : "在线"}
              />
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
                  <div className="dim">还没有消息。可发消息，或点下方回形针按钮 / 拖拽发送文件。</div>
                )}
                {thread.map(renderItem)}
              </div>

              {/* 输入区：上方工具条 + 文本框 */}
              <div className="lan-composer">
                <div className="lan-toolbar">
                  <button
                    className="lan-tool-btn"
                    title="发送文件"
                    aria-label="发送文件"
                    onClick={() => sendFiles(selectedPeer.fingerprint)}
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                    </svg>
                  </button>
                </div>
                <div className="lan-input-row">
                  <textarea
                    ref={taRef}
                    className="lan-input"
                    rows={1}
                    placeholder={`给 ${selectedPeer.alias} 发消息…（Enter 发送，Shift+Enter 换行）`}
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                        e.preventDefault();
                        send();
                      }
                    }}
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

      {/* 消息右键菜单（挂到 body，避免被聊天区域裁切） */}
      {msgMenu &&
        createPortal(
          <div
            className="tab-menu"
            style={{ left: msgMenu.x, top: msgMenu.y }}
            onClick={(e) => e.stopPropagation()}
          >
            {!msgMenu.isFile && (
              <button className="tab-menu-item" onClick={() => copyMsg(msgMenu.text ?? "")}>
                复制
              </button>
            )}
            {msgMenu.canRecall && msgMenu.fingerprint && (
              <button
                className="tab-menu-item"
                onClick={() => {
                  recallMessage(msgMenu.fingerprint!, msgMenu.id);
                  setMsgMenu(null);
                }}
              >
                撤回
              </button>
            )}
            <button
              className="tab-menu-item danger"
              onClick={() => {
                deleteItem(msgMenu.id);
                setMsgMenu(null);
              }}
            >
              删除
            </button>
          </div>,
          document.body
        )}

      {setOpen && (
        <div className="modal-overlay" onClick={() => setSetOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-head">
              <h3>局域网互传设置</h3>
              <button className="modal-close" onClick={() => setSetOpen(false)}>×</button>
            </div>
            <div className="settings-field">
              <span>设备名</span>
              <input
                className="kv-input"
                style={{ width: 200 }}
                value={aliasDraft}
                onChange={(e) => setAliasDraft(e.target.value)}
                onBlur={() => aliasDraft.trim() && aliasDraft.trim() !== me?.alias && setAlias(aliasDraft)}
                onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
              />
            </div>
            <div className="settings-field">
              <span>兼容 LocalSend（可与手机/电脑上的 LocalSend 互传）</span>
              <label className="switch">
                <input
                  type="checkbox"
                  checked={!!me?.compat}
                  onChange={(e) => setCompat(e.target.checked)}
                />
                <span className="switch-slider" />
              </label>
            </div>
            <div className="settings-field">
              <span>接收目录</span>
              <span className="settings-field-val dim" title={me?.downloadDir}>{me?.downloadDir}</span>
              <button className="btn btn-ghost btn-sm" onClick={pickDir}>更改</button>
            </div>
            <div className="modal-actions">
              <button className="btn btn-primary" onClick={() => setSetOpen(false)}>完成</button>
            </div>
          </div>
        </div>
      )}

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
