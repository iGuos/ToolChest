import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface LogEntry {
  time: string;
  msg: string;
  type: "ok" | "warn" | "err" | "dim";
}

function now() {
  return new Date().toLocaleTimeString("zh", { hour12: false });
}

export default function ClaudeWatcher() {
  const [running, setRunning] = useState(false);
  const [diagnosing, setDiagnosing] = useState(false);
  const [clicking, setClicking] = useState(false);
  const [interval, setIntervalMs] = useState(2);
  const [accessible, setAccessible] = useState<boolean | null>(null);
  const [binaryPath, setBinaryPath] = useState("");
  const [logs, setLogs] = useState<LogEntry[]>([
    { time: now(), msg: "就绪，点击「启动」开始监听", type: "dim" },
  ]);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const logBoxRef = useRef<HTMLDivElement>(null);

  const addLog = useCallback((msg: string, type: LogEntry["type"] = "dim") => {
    setLogs((prev) => [...prev.slice(-199), { time: now(), msg, type }]);
  }, []);

  useEffect(() => {
    if (logBoxRef.current) {
      logBoxRef.current.scrollTop = logBoxRef.current.scrollHeight;
    }
  }, [logs]);

  const checkAccess = useCallback(async () => {
    try {
      const ok = await invoke<boolean>("check_accessibility");
      setAccessible(ok);
      return ok;
    } catch {
      setAccessible(false);
      return false;
    }
  }, []);

  useEffect(() => {
    checkAccess();
    invoke<string>("get_binary_path").then(setBinaryPath).catch(() => {});
  }, [checkAccess]);

  // Drain click results written by the persistent watcher loop.
  const tick = useCallback(async () => {
    try {
      const clicks = await invoke<string[]>("read_watcher_clicks");
      clicks.forEach((c) => addLog(`✓ ${c}`, "ok"));
    } catch (e) {
      addLog(`轮询错误: ${e}`, "err");
    }
  }, [addLog]);

  useEffect(() => {
    if (!running) return;
    const loop = async () => {
      await tick();
      timerRef.current = setTimeout(loop, interval * 1000);
    };
    timerRef.current = setTimeout(loop, 500);
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [running, interval, tick]);

  const handleStart = async () => {
    const ok = await checkAccess();
    if (!ok) {
      addLog("✗ UI elements enabled = false，辅助功能权限未生效", "err");
      addLog("→ 请将下方二进制路径添加到系统设置 › 辅助功能", "err");
      return;
    }
    try {
      await invoke("start_watcher");
    } catch (e) {
      addLog(`启动监听进程失败: ${e}`, "err");
      return;
    }
    addLog("开始监听（常驻热循环，每秒扫描自动点击）…", "warn");
    setRunning(true);
  };

  const handleStop = async () => {
    if (timerRef.current) clearTimeout(timerRef.current);
    try {
      await invoke("stop_watcher");
    } catch (e) {
      addLog(`停止失败: ${e}`, "err");
    }
    setRunning(false);
    addLog("已停止", "dim");
  };

  const statusColor = running ? "running" : accessible === false ? "error" : "stopped";
  const statusText = running ? "监听中" : accessible === false ? "无权限" : "已停止";

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>Claude 自动授权</h2>
        <div className="tool-actions">
          <button
            className="btn btn-ghost btn-sm"
            disabled={diagnosing}
            onClick={async () => {
              if (diagnosing) return;
              setDiagnosing(true);
              addLog("── 诊断信息（扫描中…）──", "warn");
              try {
                const btns = await invoke<string[]>("dump_claude_buttons");
                btns.forEach((b) => addLog(b, "dim"));
              } catch (e) {
                addLog(`dump 失败: ${e}`, "err");
              } finally {
                setDiagnosing(false);
              }
            }}
          >
            {diagnosing ? "诊断中…" : "诊断"}
          </button>
          <button
            className="btn btn-ghost btn-sm"
            disabled={clicking}
            onClick={async () => {
              if (clicking) return;
              setClicking(true);
              addLog("── 试点一次：先看树，再点击 ──", "warn");
              try {
                addLog("① 当前树状态：", "dim");
                const btns = await invoke<string[]>("dump_claude_buttons");
                btns.forEach((b) => addLog(b, "dim"));
                addLog("② 尝试点击：", "dim");
                const res = await invoke<string[]>("check_claude_dialogs");
                if (res.length > 0) {
                  res.forEach((r) => addLog(`✓ ${r}`, "ok"));
                } else {
                  addLog("未发现可点击的授权按钮", "dim");
                }
              } catch (e) {
                addLog(`试点失败: ${e}`, "err");
              } finally {
                setClicking(false);
              }
            }}
          >
            {clicking ? "试点中…" : "试点一次"}
          </button>
          <button className="btn btn-ghost btn-sm" onClick={checkAccess}>
            检查权限
          </button>
          {running ? (
            <button className="btn btn-danger" onClick={handleStop}>停止</button>
          ) : (
            <button className="btn btn-primary" onClick={handleStart} disabled={accessible === null}>
              启动
            </button>
          )}
        </div>
      </div>

      <div className="watcher-body">
        {/* Status */}
        <div className="status-card">
          <div className="status-row">
            <div className="status-label">
              <div className={`status-dot ${statusColor}`} />
              <span>{statusText}</span>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span className="dim" style={{ fontSize: 12 }}>检测间隔</span>
              <input
                type="number"
                min={1}
                max={30}
                value={interval}
                onChange={(e) => setIntervalMs(Math.max(1, parseInt(e.target.value) || 2))}
                disabled={running}
                style={{
                  width: 52,
                  background: "var(--bg3)",
                  border: "1px solid var(--border)",
                  color: "var(--text)",
                  padding: "4px 8px",
                  borderRadius: 4,
                  fontSize: 13,
                  outline: "none",
                }}
              />
              <span className="dim" style={{ fontSize: 12 }}>秒</span>
            </div>
          </div>
        </div>

        {/* Accessibility permission guide */}
        {accessible === false && (
          <div className="status-card" style={{ borderColor: "rgba(244,71,71,.4)" }}>
            <div style={{ color: "var(--red)", fontWeight: 600, marginBottom: 10, fontSize: 13 }}>
              ⚠ 辅助功能权限未生效（UI elements enabled = false）
            </div>
            <div style={{ fontSize: 12, color: "var(--text-dim)", lineHeight: 1.8 }}>
              <b style={{ color: "var(--text)" }}>原因：</b>
              Tauri dev 模式每次编译都更换二进制路径，macOS 不会自动继承权限。<br />
              <b style={{ color: "var(--text)" }}>解决步骤：</b><br />
              1. 打开「系统设置 › 隐私与安全性 › 辅助功能」<br />
              2. 点击「+」，手动添加以下文件：
            </div>
            <div
              style={{
                marginTop: 8,
                padding: "8px 10px",
                background: "var(--bg3)",
                borderRadius: 4,
                fontFamily: "monospace",
                fontSize: 11,
                color: "var(--yellow)",
                wordBreak: "break-all",
                cursor: "pointer",
              }}
              onClick={() => navigator.clipboard.writeText(binaryPath)}
              title="点击复制路径"
            >
              {binaryPath || "加载中…"}
              <span style={{ color: "var(--text-dim)", marginLeft: 8 }}>(点击复制)</span>
            </div>
            <button
              className="btn btn-ghost btn-sm"
              style={{ marginTop: 10 }}
              onClick={() => invoke("open_accessibility_settings")}
            >
              打开系统设置 →
            </button>
          </div>
        )}

        {accessible === true && (
          <div className="status-card" style={{ borderColor: "rgba(78,201,176,.3)" }}>
            <div style={{ fontSize: 12, color: "var(--text-dim)", lineHeight: 1.7 }}>
              <span style={{ color: "var(--green)", fontWeight: 600 }}>✓ 辅助功能已授权</span>
              <br />
              启动后会拉起一个常驻后台进程，持续监听 Claude Desktop / Claude Code
              所有窗口（每秒扫描），检测到 Allow / Allow Once / 允许 等按钮时自动点击
              （优先「Allow once」）。
              <br />
              若未生效，点「诊断」查看实际按钮名称，或点「试点一次」手动触发。
            </div>
          </div>
        )}

        {/* Log */}
        <div>
          <div style={{
            fontSize: 12,
            color: "var(--text-dim)",
            marginBottom: 6,
            display: "flex",
            justifyContent: "space-between",
          }}>
            <span>运行日志</span>
            <span
              style={{ cursor: "pointer", textDecoration: "underline" }}
              onClick={() => setLogs([{ time: now(), msg: "日志已清空", type: "dim" }])}
            >
              清空
            </span>
          </div>
          <div className="log-box" ref={logBoxRef}>
            {logs.map((entry, i) => (
              <div key={i} className={`log-entry ${entry.type}`}>
                <span style={{ color: "var(--text-dim)" }}>{entry.time}</span> {entry.msg}
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
