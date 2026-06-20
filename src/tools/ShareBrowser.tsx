import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useEscToClose } from "../hooks";

// 网上邻居式的共享文件浏览器：列对方共享根 → 进目录 → 下载文件。
// 加锁的共享首次访问弹密码框，认证通过后把 sha256(密码) 存本地，下次免输（保存凭据）。

interface ShareRoot {
  id: string;
  name: string;
  locked: boolean;
}
interface ShareEntry {
  name: string;
  dir: boolean;
  size: number;
  mtime: number;
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
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
  const [share, setShare] = useState<ShareRoot | null>(null); // 当前进入的共享根
  const [path, setPath] = useState<string[]>([]); // 当前相对路径分段
  const [entries, setEntries] = useState<ShareEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [pw, setPw] = useState(""); // 密码输入（弹框打开时）
  const [pwOpen, setPwOpen] = useState(false);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [mounting, setMounting] = useState<string | null>(null);

  const mountShare = async (sh: ShareRoot) => {
    setMounting(sh.id);
    setErr(null);
    try {
      const msg = await invoke<string>("lan_mount_share", { fingerprint, shareId: sh.id });
      setNotice(msg);
    } catch (e) {
      setErr(String(e));
    } finally {
      setMounting(null);
    }
  };

  useEffect(() => {
    setLoading(true);
    invoke<ShareRoot[]>("lan_share_roots", { fingerprint })
      .then((r) => setRoots(r))
      .catch((e) => setErr(String(e)))
      .finally(() => setLoading(false));
  }, [fingerprint]);

  // 列出 sh 共享下 segs 路径的内容；401 → 打开密码框。authOverride 用于密码框重试。
  const load = useCallback(
    async (sh: ShareRoot, segs: string[], authOverride?: string) => {
      setErr(null);
      setLoading(true);
      try {
        const auth =
          authOverride ??
          (sh.locked ? localStorage.getItem(credKey(fingerprint, sh.id)) ?? undefined : undefined);
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
      localStorage.setItem(credKey(fingerprint, share.id), hash); // 保存凭据
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
  const download = async (name: string) => {
    if (!share) return;
    setDownloading(name);
    setErr(null);
    try {
      const auth = share.locked
        ? localStorage.getItem(credKey(fingerprint, share.id)) ?? undefined
        : undefined;
      const saved = await invoke<string>("lan_share_download", {
        fingerprint,
        id: share.id,
        path: [...path, name].join("/"),
        auth: auth ?? null,
      });
      setNotice(`已下载到 ${saved}`);
    } catch (e) {
      setErr(String(e) === "auth" ? "需要密码，请重新进入该共享" : String(e));
    } finally {
      setDownloading(null);
    }
  };

  return createPortal(
    <div className="modal-overlay">
      <div className="modal share-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h3>{peerName} 的共享文件</h3>
          <button className="modal-close" onClick={onClose}>×</button>
        </div>

        {err && <div className="error-banner" style={{ margin: 0 }}>⚠ {err}</div>}
        {notice && <div className="notice-banner" style={{ margin: 0 }}>✓ {notice}</div>}

        {/* 面包屑 */}
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

        <div className="share-list">
          {!share ? (
            <>
              {roots && roots.length === 0 && <div className="dim">对方没有共享目录</div>}
              {roots?.map((r) => (
                <div key={r.id} className="share-item isdir" onClick={() => load(r, [])}>
                  <span className="share-ic">📁</span>
                  <span className="share-name">{r.name}</span>
                  {r.locked && <span className="share-lock" title="需要密码">🔒</span>}
                  <button
                    className="btn btn-ghost btn-sm"
                    title="映射成系统磁盘（macOS 访达 / Windows 盘符）"
                    onClick={(ev) => {
                      ev.stopPropagation();
                      mountShare(r);
                    }}
                    disabled={mounting === r.id}
                  >
                    {mounting === r.id ? "映射中…" : "映射磁盘"}
                  </button>
                </div>
              ))}
            </>
          ) : (
            <>
              {entries.length === 0 && !loading && <div className="dim">空目录</div>}
              {entries.map((e) => (
                <div
                  key={e.name}
                  className={`share-item${e.dir ? " isdir" : ""}`}
                  onClick={e.dir ? () => load(share, [...path, e.name]) : undefined}
                >
                  <span className="share-ic">{e.dir ? "📁" : "📄"}</span>
                  <span className="share-name">{e.name}</span>
                  {!e.dir && <span className="dim share-size">{fmtBytes(e.size)}</span>}
                  {!e.dir && (
                    <button
                      className="btn btn-ghost btn-sm"
                      onClick={(ev) => {
                        ev.stopPropagation();
                        download(e.name);
                      }}
                      disabled={downloading === e.name}
                    >
                      {downloading === e.name ? "下载中…" : "下载"}
                    </button>
                  )}
                </div>
              ))}
            </>
          )}
          {loading && <div className="dim" style={{ padding: "6px 2px" }}>加载中…</div>}
        </div>

        {/* 密码框 */}
        {pwOpen && (
          <div className="share-pw">
            <div className="dim" style={{ fontSize: 12 }}>
              该共享需要密码：
            </div>
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
              <button className="btn btn-ghost" onClick={() => { setPwOpen(false); goRoots(); }}>
                取消
              </button>
              <button className="btn btn-primary" onClick={submitPw} disabled={!pw}>
                进入
              </button>
            </div>
          </div>
        )}
      </div>
    </div>,
    document.body
  );
}
