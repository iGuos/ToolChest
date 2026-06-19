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
export interface TextMsg {
  kind: "msg";
  id: string;
  fingerprint: string;
  text: string;
  ts: number;
  incoming: boolean;
  failed?: boolean;
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
  respond: (accept: boolean, fileIds: string[]) => Promise<void>;
  acceptAllPending: () => void;
  sendMessage: (fp: string, text: string) => Promise<void>;
  sendFiles: (fp: string, paths?: string[]) => Promise<void>;
  cancelTransfer: (sessionId: string) => Promise<void>;
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
const fileKey = (dir: string, sid: string, fid: string) => `${dir}:${sid}:${fid}`;

export function LanProvider({ children }: { children: ReactNode }) {
  const [me, setMe] = useState<MyInfo | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [items, setItems] = useState<ChatItem[]>([]);
  const [confirm, setConfirm] = useState<Incoming | null>(null);
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [selected, setSelectedRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;
  const offersRef = useRef<Record<string, Incoming>>({}); // sessionId -> 待接收请求

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
          ts: Date.now(),
          updatedAt: Date.now(),
        };
        return [...list, f];
      }
      const copy = [...list];
      copy[idx] = { ...(copy[idx] as FileMsg), ...patch, updatedAt: Date.now() };
      return copy;
    });
  }, []);

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

      track(await listen<Peer[]>("lan://peers", (e) => setPeers(e.payload)));
      track(
        await listen<Incoming>("lan://incoming", (e) => {
          // 不立即弹窗：登记请求 + 在对话里放「待接收」文件气泡
          const inc = e.payload;
          offersRef.current = { ...offersRef.current, [inc.sessionId]: inc };
          for (const f of inc.files) {
            upsertFile({
              id: fileKey("in", inc.sessionId, f.id),
              sessionId: inc.sessionId,
              fileId: f.id,
              fingerprint: inc.fingerprint,
              fileName: f.fileName,
              size: f.size,
              direction: "in",
              status: "pending",
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
        await listen<{ fingerprint: string; alias: string; text: string }>("lan://message", (e) => {
          const m = e.payload;
          setItems((l) => [
            ...l,
            { kind: "msg", id: nextId(), fingerprint: m.fingerprint, text: m.text, ts: Date.now(), incoming: true },
          ]);
          bumpUnread(m.fingerprint);
        })
      );
      track(
        await listen<{
          sessionId: string;
          peer: string;
          files: { fileId: string; fileName: string; size: number }[];
        }>("lan://outgoing", (e) => {
          const o = e.payload;
          for (const f of o.files) {
            upsertFile({
              id: fileKey("out", o.sessionId, f.fileId),
              sessionId: o.sessionId,
              fileId: f.fileId,
              fingerprint: o.peer,
              fileName: f.fileName,
              size: f.size,
              direction: "out",
              status: "active",
            });
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

  // 点击「待接收」文件 → 打开确认框
  const requestConfirm = useCallback((sessionId: string) => {
    const inc = offersRef.current[sessionId];
    if (inc) setConfirm(inc);
  }, []);

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

  const sendMessage = useCallback(async (fp: string, text: string) => {
    const t = text.trim();
    if (!t) return;
    const id = nextId();
    setItems((l) => [
      ...l,
      { kind: "msg", id, fingerprint: fp, text: t, ts: Date.now(), incoming: false },
    ]);
    try {
      await invoke("lan_send_message", { fingerprint: fp, text: t });
    } catch (e) {
      setError(String(e));
      setItems((l) => l.map((x) => (x.kind === "msg" && x.id === id ? { ...x, failed: true } : x)));
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
  const pendingFiles = items.filter(
    (x) => x.kind === "file" && x.status === "pending"
  ) as FileMsg[];

  const value: LanCtxValue = {
    me, peers, items, confirm, pendingFiles, unread, totalUnread, selected, error,
    setSelected, setError, refreshPeers, requestConfirm, respond, acceptAllPending,
    sendMessage, sendFiles, cancelTransfer, setAlias, setCompat, pickDir, addPeerByIp,
  };

  return <LanCtx.Provider value={value}>{children}</LanCtx.Provider>;
}
