import { useState, useEffect, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";

interface PortInfo {
  command: string;
  pid: number;
  user: string;
  protocol: string;
  local_addr: string;
  local_port: string;
  remote_addr: string | null;
  remote_port: string | null;
  state: string | null;
  fd_type: string;
}

export default function PortScanner() {
  const [ports, setPorts] = useState<PortInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [onlyListen, setOnlyListen] = useState(false);
  const [auto, setAuto] = useState(false);
  const [killing, setKilling] = useState<Set<number>>(new Set());

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<PortInfo[]>("list_ports");
      setPorts(result);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // 自动刷新：开启后每 3 秒拉一次端口列表
  useEffect(() => {
    if (!auto) return;
    const id = setInterval(refresh, 3000);
    return () => clearInterval(id);
  }, [auto, refresh]);

  const killProcess = async (pid: number, command: string) => {
    if (!confirm(`确认终止进程「${command}」(PID: ${pid})？\n\n此操作不可撤销。`)) return;
    setKilling((prev) => new Set(prev).add(pid));
    try {
      await invoke("kill_process", { pid });
      setPorts((prev) => prev.filter((p) => p.pid !== pid));
    } catch (e) {
      alert(`终止失败：${e}`);
    } finally {
      setKilling((prev) => {
        const next = new Set(prev);
        next.delete(pid);
        return next;
      });
    }
  };

  const stateKey = (s: string | null) =>
    s ? s.toLowerCase().replace(/[_\s]/g, "-") : "";

  // 过滤 + 排序合并进 useMemo，避免每次渲染（含每次搜索按键）都重算 O(n log n)
  const sorted = useMemo(() => {
    const q = filter.toLowerCase();
    const filtered = ports.filter((p) => {
      if (onlyListen && p.state !== "LISTEN") return false;
      if (!filter) return true;
      return (
        p.command.toLowerCase().includes(q) ||
        p.local_port.includes(q) ||
        p.local_addr.toLowerCase().includes(q) ||
        String(p.pid).includes(q) ||
        p.protocol.toLowerCase().includes(q) ||
        (p.remote_addr?.toLowerCase().includes(q) ?? false) ||
        (p.state?.toLowerCase().includes(q) ?? false)
      );
    });
    return filtered.sort((a, b) => {
      const pa = parseInt(a.local_port) || 0;
      const pb = parseInt(b.local_port) || 0;
      return pa - pb;
    });
  }, [ports, filter, onlyListen]);

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>端口查询</h2>
        <div className="tool-actions">
          <input
            className="search-input"
            placeholder="搜索端口、进程名、地址..."
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          <button
            className={`btn btn-ghost${onlyListen ? " active" : ""}`}
            onClick={() => setOnlyListen((v) => !v)}
            title="仅显示 LISTEN 状态"
          >
            仅监听
          </button>
          <button
            className={`btn btn-ghost${auto ? " active" : ""}`}
            onClick={() => setAuto((v) => !v)}
            title="每 3 秒自动刷新"
          >
            自动
          </button>
          <button className="btn btn-primary" onClick={refresh} disabled={loading}>
            {loading ? "加载中…" : "刷新"}
          </button>
        </div>
      </div>

      {error && <div className="error-banner">⚠ {error}</div>}

      <div className="table-wrap">
        {sorted.length === 0 && !loading ? (
          <div className="empty">
            <div className="empty-icon">⚡</div>
            <div>{filter || onlyListen ? "无匹配结果" : "暂无数据，点击刷新"}</div>
          </div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>协议</th>
                <th>本地地址</th>
                <th>端口</th>
                <th>远程</th>
                <th>状态</th>
                <th>进程</th>
                <th>PID</th>
                <th>用户</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {sorted.map((p, i) => (
                <tr key={`${p.pid}-${p.local_port}-${i}`}>
                  <td>
                    <span className={`badge badge-${p.protocol.toLowerCase()}`}>
                      {p.protocol}
                    </span>
                  </td>
                  <td className="mono dim">{p.local_addr}</td>
                  <td className="mono port">{p.local_port}</td>
                  <td className="mono dim" style={{ fontSize: 11 }}>
                    {p.remote_addr
                      ? `${p.remote_addr}:${p.remote_port}`
                      : "—"}
                  </td>
                  <td>
                    {p.state ? (
                      <span className={`state-pill state-${stateKey(p.state)}`}>
                        {p.state}
                      </span>
                    ) : (
                      <span className="dim">—</span>
                    )}
                  </td>
                  <td className="bold">{p.command}</td>
                  <td className="mono dim">{p.pid}</td>
                  <td className="dim">{p.user}</td>
                  <td>
                    <button
                      className="btn btn-danger btn-sm"
                      onClick={() => killProcess(p.pid, p.command)}
                      disabled={killing.has(p.pid)}
                    >
                      {killing.has(p.pid) ? "…" : "终止"}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="tool-footer">
        <span>
          {loading ? "加载中…" : `${sorted.length} 条`}
          {(filter || onlyListen) && ports.length > 0
            ? ` / 共 ${ports.length} 条`
            : ""}
        </span>
      </div>
    </div>
  );
}
