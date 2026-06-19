import {
  createContext,
  useContext,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// LAN 互传的全局状态：服务在 app 启动即常驻运行，来件提醒、未读计数等
// 都不依赖「局域网互传」这个 tab 是否打开，从而修复「切走 tab 收不到」。

export interface MyInfo {
  alias: string;
  fingerprint: string;
  port: number;
  ip: string;
  running: boolean;
  compat: boolean;
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
}
export interface FileMeta {
  id: string;
  fileName: string;
  size: number;
  fileType: string;
}
export interface Incoming {
  sessionId: string;
  alias: string;
  fingerprint: string;
  isBaibao: boolean;
  files: FileMeta[];
}
export interface ChatMsg {
  id: string;
  fingerprint: string;
  alias: string;
  text: string;
  ts: number;
  incoming: boolean;
  failed?: boolean;
}
export interface Transfer {
  key: string;
  sessionId: string;
  fileId: string;
  fileName: string;
  transferred: number;
  size: number;
  direction: "in" | "out";
  done: boolean;
  cancelled: boolean;
  speed: number; // 字节/秒
  updatedAt: number;
}
export interface Received {
  fileName: string;
  path: string;
  size: number;
  verified: boolean | null;
  ts: number;
}

interface LanCtxValue {
  me: MyInfo | null;
  peers: Peer[];
  messages: ChatMsg[];
  transfers: Transfer[];
  received: Received[];
  incoming: Incoming | null;
  unread: Record<string, number>;
  totalUnread: number;
  selected: string | null;
  error: string | null;
  setSelected: (fp: string | null) => void;
  setError: (e: string | null) => void;
  refreshPeers: () => Promise<void>;
  respond: (accept: boolean, fileIds: string[]) => Promise<void>;
  sendMessage: (fp: string, text: string) => Promise<void>;
  sendFiles: (fp: string, paths?: string[]) => Promise<void>;
  cancelTransfer: (sessionId: string) => Promise<void>;
  clearFinishedTransfers: () => void;
  setAlias: (alias: string) => Promise<void>;
  setCompat: (enabled: boolean) => Promise<void>;
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

export function LanProvider({ children }: { children: ReactNode }) {
  const [me, setMe] = useState<MyInfo | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const [received, setReceived] = useState<Received[]>([]);
  const [incoming, setIncoming] = useState<Incoming | null>(null);
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [selected, setSelectedRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;

  const refreshPeers = useCallback(async () => {
    try {
      setPeers(await invoke<Peer[]>("lan_peers"));
    } catch {
      /* ignore */
    }
  }, []);

  const setSelected = useCallback((fp: string | null) => {
    setSelectedRaw(fp);
    if (fp) setUnread((u) => (u[fp] ? { ...u, [fp]: 0 } : u));
  }, []);

  // 启动服务 + 注册全部事件监听（仅一次）
  useEffect(() => {
    let alive = true;
    const unsubs: UnlistenFn[] = [];
    const track = (u: UnlistenFn) => {
      if (alive) unsubs.push(u);
      else u();
    };

    (async () => {
      try {
        const info = await invoke<MyInfo>("lan_start");
        if (alive) setMe(info);
        await refreshPeers();
      } catch (e) {
        if (alive) setError(String(e));
      }

      track(await listen<Peer[]>("lan://peers", (e) => setPeers(e.payload)));
      track(await listen<Incoming>("lan://incoming", (e) => setIncoming(e.payload)));
      track(
        await listen<Omit<ChatMsg, "id">>("lan://message", (e) => {
          const m = e.payload;
          setMessages((list) => [...list, { ...m, id: nextId() }]);
          if (m.incoming && m.fingerprint !== selectedRef.current) {
            setUnread((u) => ({ ...u, [m.fingerprint]: (u[m.fingerprint] ?? 0) + 1 }));
          }
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
        }>("lan://progress", (e) => {
          const p = e.payload;
          const key = `${p.direction}:${p.fileId}`;
          const now = Date.now();
          setTransfers((listv) => {
            const idx = listv.findIndex((t) => t.key === key);
            const prev = idx >= 0 ? listv[idx] : null;
            const dt = prev ? (now - prev.updatedAt) / 1000 : 0;
            const speed =
              prev && dt > 0 ? (p.transferred - prev.transferred) / dt : prev?.speed ?? 0;
            const next: Transfer = {
              key,
              sessionId: p.sessionId,
              fileId: p.fileId,
              fileName: p.fileName,
              transferred: p.transferred,
              size: p.size,
              direction: p.direction,
              done: p.size > 0 && p.transferred >= p.size,
              cancelled: false,
              speed: Math.max(0, speed),
              updatedAt: now,
            };
            if (idx < 0) return [next, ...listv].slice(0, 80);
            const copy = [...listv];
            copy[idx] = next;
            return copy;
          });
        })
      );
      track(
        await listen<{ direction: "in" | "out"; sessionId: string; fileName: string }>(
          "lan://cancelled",
          (e) => {
            const c = e.payload;
            setTransfers((listv) =>
              listv.map((t) =>
                t.sessionId === c.sessionId && t.direction === c.direction
                  ? { ...t, cancelled: true, done: true }
                  : t
              )
            );
          }
        )
      );
      track(
        await listen<Omit<Received, "ts">>("lan://received", (e) => {
          setReceived((r) => [{ ...e.payload, ts: Date.now() }, ...r].slice(0, 80));
        })
      );
    })();

    return () => {
      alive = false;
      unsubs.forEach((u) => u());
    };
  }, [refreshPeers]);

  const respond = useCallback(
    async (accept: boolean, fileIds: string[]) => {
      setIncoming((cur) => {
        if (cur) {
          invoke("lan_respond", { sessionId: cur.sessionId, accept, fileIds }).catch((e) =>
            setError(String(e))
          );
        }
        return null;
      });
    },
    []
  );

  const sendMessage = useCallback(async (fp: string, text: string) => {
    const t = text.trim();
    if (!t) return;
    const id = nextId();
    setMessages((m) => [
      ...m,
      { id, fingerprint: fp, alias: "", text: t, ts: Date.now(), incoming: false },
    ]);
    try {
      await invoke("lan_send_message", { fingerprint: fp, text: t });
    } catch (e) {
      setError(String(e));
      setMessages((m) => m.map((x) => (x.id === id ? { ...x, failed: true } : x)));
    }
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

  const clearFinishedTransfers = useCallback(() => {
    setTransfers((listv) => listv.filter((t) => !t.done));
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

  const value: LanCtxValue = {
    me,
    peers,
    messages,
    transfers,
    received,
    incoming,
    unread,
    totalUnread,
    selected,
    error,
    setSelected,
    setError,
    refreshPeers,
    respond,
    sendMessage,
    sendFiles,
    cancelTransfer,
    clearFinishedTransfers,
    setAlias,
    setCompat,
    pickDir,
    addPeerByIp,
  };

  return <LanCtx.Provider value={value}>{children}</LanCtx.Provider>;
}
