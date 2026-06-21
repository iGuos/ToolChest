import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { useLan, type Peer, type FileMsg, type ChatItem } from "./lanContext";
import { useEscToClose, useDragReorder } from "../hooks";
import ShareBrowser from "./ShareBrowser";

interface NetIface {
  name: string;
  ip: string;
  netmask: string;
  prefix: number;
  cidr: string;
  isVpn: boolean;
}
interface OverlayRoute {
  dest: string;
  gateway: string;
  iface: string;
}

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

// 统一的设备状态图标（不用 emoji，避免 Windows/macOS 渲染不一致）：
// 在线 → 屏幕亮蓝色；离线 → 屏幕黑色。手机类设备用手机外形，其余用电脑/显示器外形。
function DeviceIcon({ peer }: { peer: Peer }) {
  const online = peer.online !== false;
  const screen = online ? "#4c9bff" : "#2b2f36"; // 蓝屏 / 黑屏
  if ((peer.deviceType ?? "") === "mobile") {
    return (
      <svg className="lan-dev-svg" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <rect x="6.5" y="2.5" width="11" height="19" rx="2.5" stroke="currentColor" strokeWidth="1.6" />
        <rect x="8" y="4.5" width="8" height="12.6" rx="0.6" fill={screen} />
        <circle cx="12" cy="19.4" r="0.9" fill="currentColor" />
      </svg>
    );
  }
  return (
    <svg className="lan-dev-svg" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <rect x="2.5" y="3.5" width="19" height="13" rx="2" stroke="currentColor" strokeWidth="1.6" />
      <rect x="4.3" y="5.3" width="15.4" height="9.4" rx="0.6" fill={screen} />
      <path d="M9 19.5h6M12 16.5v3" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
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
const PAGE_SIZE = 20; // 聊天记录每页条数：默认显示最近 20 条，向上滚动再加载 20 条

export default function LanShare() {
  const {
    me, peers, items, unread, selected, error,
    setSelected, setError, refreshPeers, sendMessage, resendMessage, recallMessage, deleteItem, sendFiles,
    cancelTransfer, cancelSend, requestConfirm, addPeerByIp, setAlias, setCompat, setInvisible, pickDir,
    setRemark, clearChat, togglePin, reorderPins,
  } = useLan();

  const [draft, setDraft] = useState("");
  const [dragOver, setDragOver] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [addIp, setAddIp] = useState("");
  const [adding, setAdding] = useState(false);
  const [setOpen, setSetOpen] = useState(false);
  useEscToClose(addOpen, () => setAddOpen(false));
  useEscToClose(setOpen, () => setSetOpen(false));
  const [aliasDraft, setAliasDraft] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [ifaces, setIfaces] = useState<NetIface[]>([]); // 当前所有网段（设置面板里展示）
  const [routes, setRoutes] = useState<OverlayRoute[]>([]); // 经隧道可达的组网地址（补充展示）
  const [statusOpen, setStatusOpen] = useState(false); // 在线/隐身下拉
  // 拉取本机网段 + 组网可达地址（诊断「VPN 多网段」「mesh 组网 IP 在哪条隧道」）
  const loadNet = () => {
    invoke<NetIface[]>("lan_interfaces").then(setIfaces).catch(() => setIfaces([]));
    invoke<OverlayRoute[]>("lan_overlay_routes").then(setRoutes).catch(() => setRoutes([]));
  };
  // 打开设置面板时拉一次
  useEffect(() => {
    if (!setOpen) return;
    loadNet();
  }, [setOpen]);
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

  // 设备右键菜单（备注 / 清空记录 / 置顶）
  const [peerMenu, setPeerMenu] = useState<{ x: number; y: number; fp: string } | null>(null);
  useEffect(() => {
    if (!peerMenu) return;
    const close = () => setPeerMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setPeerMenu(null);
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
  }, [peerMenu]);

  // 查看对方共享文件的浏览器弹框
  const [shareView, setShareView] = useState<{ fp: string; name: string } | null>(null);

  // 我自己的共享目录（设置面板内管理）
  interface ShareView { id: string; name: string; path: string; locked: boolean; password?: string | null }
  const [myShares, setMyShares] = useState<ShareView[]>([]);
  useEffect(() => {
    if (!setOpen) return;
    invoke<ShareView[]>("lan_list_shares").then(setMyShares).catch(() => setMyShares([]));
  }, [setOpen]);
  const addShare = async () => {
    try {
      const dir = await invoke<string | null>("lan_pick_dir");
      if (!dir) return;
      setMyShares(await invoke<ShareView[]>("lan_add_share", { path: dir, name: null, password: null }));
    } catch (e) {
      setError(String(e));
    }
  };
  const removeShare = async (id: string) => {
    try {
      setMyShares(await invoke<ShareView[]>("lan_remove_share", { id }));
    } catch (e) {
      setError(String(e));
    }
  };
  const setSharePassword = async (id: string, password: string | null) => {
    try {
      setMyShares(await invoke<ShareView[]>("lan_set_share_password", { id, password }));
    } catch (e) {
      setError(String(e));
    }
  };
  // 共享密码编辑弹框（替代不可用的 window.prompt）
  const [pwEdit, setPwEdit] = useState<{ id: string; name: string; value: string } | null>(null);
  useEscToClose(!!pwEdit, () => setPwEdit(null));
  const genPassword = () => {
    // 21 位、大小写字母+数字（去掉易混字符），区分大小写
    const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    return Array.from(crypto.getRandomValues(new Uint8Array(21)), (b) => alphabet[b % alphabet.length]).join("");
  };
  const savePw = async () => {
    if (!pwEdit) return;
    await setSharePassword(pwEdit.id, pwEdit.value.trim() || null);
    setPwEdit(null);
  };

  // 修改备注弹框
  const [remarkEdit, setRemarkEdit] = useState<{ fp: string; value: string } | null>(null);
  useEscToClose(!!remarkEdit, () => setRemarkEdit(null));
  const saveRemark = () => {
    if (remarkEdit) setRemark(remarkEdit.fp, remarkEdit.value);
    setRemarkEdit(null);
  };

  // 置顶设备拖拽排序：复用通用拖拽 hook。仅置顶设备可拖、且只能放到另一台置顶设备上。
  const isPinned = (fp: string) => !!peers.find((p) => p.fingerprint === fp)?.pinned;
  const peerDnd = useDragReorder({
    selector: "[data-peer-fp]",
    dataAttr: "peerFp",
    canDrag: isPinned,
    canDropOn: isPinned,
    onReorder: reorderPins,
  });

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

  // 分页：只渲染最近 visibleCount 条；向上滚到顶再加载 PAGE_SIZE 条
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);
  useEffect(() => {
    setVisibleCount(PAGE_SIZE); // 切换设备时重置到最近一页
  }, [selected]);
  const visibleThread = useMemo(
    () => thread.slice(Math.max(0, thread.length - visibleCount)),
    [thread, visibleCount]
  );
  const hasMore = thread.length > visibleCount;
  // 加载更早记录时，保持当前阅读位置不跳动
  const prependRef = useRef<{ pending: boolean; prevHeight: number }>({
    pending: false,
    prevHeight: 0,
  });
  const onChatScroll = () => {
    const el = chatRef.current;
    if (!el || !hasMore) return;
    if (el.scrollTop < 40) {
      prependRef.current = { pending: true, prevHeight: el.scrollHeight };
      setVisibleCount((c) => Math.min(thread.length, c + PAGE_SIZE));
    }
  };
  useLayoutEffect(() => {
    const el = chatRef.current;
    if (el && prependRef.current.pending) {
      el.scrollTop = el.scrollHeight - prependRef.current.prevHeight;
      prependRef.current.pending = false;
    }
  }, [visibleCount]);

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
              data-peer-fp={p.fingerprint}
              className={`lan-peer${selected === p.fingerprint ? " active" : ""}${
                p.online === false ? " offline" : ""
              }${p.pinned ? " pinned" : ""}${peerDnd.drag?.id === p.fingerprint ? " dragging" : ""}`}
              onClick={() => {
                if (peerDnd.suppressNextClick.current) {
                  peerDnd.suppressNextClick.current = false;
                  return;
                }
                setSelected(p.fingerprint);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                setPeerMenu({ x: e.clientX, y: e.clientY, fp: p.fingerprint });
              }}
              onPointerDown={(e) => peerDnd.onPointerDown(e, p.fingerprint)}
              onPointerMove={peerDnd.onPointerMove}
              onPointerUp={peerDnd.onPointerEnd}
              onPointerCancel={peerDnd.onPointerEnd}
              title={p.pinned ? "拖拽可调整置顶顺序" : undefined}
            >
              {p.pinned && <span className="lan-peer-corner" title="已置顶" />}
              <span className="lan-peer-icon"><DeviceIcon peer={p} /></span>
              <span className="lan-peer-info">
                <span className="lan-peer-alias">
                  {p.remark || p.alias}
                  {p.online !== false && (p.shares ?? 0) > 0 && (
                    <span className="lan-share-tag" title="有共享目录">共享</span>
                  )}
                </span>
                <span className="lan-peer-sub dim">
                  {p.remark
                    ? p.alias
                    : p.online === false
                    ? "离线 · 仅查看记录"
                    : `${p.ip} ${p.isBaibao ? "· 百宝箱" : "· LocalSend"}`}
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
                {selectedPeer.pinned && <span className="lan-peer-pin" title="已置顶">📌</span>}
                <b>{selectedPeer.remark || selectedPeer.alias}</b>
                <span className="dim">{selectedPeer.remark ? selectedPeer.alias : selectedPeer.ip}</span>
              </div>

              <div className="lan-chat" ref={chatRef} onScroll={onChatScroll}>
                {thread.length === 0 && (
                  <div className="dim">还没有消息。可发消息，或点下方回形针按钮 / 拖拽发送文件。</div>
                )}
                {hasMore && <div className="lan-loadmore dim">↑ 上滑加载更早的消息</div>}
                {visibleThread.map(renderItem)}
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
                    placeholder={`给 ${selectedPeer.remark || selectedPeer.alias} 发消息…（Enter 发送，Shift+Enter 换行）`}
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
              松手发送给 <b>{selectedPeer?.remark || selectedPeer?.alias}</b>
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

      {/* 设备右键菜单：修改备注 / 清空聊天记录 / 置顶 */}
      {peerMenu &&
        createPortal(
          (() => {
            const p = peers.find((x) => x.fingerprint === peerMenu.fp);
            return (
              <div
                className="tab-menu"
                style={{ left: peerMenu.x, top: peerMenu.y }}
                onClick={(e) => e.stopPropagation()}
              >
                <button
                  className="tab-menu-item"
                  onClick={() => {
                    setRemarkEdit({ fp: peerMenu.fp, value: p?.remark ?? "" });
                    setPeerMenu(null);
                  }}
                >
                  修改备注
                </button>
                {p?.online !== false && (
                  <button
                    className="tab-menu-item"
                    onClick={() => {
                      setShareView({ fp: peerMenu.fp, name: p?.remark || p?.alias || "设备" });
                      setPeerMenu(null);
                    }}
                  >
                    查看共享文件
                  </button>
                )}
                <button
                  className="tab-menu-item"
                  onClick={() => {
                    togglePin(peerMenu.fp);
                    setPeerMenu(null);
                  }}
                >
                  {p?.pinned ? "取消置顶" : "置顶"}
                </button>
                <button
                  className="tab-menu-item danger"
                  onClick={() => {
                    clearChat(peerMenu.fp);
                    setPeerMenu(null);
                  }}
                >
                  清空聊天记录
                </button>
              </div>
            );
          })(),
          document.body
        )}

      {/* 置顶设备拖拽时跟随鼠标的浮层 */}
      {peerDnd.drag &&
        createPortal(
          (() => {
            const p = peers.find((x) => x.fingerprint === peerDnd.drag!.id);
            return p ? (
              <div
                className="lan-peer-ghost"
                style={{ left: peerDnd.drag.x + 12, top: peerDnd.drag.y + 8 }}
              >
                <span className="lan-peer-corner" />
                <span className="lan-peer-icon"><DeviceIcon peer={p} /></span>
                <span className="lan-peer-alias">{p.remark || p.alias}</span>
              </div>
            ) : null;
          })(),
          document.body
        )}

      {/* 共享密码编辑弹框 */}
      {pwEdit &&
        createPortal(
          <div className="modal-overlay">
            <div className="modal" style={{ width: "min(400px, 90%)" }}>
              <div className="modal-head">
                <h3>「{pwEdit.name}」访问密码</h3>
                <button className="modal-close" onClick={() => setPwEdit(null)}>×</button>
              </div>
              <div className="dim" style={{ fontSize: 12 }}>
                其他设备访问此目录时需要输入。留空则取消密码（任何人可访问）。
              </div>
              <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
                <input
                  className="kv-input"
                  style={{ flex: 1 }}
                  value={pwEdit.value}
                  autoFocus
                  placeholder="访问密码"
                  onChange={(e) => setPwEdit((p) => (p ? { ...p, value: e.target.value } : p))}
                  onKeyDown={(e) => e.key === "Enter" && savePw()}
                />
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() => setPwEdit((p) => (p ? { ...p, value: genPassword() } : p))}
                >
                  随机生成
                </button>
              </div>
              <div className="modal-actions">
                <button className="btn btn-ghost" onClick={() => setPwEdit(null)}>取消</button>
                <button className="btn btn-primary" onClick={savePw}>保存</button>
              </div>
            </div>
          </div>,
          document.body
        )}

      {/* 查看对方共享文件 */}
      {shareView && (
        <ShareBrowser
          fingerprint={shareView.fp}
          peerName={shareView.name}
          onClose={() => setShareView(null)}
        />
      )}

      {/* 修改备注弹框 */}
      {remarkEdit &&
        createPortal(
          <div className="modal-overlay">
            <div className="modal" style={{ width: "min(380px, 90%)" }}>
              <div className="modal-head">
                <h3>修改备注</h3>
                <button className="modal-close" onClick={() => setRemarkEdit(null)}>×</button>
              </div>
              <div className="dim" style={{ fontSize: 12 }}>
                设置该设备的备注名（留空则恢复显示对方原始名称）。
              </div>
              <input
                className="kv-input"
                style={{ width: "100%", marginTop: 10 }}
                placeholder={peers.find((x) => x.fingerprint === remarkEdit.fp)?.alias ?? "备注名"}
                value={remarkEdit.value}
                autoFocus
                onChange={(e) => setRemarkEdit((r) => (r ? { ...r, value: e.target.value } : r))}
                onKeyDown={(e) => e.key === "Enter" && saveRemark()}
              />
              <div className="modal-actions">
                <button className="btn btn-ghost" onClick={() => setRemarkEdit(null)}>取消</button>
                <button className="btn btn-primary" onClick={saveRemark}>保存</button>
              </div>
            </div>
          </div>,
          document.body
        )}

      {setOpen && (
        <div className="modal-overlay">
          <div className="modal">
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

            <div className="lan-shares">
              <div className="lan-netifaces-head">
                <span>共享目录（{myShares.length}）</span>
                <button className="btn btn-ghost btn-sm" onClick={addShare}>添加目录</button>
              </div>
              <div className="dim" style={{ fontSize: 12, marginBottom: 6 }}>
                列出的目录局域网其他设备可浏览/下载;可为每个目录单独设密码。
              </div>
              {myShares.length === 0 ? (
                <div className="dim" style={{ fontSize: 12 }}>暂无共享目录</div>
              ) : (
                <ul className="lan-share-mgr">
                  {myShares.map((s) => (
                    <li key={s.id} className="lan-share-mgr-item">
                      <span className="lan-share-mgr-name">{s.name}</span>
                      <span className="lan-share-mgr-path dim" title={s.path}>{s.path}</span>
                      {s.locked ? (
                        <span className="share-lock" title="已设密码">🔒</span>
                      ) : (
                        <span className="dim" style={{ fontSize: 11 }}>无密码</span>
                      )}
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => setPwEdit({ id: s.id, name: s.name, value: s.password ?? "" })}
                      >
                        改密码
                      </button>
                      <button className="btn btn-ghost btn-sm" onClick={() => removeShare(s.id)}>移除</button>
                    </li>
                  ))}
                </ul>
              )}
            </div>

            <div className="lan-netifaces">
              <div className="lan-netifaces-head">
                <span>当前网段（{ifaces.length}）</span>
                <button className="btn btn-ghost btn-sm" onClick={loadNet}>
                  刷新
                </button>
              </div>
              {ifaces.length === 0 ? (
                <div className="dim" style={{ fontSize: 12 }}>未检测到可用网段</div>
              ) : (
                <ul className="lan-netifaces-list">
                  {ifaces.map((nf) => (
                    <li key={`${nf.name}-${nf.ip}`} className="lan-netiface">
                      <span className="lan-netiface-cidr">{nf.cidr}</span>
                      <span className="lan-netiface-meta dim">
                        {nf.name} · {nf.ip}
                      </span>
                      {nf.isVpn && <span className="lan-netiface-tag">VPN</span>}
                    </li>
                  ))}
                </ul>
              )}
              {ifaces.filter((n) => !n.isVpn).length > 1 && (
                <div className="lan-netifaces-tip dim">
                  检测到多个真实网段：两台设备只有处于同一网段才能自动发现。若开了 VPN，可临时关闭，或用「+ IP」手动添加对方。
                </div>
              )}
              {routes.length > 0 && (
                <div className="lan-overlay">
                  <div className="lan-overlay-head">组网可达地址（经隧道路由）</div>
                  <ul className="lan-netifaces-list">
                    {routes.map((r, i) => (
                      <li key={`${r.dest}-${i}`} className="lan-netiface">
                        <span className="lan-netiface-cidr">{r.dest}</span>
                        <span className="lan-netiface-meta dim">
                          经 {r.iface} · 网关 {r.gateway}
                        </span>
                        <span className="lan-netiface-tag overlay">组网</span>
                      </li>
                    ))}
                  </ul>
                  <div className="lan-netifaces-tip dim">
                    这些地址不在本机网卡上，而是经隧道路由可达的组网/覆盖网节点。多播发现不过隧道，可用「+ IP」填这里的地址直连。
                  </div>
                </div>
              )}
            </div>
            <div className="modal-actions">
              <button className="btn btn-primary" onClick={() => setSetOpen(false)}>完成</button>
            </div>
          </div>
        </div>
      )}

      {addOpen && (
        <div className="modal-overlay">
          <div className="modal">
            <div className="modal-head">
              <h3>按 IP 添加设备</h3>
              <button className="modal-close" onClick={() => setAddOpen(false)}>×</button>
            </div>
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
