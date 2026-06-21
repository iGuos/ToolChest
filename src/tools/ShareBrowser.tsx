import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEscToClose } from "../hooks";

// 类访达/资源管理器的共享文件管理器：进目录浏览 + 下载（文件夹自动 zip→解压）+
// 按对方授予的权限做新增（上传/新建文件夹，支持把本地文件拖进窗口上传）/修改（重命名）/删除。
// 加锁的共享首次访问弹密码框，认证通过后把 sha256(密码) 存本地，下次免输。

interface ShareRoot {
  id: string;
  name: string;
  locked: boolean;
  canCreate: boolean;
  canModify: boolean;
  canDelete: boolean;
}
interface ShareEntry {
  name: string;
  dir: boolean;
  size: number;
  mtime: number; // 修改时间 ms
  ctime: number; // 创建时间 ms
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

async function sha256Hex(s: string): Promise<string> {
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
const credKey = (fp: string, id: string) => `baibao.share.cred.${fp}.${id}`;

export default function ShareBrowser({
  fingerprint,
  peerName,
  onClose,
}: {
  fingerprint: string;
  peerName: string;
  onClose: () => void;
}) {
  useEscToClose(true, onClose);
  const [roots, setRoots] = useState<ShareRoot[] | null>(null);
  const [share, setShare] = useState<ShareRoot | null>(null);
  const [path, setPath] = useState<string[]>([]);
  const [entries, setEntries] = useState<ShareEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false); // 上传/操作进行中
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [revealPath, setRevealPath] = useState<string | null>(null); // 下载完成后可「打开目录」的路径
  const [pw, setPw] = useState("");
  const [pwOpen, setPwOpen] = useState(false);
  const [downloading, setDownloading] = useState<string | null>(null);
  // 名称输入弹框（新建文件夹 / 重命名）与删除确认（按名称，支持批量）
  const [namePrompt, setNamePrompt] = useState<{ kind: "mkdir" | "rename"; target?: string; value: string } | null>(null);
  const [delConfirm, setDelConfirm] = useState<string[] | null>(null);
  // 选中（当前目录内的文件名集合）+ 上次点击下标（shift 连选用）
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const lastIdxRef = useRef<number>(-1);
  // 文件右键菜单
  const [entryMenu, setEntryMenu] = useState<{ x: number; y: number; entry: ShareEntry } | null>(null);
  // 上传进度：name -> {transferred,size}
  const [progress, setProgress] = useState<Record<string, { transferred: number; size: number }>>({});
  // 上传覆盖二次确认
  const [overwrite, setOverwrite] = useState<{ names: string[]; locals: string[] } | null>(null);

  const getAuth = (sh: ShareRoot) =>
    sh.locked ? localStorage.getItem(credKey(fingerprint, sh.id)) ?? undefined : undefined;

  useEffect(() => {
    setLoading(true);
    invoke<ShareRoot[]>("lan_share_roots", { fingerprint })
      .then((r) => setRoots(r))
      .catch((e) => setErr(String(e)))
      .finally(() => setLoading(false));
  }, [fingerprint]);

  const load = useCallback(
    async (sh: ShareRoot, segs: string[], authOverride?: string) => {
      setErr(null);
      setLoading(true);
      try {
        const auth = authOverride ?? getAuth(sh);
        const res = await invoke<{ entries: ShareEntry[] }>("lan_share_list", {
          fingerprint,
          id: sh.id,
          path: segs.join("/"),
          auth: auth ?? null,
        });
        setEntries(res.entries);
        setShare(sh);
        setPath(segs);
        setPwOpen(false);
        setSelected(new Set()); // 进新目录清空选中
        lastIdxRef.current = -1;
      } catch (e) {
        if (String(e) === "auth") {
          setShare(sh);
          setPath(segs);
          setPw("");
          setPwOpen(true);
        } else {
          setErr(String(e));
        }
      } finally {
        setLoading(false);
      }
    },
    [fingerprint]
  );

  const reload = useCallback(() => {
    if (share) return load(share, path);
    return Promise.resolve();
  }, [share, path, load]);

  // 拖入上传/进度需要当前 share/path/entries，但监听只注册一次 → 用 ref 取最新值
  const ctxRef = useRef({ share, path, entries });
  ctxRef.current = { share, path, entries };

  // 实际执行上传（带进度，覆盖同名）
  const doUpload = useCallback(
    async (localPaths: string[]) => {
      const { share: sh, path: segs } = ctxRef.current;
      if (!sh || !sh.canCreate || localPaths.length === 0) return;
      setBusy(true);
      setErr(null);
      setProgress({});
      setRevealPath(null);
      try {
        for (const lp of localPaths) {
          await invoke("lan_share_upload", {
            fingerprint,
            id: sh.id,
            destDir: segs.join("/"),
            localPath: lp,
            auth: getAuth(sh) ?? null,
          });
        }
        setNotice(`已上传 ${localPaths.length} 项`);
        await load(sh, segs);
      } catch (e) {
        setErr(String(e) === "auth" ? "需要密码，请重新进入该共享" : String(e));
      } finally {
        setBusy(false);
        setProgress({});
      }
    },
    [fingerprint, load]
  );

  // 上传入口：先检测同名冲突，有则二次确认覆盖，否则直接传
  const startUpload = useCallback(
    (localPaths: string[]) => {
      const { share: sh, entries: cur } = ctxRef.current;
      if (!sh || !sh.canCreate || localPaths.length === 0) return;
      const names = localPaths.map((lp) => lp.split(/[/\\]/).pop() || "file");
      const collisions = names.filter((n) => cur.some((e) => e.name === n));
      if (collisions.length) setOverwrite({ names: collisions, locals: localPaths });
      else doUpload(localPaths);
    },
    [doUpload]
  );

  // 把本地文件拖进弹窗 → 上传到当前目录（仅在有「新增」权限时）
  useEffect(() => {
    let un: (() => void) | undefined;
    let alive = true;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "drop" && ctxRef.current.share?.canCreate) {
          const files = event.payload.paths?.filter(Boolean) ?? [];
          if (files.length) startUpload(files);
        }
      })
      .then((u) => (alive ? (un = u) : u()));
    return () => {
      alive = false;
      un?.();
    };
  }, [startUpload]);

  // 上传进度事件
  useEffect(() => {
    let un: (() => void) | undefined;
    let alive = true;
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen<{ name: string; transferred: number; size: number }>("lan://share-upload", (e) => {
        const { name, transferred, size } = e.payload;
        setProgress((p) => ({ ...p, [name]: { transferred, size } }));
      }).then((u) => (alive ? (un = u) : u()))
    );
    return () => {
      alive = false;
      un?.();
    };
  }, []);

  // 关闭文件右键菜单
  useEffect(() => {
    if (!entryMenu) return;
    const close = () => setEntryMenu(null);
    const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && setEntryMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [entryMenu]);

  const submitPw = async () => {
    if (!share) return;
    const hash = await sha256Hex(pw);
    try {
      const res = await invoke<{ entries: ShareEntry[] }>("lan_share_list", {
        fingerprint,
        id: share.id,
        path: path.join("/"),
        auth: hash,
      });
      localStorage.setItem(credKey(fingerprint, share.id), hash);
      setEntries(res.entries);
      setPwOpen(false);
      setErr(null);
    } catch (e) {
      setErr(String(e) === "auth" ? "密码错误" : String(e));
    }
  };

  const goRoots = () => {
    setShare(null);
    setPath([]);
    setEntries([]);
    setErr(null);
  };

  const download = async (entry: ShareEntry) => {
    if (!share) return;
    setDownloading(entry.name);
    setErr(null);
    try {
      const saved = await invoke<string>("lan_share_download", {
        fingerprint,
        id: share.id,
        path: [...path, entry.name].join("/"),
        auth: getAuth(share) ?? null,
        isDir: entry.dir,
      });
      setNotice(`已下载到 ${saved}`);
      setRevealPath(saved);
    } catch (e) {
      setErr(String(e) === "auth" ? "需要密码，请重新进入该共享" : String(e));
    } finally {
      setDownloading(null);
    }
  };

  // 单击选中：普通=单选，Cmd/Ctrl=切换，Shift=连选
  const clickEntry = (ev: React.MouseEvent, idx: number, name: string) => {
    if (ev.metaKey || ev.ctrlKey) {
      setSelected((s) => {
        const n = new Set(s);
        if (n.has(name)) n.delete(name);
        else n.add(name);
        return n;
      });
      lastIdxRef.current = idx;
    } else if (ev.shiftKey && lastIdxRef.current >= 0) {
      const [a, b] = [lastIdxRef.current, idx].sort((x, y) => x - y);
      setSelected(new Set(entries.slice(a, b + 1).map((e) => e.name)));
    } else {
      setSelected(new Set([name]));
      lastIdxRef.current = idx;
    }
  };
  // 双击打开：文件夹进入，文件下载
  const openEntry = (e: ShareEntry) => {
    if (e.dir) load(share!, [...path, e.name]);
    else download(e);
  };
  // 批量下载选中
  const downloadSelected = async () => {
    const list = entries.filter((e) => selected.has(e.name));
    for (const e of list) await download(e);
  };

  const runOp = async (op: "mkdir" | "rename" | "delete", p: string, to?: string) => {
    if (!share) return;
    setBusy(true);
    setErr(null);
    try {
      await invoke("lan_share_op", {
        fingerprint,
        id: share.id,
        op,
        path: p,
        to: to ?? null,
        auth: getAuth(share) ?? null,
      });
      await reload();
    } catch (e) {
      setErr(String(e) === "auth" ? "需要密码，请重新进入该共享" : String(e));
    } finally {
      setBusy(false);
    }
  };

  const pickUpload = async () => {
    try {
      const paths = await invoke<string[]>("lan_pick_files");
      if (paths?.length) startUpload(paths);
    } catch (e) {
      setErr(String(e));
    }
  };
  const pickUploadFolder = async () => {
    try {
      const dir = await invoke<string | null>("lan_pick_dir");
      if (dir) startUpload([dir]);
    } catch (e) {
      setErr(String(e));
    }
  };

  const submitName = async () => {
    if (!namePrompt || !share) return;
    const v = namePrompt.value.trim();
    if (!v) return;
    if (namePrompt.kind === "mkdir") {
      await runOp("mkdir", [...path, v].join("/"));
    } else if (namePrompt.target) {
      await runOp("rename", [...path, namePrompt.target].join("/"), v);
    }
    setNamePrompt(null);
  };

  const canWrite = share && (share.canCreate || share.canModify || share.canDelete);

  // 键盘:↑/↓ 移动选中,Enter 打开,Delete 删除选中(有任何弹框/输入聚焦时不拦截)
  useEffect(() => {
    if (!share) return;
    const onKey = (ev: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      if (pwOpen || namePrompt || delConfirm || overwrite || entries.length === 0) return;
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        const cur = lastIdxRef.current;
        const next = Math.max(
          0,
          Math.min(entries.length - 1, cur < 0 ? 0 : cur + (ev.key === "ArrowDown" ? 1 : -1))
        );
        lastIdxRef.current = next;
        setSelected(new Set([entries[next].name]));
      } else if (ev.key === "Enter" && selected.size === 1) {
        const e = entries.find((x) => selected.has(x.name));
        if (e) {
          ev.preventDefault();
          openEntry(e);
        }
      } else if ((ev.key === "Delete" || ev.key === "Backspace") && share.canDelete && selected.size > 0) {
        ev.preventDefault();
        setDelConfirm([...selected]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [share, entries, selected, pwOpen, namePrompt, delConfirm, overwrite]);

  return createPortal(
    <div className="modal-overlay">
      <div className="modal share-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h3>{peerName} 的共享文件</h3>
          <button className="modal-close" onClick={onClose}>×</button>
        </div>

        {err && <div className="error-banner" style={{ margin: 0 }}>⚠ {err}</div>}
        {notice && (
          <div className="notice-banner share-notice" style={{ margin: 0 }}>
            <span>✓ {notice}</span>
            {revealPath && (
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => invoke("lan_reveal", { path: revealPath }).catch((e) => setErr(String(e)))}
              >
                打开目录
              </button>
            )}
          </div>
        )}

        {/* 面包屑 + 工具栏 */}
        <div className="share-bar">
          <div className="share-crumb">
            <button onClick={goRoots}>共享</button>
            {share && (
              <>
                <span>/</span>
                <button onClick={() => load(share, [])}>{share.name}</button>
              </>
            )}
            {path.map((seg, i) => (
              <span key={i}>
                <span>/</span>
                <button onClick={() => share && load(share, path.slice(0, i + 1))}>{seg}</button>
              </span>
            ))}
          </div>
          {share && (
            <div className="share-tools">
              {share.canCreate && (
                <>
                  <button className="btn btn-ghost btn-sm" onClick={pickUpload} disabled={busy}>上传文件</button>
                  <button className="btn btn-ghost btn-sm" onClick={pickUploadFolder} disabled={busy}>上传文件夹</button>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() => setNamePrompt({ kind: "mkdir", value: "" })}
                    disabled={busy}
                  >
                    新建文件夹
                  </button>
                </>
              )}
            </div>
          )}
        </div>

        {/* 选中后的批量操作条 */}
        {share && selected.size > 0 && (
          <div className="share-selbar">
            <span className="dim">已选 {selected.size} 项</span>
            <button className="btn btn-ghost btn-sm" onClick={downloadSelected} disabled={busy}>下载选中</button>
            {share.canDelete && (
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => setDelConfirm([...selected])}
                disabled={busy}
              >
                删除选中
              </button>
            )}
            <button className="btn btn-ghost btn-sm" onClick={() => setSelected(new Set())}>清除</button>
          </div>
        )}

        <div className="share-list">
          {!share ? (
            <>
              {roots && roots.length === 0 && <div className="dim">对方没有共享目录</div>}
              {roots?.map((r) => (
                <button key={r.id} className="share-item isdir" onClick={() => load(r, [])}>
                  <span className="share-ic">📁</span>
                  <span className="share-name">{r.name}</span>
                  {r.locked && <span className="share-lock" title="需要密码">🔒</span>}
                </button>
              ))}
            </>
          ) : (
            <>
              {entries.length === 0 && !loading && (
                <div className="dim">{share.canCreate ? "空目录（可上传或拖入文件）" : "空目录"}</div>
              )}
              <div className="share-head dim">
                <span className="share-col-name">名称</span>
                <span className="share-col-time">修改时间</span>
                <span className="share-col-time">创建时间</span>
                <span className="share-col-size">大小</span>
              </div>
              {entries.map((e, idx) => (
                <div
                  key={e.name}
                  className={`share-item${e.dir ? " isdir" : ""}${
                    selected.has(e.name) ? " selected" : ""
                  }${downloading === e.name ? " busy" : ""}`}
                  onClick={(ev) => clickEntry(ev, idx, e.name)}
                  onDoubleClick={() => openEntry(e)}
                  onContextMenu={(ev) => {
                    ev.preventDefault();
                    ev.stopPropagation();
                    if (!selected.has(e.name)) setSelected(new Set([e.name])); // 右键先选中
                    setEntryMenu({ x: ev.clientX, y: ev.clientY, entry: e });
                  }}
                  title={e.dir ? "双击进入 · 右键操作" : "双击下载 · 右键操作"}
                >
                  <span className="share-ic">{e.dir ? "📁" : "📄"}</span>
                  <span className="share-name">{e.name}</span>
                  <span className="share-col-time dim">{fmtTime(e.mtime)}</span>
                  <span className="share-col-time dim">{fmtTime(e.ctime)}</span>
                  <span className="share-col-size dim">{e.dir ? "—" : fmtBytes(e.size)}</span>
                </div>
              ))}
            </>
          )}
          {loading && <div className="dim" style={{ padding: "6px 2px" }}>加载中…</div>}
        </div>

        {share && canWrite && (
          <div className="dim" style={{ fontSize: 11 }}>
            权限：{[share.canCreate && "新增", share.canModify && "修改", share.canDelete && "删除"]
              .filter(Boolean)
              .join(" / ") || "只读"}
            {share.canCreate && "（可把本地文件拖进窗口上传）"}
          </div>
        )}

        {/* 上传进度（带动态文字描述） */}
        {(busy || Object.keys(progress).length > 0) && (
          <div className="share-progress">
            <div className="share-prog-title dim">正在上传…</div>
            {Object.keys(progress).length === 0 && (
              <div className="dim" style={{ fontSize: 12 }}>准备中…</div>
            )}
            {Object.entries(progress).map(([name, p]) => {
              const zipping = p.size === 0; // size=0 → 打包阶段
              const pct = p.size ? Math.min(100, (p.transferred / p.size) * 100) : 0;
              const done = p.size > 0 && p.transferred >= p.size;
              return (
                <div key={name} className="share-prog-item">
                  <span className="share-prog-name" title={name}>{name}</span>
                  <span className="share-prog-bar">
                    <span className={zipping ? "indet" : ""} style={{ width: zipping ? "100%" : `${pct}%` }} />
                  </span>
                  <span className="dim share-prog-text">
                    {zipping
                      ? "打包中…"
                      : done
                      ? "已完成"
                      : `${fmtBytes(p.transferred)} / ${fmtBytes(p.size)} · ${Math.round(pct)}%`}
                  </span>
                </div>
              );
            })}
          </div>
        )}

        {/* 密码框 */}
        {pwOpen && (
          <div className="modal-overlay">
            <div className="modal share-subdialog" onClick={(e) => e.stopPropagation()}>
              <div className="dim" style={{ fontSize: 12 }}>该共享需要密码：</div>
              <input
                className="kv-input"
                type="password"
                autoFocus
                value={pw}
                placeholder="访问密码"
                onChange={(e) => setPw(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && submitPw()}
              />
              <div className="modal-actions">
                <button className="btn btn-ghost" onClick={() => { setPwOpen(false); goRoots(); }}>取消</button>
                <button className="btn btn-primary" onClick={submitPw} disabled={!pw}>进入</button>
              </div>
            </div>
          </div>
        )}

        {/* 名称输入（新建文件夹 / 重命名） */}
        {namePrompt && (
          <div className="modal-overlay">
            <div className="modal share-subdialog" onClick={(e) => e.stopPropagation()}>
              <div className="dim" style={{ fontSize: 12 }}>
                {namePrompt.kind === "mkdir" ? "新文件夹名称：" : "重命名为："}
              </div>
              <input
                className="kv-input"
                autoFocus
                value={namePrompt.value}
                onChange={(e) => setNamePrompt((p) => (p ? { ...p, value: e.target.value } : p))}
                onKeyDown={(e) => e.key === "Enter" && submitName()}
              />
              <div className="modal-actions">
                <button className="btn btn-ghost" onClick={() => setNamePrompt(null)}>取消</button>
                <button className="btn btn-primary" onClick={submitName} disabled={!namePrompt.value.trim() || busy}>确定</button>
              </div>
            </div>
          </div>
        )}

        {/* 删除确认（支持批量） */}
        {delConfirm && delConfirm.length > 0 && (
          <div className="modal-overlay">
            <div className="modal share-subdialog" onClick={(e) => e.stopPropagation()}>
              <div style={{ fontSize: 13 }}>
                确认删除{delConfirm.length > 1 ? ` ${delConfirm.length} 项` : `「${delConfirm[0]}」`}？文件夹会连同内容一起删除,此操作不可撤销。
              </div>
              <div className="modal-actions">
                <button className="btn btn-ghost" onClick={() => setDelConfirm(null)}>取消</button>
                <button
                  className="btn btn-danger"
                  disabled={busy}
                  onClick={async () => {
                    const names = delConfirm;
                    setDelConfirm(null);
                    for (const n of names) await runOp("delete", [...path, n].join("/"));
                    setSelected(new Set());
                  }}
                >
                  删除
                </button>
              </div>
            </div>
          </div>
        )}

        {/* 上传覆盖二次确认 */}
        {overwrite && (
          <div className="modal-overlay">
            <div className="modal share-subdialog" onClick={(e) => e.stopPropagation()}>
              <div style={{ fontSize: 13 }}>
                已存在同名项：{overwrite.names.join("、")}，继续上传将<b>覆盖</b>它们。
              </div>
              <div className="modal-actions">
                <button className="btn btn-ghost" onClick={() => setOverwrite(null)}>取消</button>
                <button
                  className="btn btn-danger"
                  onClick={() => {
                    const locals = overwrite.locals;
                    setOverwrite(null);
                    doUpload(locals);
                  }}
                >
                  覆盖上传
                </button>
              </div>
            </div>
          </div>
        )}
      </div>

      {/* 文件右键菜单 */}
      {entryMenu && share && (
        <div
          className="tab-menu"
          style={{ left: entryMenu.x, top: entryMenu.y }}
          onClick={(e) => e.stopPropagation()}
        >
          <button
            className="tab-menu-item"
            onClick={() => {
              download(entryMenu.entry);
              setEntryMenu(null);
            }}
          >
            下载
          </button>
          {share.canModify && (
            <button
              className="tab-menu-item"
              onClick={() => {
                setNamePrompt({ kind: "rename", target: entryMenu.entry.name, value: entryMenu.entry.name });
                setEntryMenu(null);
              }}
            >
              重命名
            </button>
          )}
          {share.canDelete && (
            <button
              className="tab-menu-item danger"
              onClick={() => {
                // 若右键项在多选内,批量删除;否则删该项
                setDelConfirm(selected.has(entryMenu.entry.name) ? [...selected] : [entryMenu.entry.name]);
                setEntryMenu(null);
              }}
            >
              删除
            </button>
          )}
        </div>
      )}
    </div>,
    document.body
  );
}
