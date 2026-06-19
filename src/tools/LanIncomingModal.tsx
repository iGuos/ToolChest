import { useEffect, useState } from "react";
import { useLan } from "./lanContext";

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

// 收到文件请求的确认弹框。Tab 模式：
//   「当前文件」默认只确认点击的这次请求（可勾选其中文件）；
//   「全部待接收」列出所有发送方待接收的文件，可一次全部接收。
export default function LanIncomingModal() {
  const { confirm: incoming, pendingFiles, peers, respond, acceptAllPending } = useLan();
  const [tab, setTab] = useState<"current" | "all">("current");
  const [picked, setPicked] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (incoming) {
      setTab("current");
      setPicked(new Set(incoming.files.map((f) => f.id)));
    }
  }, [incoming]);

  if (!incoming) return null;

  const toggle = (id: string) =>
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const pickedTotal = incoming.files
    .filter((f) => picked.has(f.id))
    .reduce((a, f) => a + f.size, 0);
  const aliasOf = (fp: string) => peers.find((p) => p.fingerprint === fp)?.alias ?? "未知设备";
  const allTotal = pendingFiles.reduce((a, f) => a + f.size, 0);

  return (
    <div className="modal-overlay" onClick={() => respond(false, [])}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>收到文件请求</h3>

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
              <b>{incoming.alias}</b>（{incoming.isBaibao ? "百宝箱" : "LocalSend"}）想发送{" "}
              {incoming.files.length} 个文件：
            </div>
            <div className="lan-req-files">
              {incoming.files.map((f) => (
                <label key={f.id} className="lan-req-file">
                  <input type="checkbox" checked={picked.has(f.id)} onChange={() => toggle(f.id)} />
                  <span className="lan-xfer-name" title={f.fileName}>{f.fileName}</span>
                  <span className="dim">{fmtBytes(f.size)}</span>
                </label>
              ))}
            </div>
            <div className="modal-actions">
              <button className="btn btn-ghost" onClick={() => respond(false, [])}>拒绝</button>
              <button
                className="btn btn-primary"
                disabled={picked.size === 0}
                onClick={() => respond(true, [...picked])}
              >
                接受 {picked.size}/{incoming.files.length} 项（{fmtBytes(pickedTotal)}）
              </button>
            </div>
          </>
        ) : (
          <>
            <div className="dim" style={{ fontSize: 13, marginTop: 8 }}>
              共 {pendingFiles.length} 个待接收文件，来自各发送方：
            </div>
            <div className="lan-req-files">
              {pendingFiles.length === 0 && <div className="dim">没有待接收的文件</div>}
              {pendingFiles.map((f) => (
                <div key={f.id} className="lan-req-file">
                  <span className="lan-xfer-name" title={f.fileName}>{f.fileName}</span>
                  <span className="dim" style={{ flexShrink: 0 }}>{aliasOf(f.fingerprint)}</span>
                  <span className="dim" style={{ flexShrink: 0 }}>{fmtBytes(f.size)}</span>
                </div>
              ))}
            </div>
            <div className="modal-actions">
              <button className="btn btn-ghost" onClick={() => respond(false, [])}>关闭</button>
              <button
                className="btn btn-primary"
                disabled={pendingFiles.length === 0}
                onClick={acceptAllPending}
              >
                全部接受（{pendingFiles.length} 项 · {fmtBytes(allTotal)}）
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
