import { useEffect, useState } from "react";
import { useLan } from "./lanContext";

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

// 收到文件请求的确认弹框。渲染在 app 根部，无论当前在哪个 tab 都能看到。
// 支持勾选只接收部分文件；不操作则后端 70s 超时自动拒绝。
export default function LanIncomingModal() {
  const { confirm: incoming, respond } = useLan();
  const [picked, setPicked] = useState<Set<string>>(new Set());

  useEffect(() => {
    setPicked(new Set(incoming?.files.map((f) => f.id) ?? []));
  }, [incoming]);

  if (!incoming) return null;

  const toggle = (id: string) =>
    setPicked((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const total = incoming.files
    .filter((f) => picked.has(f.id))
    .reduce((a, f) => a + f.size, 0);

  return (
    <div className="modal-overlay" onClick={() => respond(false, [])}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>收到文件请求</h3>
        <div className="dim" style={{ fontSize: 13 }}>
          <b>{incoming.alias}</b>（{incoming.isBaibao ? "百宝箱" : "LocalSend"}）想发送{" "}
          {incoming.files.length} 个文件：
        </div>
        <div className="lan-req-files">
          {incoming.files.map((f) => (
            <label key={f.id} className="lan-req-file">
              <input
                type="checkbox"
                checked={picked.has(f.id)}
                onChange={() => toggle(f.id)}
              />
              <span className="lan-xfer-name" title={f.fileName}>{f.fileName}</span>
              <span className="dim">{fmtBytes(f.size)}</span>
            </label>
          ))}
        </div>
        <div className="modal-actions">
          <button className="btn btn-ghost" onClick={() => respond(false, [])}>
            拒绝
          </button>
          <button
            className="btn btn-primary"
            disabled={picked.size === 0}
            onClick={() => respond(true, [...picked])}
          >
            接受 {picked.size}/{incoming.files.length} 项（{fmtBytes(total)}）
          </button>
        </div>
      </div>
    </div>
  );
}
