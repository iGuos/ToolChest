import { useEffect, useState } from "react";
import { useLan } from "./lanContext";

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

// 收到文件请求的确认弹框。发送方每个文件各自一个会话，所以「当前文件」即单个文件。
//   「当前文件」：确认你点击的这个文件；
//   「全部待接收」：列出所有待接收文件，可勾选后接受所选。
export default function LanIncomingModal() {
  const {
    confirm: incoming,
    pendingFiles,
    peers,
    respond,
    acceptPendingFiles,
    rejectAllPending,
    dismissConfirm,
  } = useLan();
  const [tab, setTab] = useState<"current" | "all">("current");
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [confirmReject, setConfirmReject] = useState(false); // 全部拒绝二次确认

  useEffect(() => {
    if (incoming) {
      setTab("current");
      // 「全部待接收」默认勾选全部，可手动取消
      setPicked(new Set(pendingFiles.map((f) => f.id)));
      setConfirmReject(false);
    }
    // 仅在弹框打开（incoming 变化）时重置选择
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [incoming]);

  if (!incoming) return null;

  const toggle = (id: string) =>
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const currentTotal = incoming.files.reduce((a, f) => a + f.size, 0);
  const aliasOf = (fp: string) => peers.find((p) => p.fingerprint === fp)?.alias ?? "未知设备";
  const pickedFiles = pendingFiles.filter((f) => picked.has(f.id));
  const pickedTotal = pickedFiles.reduce((a, f) => a + f.size, 0);

  return (
    <div className="modal-overlay" onClick={dismissConfirm}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h3>收到文件请求</h3>
          <button className="modal-close" title="关闭（保留待接收）" onClick={dismissConfirm}>
            ×
          </button>
        </div>

        <div className="sub-tabs" style={{ marginTop: 4 }}>
          <button
            className={`sub-tab${tab === "current" ? " active" : ""}`}
            onClick={() => setTab("current")}
          >
            当前文件
          </button>
          <button
            className={`sub-tab${tab === "all" ? " active" : ""}`}
            onClick={() => setTab("all")}
          >
            全部待接收（{pendingFiles.length}）
          </button>
        </div>

        {tab === "current" ? (
          <>
            <div className="dim" style={{ fontSize: 13, marginTop: 8 }}>
              <b>{incoming.alias}</b>（{incoming.isBaibao ? "百宝箱" : "LocalSend"}）发来的文件：
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
              <button className="btn btn-ghost" onClick={() => respond(false, [])}>拒绝</button>
              <button className="btn btn-primary" onClick={() => respond(true, [])}>
                接受（{fmtBytes(currentTotal)}）
              </button>
            </div>
          </>
        ) : (
          <>
            <div className="dim" style={{ fontSize: 13, marginTop: 8 }}>
              共 {pendingFiles.length} 个待接收文件，勾选后接受所选：
            </div>
            <div className="lan-req-files">
              {pendingFiles.length === 0 && <div className="dim">没有待接收的文件</div>}
              {pendingFiles.map((f) => (
                <label key={f.id} className="lan-req-file">
                  <input type="checkbox" checked={picked.has(f.id)} onChange={() => toggle(f.id)} />
                  <span className="lan-xfer-name" title={f.fileName}>{f.fileName}</span>
                  <span className="dim" style={{ flexShrink: 0 }}>{aliasOf(f.fingerprint)}</span>
                  <span className="dim" style={{ flexShrink: 0 }}>{fmtBytes(f.size)}</span>
                </label>
              ))}
            </div>
            {confirmReject ? (
              <div className="modal-actions">
                <span className="dim" style={{ marginRight: "auto", fontSize: 13 }}>
                  确认拒绝全部 {pendingFiles.length} 个文件？
                </span>
                <button className="btn btn-ghost" onClick={() => setConfirmReject(false)}>
                  返回
                </button>
                <button className="btn btn-danger" onClick={rejectAllPending}>
                  确认全部拒绝
                </button>
              </div>
            ) : (
              <div className="modal-actions">
                <button className="btn btn-ghost" onClick={dismissConfirm}>关闭</button>
                <button
                  className="btn btn-danger"
                  disabled={pendingFiles.length === 0}
                  onClick={() => setConfirmReject(true)}
                >
                  全部拒绝（{pendingFiles.length}）
                </button>
                <button
                  className="btn btn-primary"
                  disabled={pickedFiles.length === 0}
                  onClick={() => acceptPendingFiles(pickedFiles)}
                >
                  接受所选（{pickedFiles.length} 项 · {fmtBytes(pickedTotal)}）
                </button>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
