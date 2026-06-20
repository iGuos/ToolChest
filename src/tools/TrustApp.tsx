import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";

export default function TrustApp() {
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [over, setOver] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const zoneRef = useRef<HTMLDivElement>(null);

  const appName = path ? path.split("/").filter(Boolean).pop() ?? path : "";
  // 与后端 trust_app 实际执行的命令保持一致（以管理员权限运行），供用户核对
  const command = path
    ? `sudo xattr -dr com.apple.quarantine "${path}"`
    : "";

  // 拖拽事件是整个 webview 全局的（所有已打开 tab 的组件都在监听）。这里不再做坐标命中判断
  // （坐标系/DPR/偏移容易在边缘出错，导致左侧等区域失效），而是只在「本工具是当前可见 tab」时接收：
  // 非激活 tab 是 display:none → 尺寸为 0，据此过滤；可见时整页任意位置都可投放。
  const isViewVisible = useCallback(() => {
    const el = zoneRef.current;
    if (!el) return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }, []);

  const setApp = useCallback((p: string) => {
    if (!p.toLowerCase().endsWith(".app")) {
      setError("请拖入 / 选择 .app 应用");
      return;
    }
    setError(null);
    setNotice(null);
    setPath(p);
  }, []);

  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent((event) => {
      const pl = event.payload;
      if (pl.type === "over") {
        setOver(isViewVisible());
      } else if (pl.type === "drop") {
        setOver(false);
        if (!isViewVisible()) return; // 非当前可见 tab 不接收
        const app =
          pl.paths.find((p) => p.toLowerCase().endsWith(".app")) ?? pl.paths[0];
        if (app) setApp(app);
      } else {
        setOver(false);
      }
    });
    return () => {
      un.then((u) => u());
    };
  }, [isViewVisible, setApp]);

  const pick = async () => {
    setError(null);
    try {
      const p = await invoke<string | null>("pick_app");
      if (p) setApp(p);
    } catch (e) {
      setError(String(e));
    }
  };

  const trust = async () => {
    if (!path) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const msg = await invoke<string>("trust_app", { path });
      setNotice(msg);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const clear = () => {
    setPath("");
    setError(null);
    setNotice(null);
  };

  return (
    <div className="tool-container" ref={zoneRef}>
      <div className="tool-header">
        <h2>Mac 应用授权</h2>
      </div>

      {error && <div className="error-banner">⚠ {error}</div>}
      {notice && <div className="notice-banner">✓ {notice}</div>}

      <div className="trust-body">
        <div
          className={`drop-zone${over ? " over" : ""}${path ? " filled" : ""}`}
          onClick={pick}
          title="点击选择，或将 App 拖拽到此处"
        >
          {path ? (
            <>
              <div className="drop-icon">📦</div>
              <div className="drop-app-name">{appName}</div>
              <div className="drop-app-path">{path}</div>
              <div className="drop-hint">点击可重新选择</div>
            </>
          ) : (
            <>
              <div className="drop-icon">{over ? "📥" : "⬇"}</div>
              <div className="drop-title">将 App 拖拽到此处</div>
              <div className="drop-hint">或点击，从「应用程序」中选择 .app</div>
            </>
          )}
        </div>

        <div className="trust-command">
          <label className="trust-command-label dim">将执行的命令</label>
          <input
            className="trust-command-input"
            type="text"
            readOnly
            value={command}
            placeholder="选择 App 后，这里显示将要执行的命令"
            title={command || undefined}
            onFocus={(e) => e.currentTarget.select()}
          />
        </div>

        <div className="trust-actions">
          <button
            className="btn btn-primary"
            onClick={trust}
            disabled={!path || busy}
          >
            {busy ? "授权中…" : "信任此应用"}
          </button>
          {path && !busy && (
            <button className="btn btn-ghost" onClick={clear}>
              清除
            </button>
          )}
        </div>

        <p className="trust-desc dim">
          「信任此应用」会移除该 App 的隔离标记（com.apple.quarantine），用于打开提示
          「无法验证开发者」「已损坏，应移到废纸篓」的应用。点击后会弹出系统密码框，
          输入登录密码授权即可。
        </p>
      </div>
    </div>
  );
}
