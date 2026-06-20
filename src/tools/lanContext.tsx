import {
  createContext,
  useContext,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// LAN 互传的全局状态：服务在 app 启动即常驻运行。
// 收文件不立即弹窗——会话里先出现「待接收」文件气泡，点击才弹框确认。
// 文件传输以文件气泡形式归入对应对端的对话流（微信式）。

export interface MyInfo {
  alias: string;
  fingerprint: string;
  port: number;
  ip: string;
  running: boolean;
  compat: boolean;
  invisible: boolean;
  downloadDir: string;
}
export interface Peer {
  alias: string;
  fingerprint: string;
  ip: string;
  port: number;
  protocol: string;
  deviceType?: string | null;
  isBaibao: boolean;
  lastSeenMs: number;
  online?: boolean; // 合并历史已知设备后：当前是否在线（用于展示离线设备的历史记录）
  remark?: string; // 备注名（仅 UI 派生，按 fingerprint 单独持久化）
  pinned?: boolean; // 是否置顶（仅 UI 派生）
}
export interface FileMeta {
  id: string;
  fileName: string;
  size: number;
  fileType: string;
  order?: number; // 同一次发送内的顺序号
  batch?: string; // 同一次发送的批次标识
}
export interface Incoming {
  sessionId: string;
  alias: string;
  fingerprint: string;
  isBaibao: boolean;
  files: FileMeta[];
}
export interface TextMsg {
  kind: "msg";
  id: string;
  fingerprint: string;
  text: string;
  ts: number;
  incoming: boolean;
  failed?: boolean;
  recalled?: boolean; // 已撤回：界面改为小字「消息已撤回」提示
}
export type FileStatus = "pending" | "active" | "done" | "cancelled" | "failed";
export interface FileMsg {
  kind: "file";
  id: string; // = `${direction}:${sessionId}:${fileId}`
  sessionId: string;
  fileId: string;
  fingerprint: string;
  fileName: string;
  size: number;
  transferred: number;
  direction: "in" | "out";
  status: FileStatus;
  verified: boolean | null;
  path?: string;
  failReason?: string; // status="failed" 时的具体原因（如「对方拒绝接收」）
  speed: number;
  ts: number;
  updatedAt: number;
}
export type ChatItem = TextMsg | FileMsg;

interface LanCtxValue {
  me: MyInfo | null;
  peers: Peer[];
  items: ChatItem[];
  confirm: Incoming | null; // 当前正在确认接收的请求（点击待接收文件后弹出）
  pendingFiles: FileMsg[]; // 所有待接收文件（跨会话/发送方）
  unread: Record<string, number>;
  totalUnread: number;
  selected: string | null;
  error: string | null;
  setSelected: (fp: string | null) => void;
  setError: (e: string | null) => void;
  refreshPeers: () => Promise<void>;
  requestConfirm: (sessionId: string) => void;
  dismissConfirm: () => void;
  respond: (accept: boolean, fileIds: string[]) => Promise<void>;
  acceptPendingFiles: (files: FileMsg[]) => void;
  acceptAllPending: () => void;
  rejectAllPending: () => void;
  sendMessage: (fp: string, text: string) => Promise<void>;
  resendMessage: (fp: string, id: string, text: string) => Promise<void>;
  recallMessage: (fp: string, id: string) => Promise<void>;
  deleteItem: (id: string) => void;
  sendFiles: (fp: string, paths?: string[]) => Promise<void>;
  cancelTransfer: (sessionId: string) => Promise<void>;
  cancelSend: (fileId: string) => void;
  setRemark: (fp: string, remark: string) => void; // 设置/清除设备备注（空字符串=清除）
  clearChat: (fp: string) => void; // 清空与某设备的全部聊天/传输记录
  togglePin: (fp: string) => void; // 置顶/取消置顶
  reorderPins: (fromFp: string, toFp: string) => void; // 拖拽调整置顶设备顺序
  setAlias: (alias: string) => Promise<void>;
  setCompat: (enabled: boolean) => Promise<void>;
  setInvisible: (enabled: boolean) => Promise<void>;
  pickDir: () => Promise<void>;
  addPeerByIp: (ip: string, port?: number) => Promise<void>;
}

const LanCtx = createContext<LanCtxValue | null>(null);

export function useLan(): LanCtxValue {
  const ctx = useContext(LanCtx);
  if (!ctx) throw new Error("useLan 必须在 <LanProvider> 内使用");
  return ctx;
}

let idSeq = 0;
const nextId = () => `${Date.now()}-${idSeq++}`;
const fileKey = (dir: string, sid: string, fid: string) => `${dir}:${sid}:${fid}`;

// ── 历史记录持久化（按设备唯一 ID = fingerprint 关联，改名也不丢）──
const LS_ITEMS = "baibao.lan.items.v1";
const LS_PEERS = "baibao.lan.knownpeers.v1";
const LS_REMARKS = "baibao.lan.remarks.v1"; // fingerprint -> 备注名
const LS_PINS = "baibao.lan.pins.v1"; // 置顶设备的有序 fingerprint 列表
const HISTORY_CAP = 3000; // 最多保留多少条记录，避免无限增长

function loadRemarks(): Record<string, string> {
  try {
    const raw = localStorage.getItem(LS_REMARKS);
    return raw ? (JSON.parse(raw) as Record<string, string>) : {};
  } catch {
    return {};
  }
}
function loadPins(): string[] {
  try {
    const raw = localStorage.getItem(LS_PINS);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}

function loadItems(): ChatItem[] {
  try {
    const raw = localStorage.getItem(LS_ITEMS);
    if (!raw) return [];
    const arr = JSON.parse(raw) as ChatItem[];
    // 重启后无法续传：把未完成的传输状态归一为「已取消」，避免卡在「传输中/待接收」
    return arr.map((x) =>
      x.kind === "file" && (x.status === "active" || x.status === "pending")
        ? { ...x, status: "cancelled" as FileStatus, speed: 0 }
        : x
    );
  } catch {
    return [];
  }
}

function loadKnownPeers(): Record<string, Peer> {
  try {
    const raw = localStorage.getItem(LS_PEERS);
    return raw ? (JSON.parse(raw) as Record<string, Peer>) : {};
  } catch {
    return {};
  }
}

export function LanProvider({ children }: { children: ReactNode }) {
  const [me, setMe] = useState<MyInfo | null>(null);
  const [livePeers, setLivePeers] = useState<Peer[]>([]); // 当前在线发现到的设备
  const [knownPeers, setKnownPeers] = useState<Record<string, Peer>>(loadKnownPeers); // 历史已知设备（持久化）
  const [items, setItems] = useState<ChatItem[]>(loadItems);
  const [remarks, setRemarks] = useState<Record<string, string>>(loadRemarks); // 备注（持久化）
  const [pins, setPins] = useState<string[]>(loadPins); // 置顶设备有序列表（持久化）
  const [confirm, setConfirm] = useState<Incoming | null>(null);
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [selected, setSelectedRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;
  const offersRef = useRef<Record<string, Incoming>>({}); // sessionId -> 待接收请求
  const batchBaseRef = useRef<Record<string, number>>({}); // batch -> 基准时间戳，配合 order 保证接收顺序=发送顺序

  // 有聊天/传输记录的设备指纹集合（用于判断「离线且无记录」的设备可丢弃）
  const historyFps = useMemo(() => new Set(items.map((x) => x.fingerprint)), [items]);

  // 设备列表 = 在线发现 + 历史已知（离线）。按 fingerprint 去重，online 标记是否在线。
  // 排序：置顶设备在最前（按 pins 自定义顺序），其余按「在线优先 + 最近活跃」。
  // 备注/置顶按 fingerprint 单独维护，附加到派生出的 Peer 上（不写回 knownPeers）。
  // 离线 + 无聊天记录 + 未置顶的设备不展示（视为已删除）。
  const peers = useMemo<Peer[]>(() => {
    const map = new Map<string, Peer>();
    for (const fp of Object.keys(knownPeers)) map.set(fp, { ...knownPeers[fp], online: false });
    for (const p of livePeers) map.set(p.fingerprint, { ...p, online: true });
    const pinIndex = new Map(pins.map((fp, i) => [fp, i]));
    const arr = [...map.values()]
      .map((p) => ({
        ...p,
        remark: remarks[p.fingerprint] || undefined,
        pinned: pinIndex.has(p.fingerprint),
      }))
      .filter(
        (p) => p.online !== false || p.pinned || historyFps.has(p.fingerprint)
      );
    return arr.sort((a, b) => {
      const ai = pinIndex.get(a.fingerprint);
      const bi = pinIndex.get(b.fingerprint);
      if (ai !== undefined && bi !== undefined) return ai - bi; // 都置顶 → 按自定义顺序
      if ((ai !== undefined) !== (bi !== undefined)) return ai !== undefined ? -1 : 1; // 置顶在前
      // 未置顶：在线优先，然后按「名称 → fingerprint」稳定排序。
      // 不再用 lastSeenMs 作次序——它每次心跳都变，会导致设备顺序来回跳。
      return (
        Number(!!b.online) - Number(!!a.online) ||
        (a.alias || "").localeCompare(b.alias || "") ||
        a.fingerprint.localeCompare(b.fingerprint)
      );
    });
  }, [livePeers, knownPeers, remarks, pins, historyFps]);

  // 维护历史已知表：在线设备刷新元数据；离线且「无聊天记录、未置顶」的设备删除，避免常驻堆积。
  // 在线集合与被删集合不相交（删的都不在 livePeers 里），不会产生「加了又删」的抖动。
  useEffect(() => {
    setKnownPeers((kp) => {
      const liveFps = new Set(livePeers.map((p) => p.fingerprint));
      const pinSet = new Set(pins);
      let changed = false;
      const next = { ...kp };
      for (const p of livePeers) {
        const rec: Peer = { ...p };
        if (JSON.stringify(next[p.fingerprint]) !== JSON.stringify(rec)) {
          next[p.fingerprint] = rec;
          changed = true;
        }
      }
      for (const fp of Object.keys(next)) {
        if (!liveFps.has(fp) && !historyFps.has(fp) && !pinSet.has(fp)) {
          delete next[fp];
          changed = true;
        }
      }
      return changed ? next : kp;
    });
  }, [livePeers, historyFps, pins]);

  // 持久化：历史已知设备 + 聊天/传输记录（防抖、裁剪到上限）
  useEffect(() => {
    try {
      localStorage.setItem(LS_PEERS, JSON.stringify(knownPeers));
    } catch {
      /* ignore */
    }
  }, [knownPeers]);
  useEffect(() => {
    try {
      localStorage.setItem(LS_REMARKS, JSON.stringify(remarks));
    } catch {
      /* ignore */
    }
  }, [remarks]);
  useEffect(() => {
    try {
      localStorage.setItem(LS_PINS, JSON.stringify(pins));
    } catch {
      /* ignore */
    }
  }, [pins]);
  useEffect(() => {
    const t = window.setTimeout(() => {
      try {
        const trimmed =
          items.length > HISTORY_CAP ? items.slice(items.length - HISTORY_CAP) : items;
        localStorage.setItem(LS_ITEMS, JSON.stringify(trimmed));
      } catch {
        /* ignore */
      }
    }, 500);
    return () => window.clearTimeout(t);
  }, [items]);

  const bumpUnread = useCallback((fp: string) => {
    if (fp && fp !== selectedRef.current) {
      setUnread((u) => ({ ...u, [fp]: (u[fp] ?? 0) + 1 }));
    }
  }, []);

  const upsertFile = useCallback((patch: Partial<FileMsg> & { id: string }) => {
    setItems((list) => {
      const idx = list.findIndex((x) => x.kind === "file" && x.id === patch.id);
      if (idx < 0) {
        const f: FileMsg = {
          kind: "file",
          id: patch.id,
          sessionId: patch.sessionId ?? "",
          fileId: patch.fileId ?? "",
          fingerprint: patch.fingerprint ?? "",
          fileName: patch.fileName ?? "文件",
          size: patch.size ?? 0,
          transferred: patch.transferred ?? 0,
          direction: patch.direction ?? "in",
          status: patch.status ?? "active",
          verified: patch.verified ?? null,
          path: patch.path,
          speed: patch.speed ?? 0,
          ts: patch.ts ?? Date.now(), // 允许调用方指定 ts，用于保证展示顺序
          updatedAt: Date.now(),
        };
        return [...list, f];
      }
      const copy = [...list];
      // 已存在的不覆盖 ts（保持原有顺序），只更新其余字段
      const { ts: _ignoredTs, ...rest } = patch;
      copy[idx] = { ...(copy[idx] as FileMsg), ...rest, updatedAt: Date.now() };
      return copy;
    });
  }, []);

  const refreshPeers = useCallback(async () => {
    try {
      setLivePeers(await invoke<Peer[]>("lan_peers"));
    } catch {
      /* ignore */
    }
  }, []);

  const setSelected = useCallback((fp: string | null) => {
    setSelectedRaw(fp);
    if (fp) setUnread((u) => (u[fp] ? { ...u, [fp]: 0 } : u));
  }, []);

  useEffect(() => {
    let alive = true;
    const unsubs: UnlistenFn[] = [];
    const track = (u: UnlistenFn) => (alive ? unsubs.push(u) : u());

    (async () => {
      try {
        const info = await invoke<MyInfo>("lan_start");
        if (alive) setMe(info);
        await refreshPeers();
      } catch (e) {
        if (alive) setError(String(e));
      }

      track(await listen<Peer[]>("lan://peers", (e) => setLivePeers(e.payload)));
      track(
        await listen<Incoming>("lan://incoming", (e) => {
          // 不立即弹窗：登记请求 + 在对话里放「待接收」文件气泡
          const inc = e.payload;
          offersRef.current = { ...offersRef.current, [inc.sessionId]: inc };
          for (const f of inc.files) {
            // 并发发送时各文件到达顺序是乱的；用「批次基准时间 + 顺序号」给气泡定 ts，
            // 保证接收端展示顺序 = 发送顺序（同批共用基准，跨批/消息仍按到达时间）。
            let ts = Date.now();
            if (f.batch) {
              const base = batchBaseRef.current[f.batch] ?? ts;
              batchBaseRef.current[f.batch] = base;
              ts = base + (f.order ?? 0);
            }
            upsertFile({
              id: fileKey("in", inc.sessionId, f.id),
              sessionId: inc.sessionId,
              fileId: f.id,
              fingerprint: inc.fingerprint,
              fileName: f.fileName,
              size: f.size,
              direction: "in",
              status: "pending",
              ts,
            });
          }
          bumpUnread(inc.fingerprint);
        })
      );
      track(
        await listen<{ sessionId: string }>("lan://offer-timeout", (e) => {
          const sid = e.payload.sessionId;
          const inc = offersRef.current[sid];
          if (inc) {
            for (const f of inc.files) {
              upsertFile({ id: fileKey("in", sid, f.id), status: "cancelled" });
            }
            const next = { ...offersRef.current };
            delete next[sid];
            offersRef.current = next;
          }
        })
      );
      track(
        await listen<{ fingerprint: string; alias: string; text: string; id?: string | null }>("lan://message", (e) => {
          const m = e.payload;
          setItems((l) => [
            ...l,
            { kind: "msg", id: m.id || nextId(), fingerprint: m.fingerprint, text: m.text, ts: Date.now(), incoming: true },
          ]);
          bumpUnread(m.fingerprint);
        })
      );
      // 对方撤回了一条消息：按共享 msgId 把本地那条标记为已撤回
      track(
        await listen<{ fingerprint: string; msgId: string }>("lan://recall", (e) => {
          const { fingerprint, msgId } = e.payload;
          setItems((l) =>
            l.map((x) =>
              x.kind === "msg" && x.id === msgId && x.fingerprint === fingerprint
                ? { ...x, recalled: true }
                : x
            )
          );
        })
      );
      // 发送方点了发送：先在本地放一个「待发送」气泡（还没拿到会话号，按 fileId 临时归位）
      track(
        await listen<{ fileId: string; fileName: string; size: number; peer: string; order?: number }>(
          "lan://send-pending",
          (e) => {
            const o = e.payload;
            upsertFile({
              id: fileKey("out", "", o.fileId),
              sessionId: "",
              fileId: o.fileId,
              fingerprint: o.peer,
              fileName: o.fileName,
              size: o.size,
              direction: "out",
              status: "pending",
              ts: Date.now() + (o.order ?? 0), // 保证发送方一侧的气泡顺序 = 选择顺序
            });
          }
        )
      );
      // 握手被拒/失败：把「待发送」气泡标记为失败
      track(
        await listen<{ fileId: string; peer: string; reason?: string }>("lan://send-rejected", (e) => {
          const { fileId, reason } = e.payload;
          setItems((list) =>
            list.map((x) =>
              x.kind === "file" && x.direction === "out" && x.fileId === fileId
                ? { ...x, status: "failed", failReason: reason, updatedAt: Date.now() }
                : x
            )
          );
        })
      );
      // 后端确认撤销成功
      track(
        await listen<{ fileId: string; peer: string }>("lan://send-cancelled", (e) => {
          const { fileId } = e.payload;
          setItems((list) =>
            list.map((x) =>
              x.kind === "file" && x.direction === "out" && x.fileId === fileId
                ? { ...x, status: "cancelled", updatedAt: Date.now() }
                : x
            )
          );
        })
      );
      track(
        await listen<{
          sessionId: string;
          peer: string;
          files: { fileId: string; fileName: string; size: number }[];
        }>("lan://outgoing", (e) => {
          const o = e.payload;
          // 对方已接受：把之前按 fileId 临时落位的「待发送」气泡升级为「传输中」，
          // 并补上真正的会话号（id 也随之换成 out:sessionId:fileId）。
          setItems((list) => {
            const copy = [...list];
            const now = Date.now();
            for (const f of o.files) {
              const realId = fileKey("out", o.sessionId, f.fileId);
              const idx = copy.findIndex(
                (x) => x.kind === "file" && x.direction === "out" && x.fileId === f.fileId
              );
              if (idx >= 0) {
                copy[idx] = {
                  ...(copy[idx] as FileMsg),
                  id: realId,
                  sessionId: o.sessionId,
                  status: "active",
                  updatedAt: now,
                };
              } else {
                copy.push({
                  kind: "file",
                  id: realId,
                  sessionId: o.sessionId,
                  fileId: f.fileId,
                  fingerprint: o.peer,
                  fileName: f.fileName,
                  size: f.size,
                  transferred: 0,
                  direction: "out",
                  status: "active",
                  verified: null,
                  speed: 0,
                  ts: now,
                  updatedAt: now,
                });
              }
            }
            return copy;
          });
        })
      );
      track(
        await listen<{
          direction: "in" | "out";
          sessionId: string;
          fileId: string;
          fileName: string;
          transferred: number;
          size: number;
          peer: string;
        }>("lan://progress", (e) => {
          const p = e.payload;
          const id = fileKey(p.direction, p.sessionId, p.fileId);
          setItems((list) => {
            const idx = list.findIndex((x) => x.kind === "file" && x.id === id);
            const now = Date.now();
            if (idx < 0) {
              return [
                ...list,
                {
                  kind: "file", id, sessionId: p.sessionId, fileId: p.fileId, fingerprint: p.peer,
                  fileName: p.fileName, size: p.size, transferred: p.transferred, direction: p.direction,
                  status: "active", verified: null, speed: 0, ts: now, updatedAt: now,
                } as FileMsg,
              ];
            }
            const prev = list[idx] as FileMsg;
            const dt = (now - prev.updatedAt) / 1000;
            const speed = dt > 0 ? Math.max(0, (p.transferred - prev.transferred) / dt) : prev.speed;
            const copy = [...list];
            copy[idx] = { ...prev, transferred: p.transferred, size: p.size, speed, status: "active", updatedAt: now };
            return copy;
          });
        })
      );
      track(
        await listen<{ sessionId: string; fileId: string }>("lan://sent", (e) => {
          upsertFile({ id: fileKey("out", e.payload.sessionId, e.payload.fileId), status: "done" });
        })
      );
      track(
        await listen<{
          fileName: string; path: string; size: number; verified: boolean | null;
          sessionId: string; fileId: string; peer: string;
        }>("lan://received", (e) => {
          const r = e.payload;
          upsertFile({
            id: fileKey("in", r.sessionId, r.fileId),
            sessionId: r.sessionId, fileId: r.fileId, fingerprint: r.peer,
            fileName: r.fileName, size: r.size, transferred: r.size,
            direction: "in", status: "done", verified: r.verified, path: r.path,
          });
          bumpUnread(r.peer);
        })
      );
      track(
        await listen<{ direction: "in" | "out"; sessionId: string; fileId: string }>(
          "lan://cancelled",
          (e) => {
            const c = e.payload;
            upsertFile({ id: fileKey(c.direction, c.sessionId, c.fileId), status: "cancelled" });
          }
        )
      );
    })();

    return () => {
      alive = false;
      unsubs.forEach((u) => u());
    };
  }, [refreshPeers, bumpUnread, upsertFile]);

  // 点击「待接收」文件 → 打开确认框（每个会话即单文件）
  const requestConfirm = useCallback((sessionId: string) => {
    const inc = offersRef.current[sessionId];
    if (inc) setConfirm(inc);
  }, []);

  // 仅关闭弹框，不接受也不拒绝（文件保留为待接收，可稍后再点）
  const dismissConfirm = useCallback(() => setConfirm(null), []);

  const respond = useCallback(
    async (accept: boolean, fileIds: string[]) => {
      setConfirm((cur) => {
        if (cur) {
          invoke("lan_respond", { sessionId: cur.sessionId, accept, fileIds }).catch((e) =>
            setError(String(e))
          );
          const want = new Set(fileIds);
          for (const f of cur.files) {
            const id = fileKey("in", cur.sessionId, f.id);
            if (accept && (want.size === 0 || want.has(f.id))) {
              upsertFile({ id, status: "active" });
            } else {
              upsertFile({ id, status: "cancelled" });
            }
          }
          const next = { ...offersRef.current };
          delete next[cur.sessionId];
          offersRef.current = next;
        }
        return null;
      });
    },
    [upsertFile]
  );

  // 接受指定的一批待接收文件（「全部待接收」勾选后接受所选）。
  // 按会话分组逐一应答；同会话内未勾选的文件按协议视为拒绝（无法稍后再单独接收）。
  const acceptPendingFiles = useCallback(
    (files: FileMsg[]) => {
      const bySession = new Map<string, Set<string>>();
      for (const f of files) {
        const set = bySession.get(f.sessionId) ?? new Set<string>();
        set.add(f.fileId);
        bySession.set(f.sessionId, set);
      }
      for (const [sid, fileIds] of bySession) {
        invoke("lan_respond", {
          sessionId: sid,
          accept: true,
          fileIds: [...fileIds],
        }).catch((e) => setError(String(e)));
        const offer = offersRef.current[sid];
        if (offer) {
          for (const f of offer.files) {
            upsertFile({
              id: fileKey("in", sid, f.id),
              status: fileIds.has(f.id) ? "active" : "cancelled",
            });
          }
          const next = { ...offersRef.current };
          delete next[sid];
          offersRef.current = next;
        }
      }
      setConfirm(null);
    },
    [upsertFile]
  );

  // 一次接受所有待接收会话的全部文件
  const acceptAllPending = useCallback(() => {
    const offers = offersRef.current;
    for (const sid of Object.keys(offers)) {
      const inc = offers[sid];
      invoke("lan_respond", {
        sessionId: sid,
        accept: true,
        fileIds: inc.files.map((f) => f.id),
      }).catch((e) => setError(String(e)));
      for (const f of inc.files) {
        upsertFile({ id: fileKey("in", sid, f.id), status: "active" });
      }
    }
    offersRef.current = {};
    setConfirm(null);
  }, [upsertFile]);

  // 一次拒绝所有待接收会话
  const rejectAllPending = useCallback(() => {
    const offers = offersRef.current;
    for (const sid of Object.keys(offers)) {
      const inc = offers[sid];
      invoke("lan_respond", { sessionId: sid, accept: false, fileIds: [] }).catch((e) =>
        setError(String(e))
      );
      for (const f of inc.files) {
        upsertFile({ id: fileKey("in", sid, f.id), status: "cancelled" });
      }
    }
    offersRef.current = {};
    setConfirm(null);
  }, [upsertFile]);

  // 发送一条文本：id 不存在则新建气泡，已存在（重发）则清掉失败标记后重试。
  // 发送失败不弹错误条，只把该条标记为 failed —— 由气泡前方的「重试」按钮处理。
  const doSendText = useCallback(async (fp: string, text: string, id: string) => {
    // 新消息才插入；重发时保留原条目（仍为 failed，三角的显隐交给气泡的「重试中」态控制）
    setItems((l) =>
      l.some((x) => x.id === id)
        ? l
        : [...l, { kind: "msg", id, fingerprint: fp, text, ts: Date.now(), incoming: false }]
    );
    try {
      await invoke("lan_send_message", { fingerprint: fp, text, msgId: id });
      setItems((l) => l.map((x) => (x.kind === "msg" && x.id === id ? { ...x, failed: false } : x)));
    } catch {
      setItems((l) => l.map((x) => (x.kind === "msg" && x.id === id ? { ...x, failed: true } : x)));
    }
  }, []);

  const sendMessage = useCallback(
    async (fp: string, text: string) => {
      const t = text.trim();
      if (!t) return;
      await doSendText(fp, t, nextId());
    },
    [doSendText]
  );

  // 重新发送一条失败的消息（沿用原 id/文本）
  const resendMessage = useCallback(
    (fp: string, id: string, text: string) => doSendText(fp, text, id),
    [doSendText]
  );

  // 撤回自己发出的一条文本消息（仅 5 分钟内允许，时限由 UI 控制）：
  // 先通知对端，成功后本地也标记为已撤回，保证两端一致。
  const recallMessage = useCallback(async (fp: string, id: string) => {
    try {
      await invoke("lan_recall_message", { fingerprint: fp, msgId: id });
      setItems((l) => l.map((x) => (x.kind === "msg" && x.id === id ? { ...x, recalled: true } : x)));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // 本地删除一条消息/记录（仅删除本端，不通知对方）
  const deleteItem = useCallback((id: string) => {
    setItems((l) => l.filter((x) => x.id !== id));
  }, []);

  const sendFiles = useCallback(async (fp: string, paths?: string[]) => {
    setError(null);
    try {
      const list = paths ?? (await invoke<string[]>("lan_pick_files"));
      if (!list.length) return;
      await invoke("lan_send_files", { fingerprint: fp, paths: list });
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const cancelTransfer = useCallback(async (sessionId: string) => {
    try {
      await invoke("lan_cancel", { sessionId });
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // 撤销一个还没被对方接受的发送（「待发送」气泡）：本地立即标记已取消，并通知后端跳过上传
  const cancelSend = useCallback(
    (fileId: string) => {
      invoke("lan_cancel_send", { fileId }).catch((e) => setError(String(e)));
      setItems((list) =>
        list.map((x) =>
          x.kind === "file" && x.direction === "out" && x.fileId === fileId && x.status === "pending"
            ? { ...x, status: "cancelled", updatedAt: Date.now() }
            : x
        )
      );
    },
    []
  );

  // 设备备注：空字符串=清除，恢复显示对方原始名称
  const setRemark = useCallback((fp: string, remark: string) => {
    const r = remark.trim();
    setRemarks((m) => {
      if (r === (m[fp] ?? "")) return m;
      const next = { ...m };
      if (r) next[fp] = r;
      else delete next[fp];
      return next;
    });
  }, []);

  // 清空与某设备的全部聊天/传输记录
  const clearChat = useCallback((fp: string) => {
    setItems((l) => l.filter((x) => x.fingerprint !== fp));
    setUnread((u) => (u[fp] ? { ...u, [fp]: 0 } : u));
  }, []);

  // 置顶/取消置顶：新置顶的追加到置顶区末尾
  const togglePin = useCallback((fp: string) => {
    setPins((ps) => (ps.includes(fp) ? ps.filter((x) => x !== fp) : [...ps, fp]));
  }, []);

  // 拖拽调整置顶设备顺序：把 fromFp 移动到 toFp 所在位置
  const reorderPins = useCallback((fromFp: string, toFp: string) => {
    setPins((ps) => {
      const from = ps.indexOf(fromFp);
      const to = ps.indexOf(toFp);
      if (from < 0 || to < 0 || from === to) return ps;
      const next = [...ps];
      next.splice(from, 1);
      next.splice(to, 0, fromFp);
      return next;
    });
  }, []);

  const setAlias = useCallback(async (alias: string) => {
    const a = alias.trim();
    if (!a) return;
    await invoke("lan_set_alias", { alias: a });
    setMe((cur) => (cur ? { ...cur, alias: a } : cur));
  }, []);

  const setCompat = useCallback(async (enabled: boolean) => {
    await invoke("lan_set_compat", { enabled });
    setMe((cur) => (cur ? { ...cur, compat: enabled } : cur));
  }, []);

  const setInvisible = useCallback(async (enabled: boolean) => {
    await invoke("lan_set_invisible", { enabled });
    setMe((cur) => (cur ? { ...cur, invisible: enabled } : cur));
  }, []);

  const pickDir = useCallback(async () => {
    const dir = await invoke<string | null>("lan_pick_dir");
    if (dir) {
      await invoke("lan_set_dir", { dir });
      setMe((cur) => (cur ? { ...cur, downloadDir: dir } : cur));
    }
  }, []);

  const addPeerByIp = useCallback(
    async (ip: string, port?: number) => {
      setError(null);
      try {
        await invoke("lan_add_peer", { ip: ip.trim(), port: port ?? null });
        await refreshPeers();
      } catch (e) {
        setError(String(e));
        throw e;
      }
    },
    [refreshPeers]
  );

  const totalUnread = Object.values(unread).reduce((a, b) => a + b, 0);
  const pendingFiles = items.filter(
    (x) => x.kind === "file" && x.direction === "in" && x.status === "pending"
  ) as FileMsg[];

  const value: LanCtxValue = {
    me, peers, items, confirm, pendingFiles, unread, totalUnread, selected, error,
    setSelected, setError, refreshPeers, requestConfirm, dismissConfirm, respond,
    acceptPendingFiles, acceptAllPending, rejectAllPending,
    sendMessage, resendMessage, recallMessage, deleteItem, sendFiles, cancelTransfer, cancelSend,
    setRemark, clearChat, togglePin, reorderPins,
    setAlias, setCompat, setInvisible, pickDir, addPeerByIp,
  };

  return <LanCtx.Provider value={value}>{children}</LanCtx.Provider>;
}
