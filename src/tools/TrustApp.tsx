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

  // 拖拽事件是整个 webview 全局的：用包围盒把它限定在本工具可见的投放区内，
  // 顺带过滤掉非激活（隐藏，rect 为 0）的本组件实例。坐标是物理像素，需除以 DPR。
  const inZone = useCallback((pos: { x: number; y: number }) => {
    const el = zoneRef.current;
    if (!el) return false;
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const dpr = window.devicePixelRatio || 1;
    const x = pos.x / dpr;
    const y = pos.y / dpr;
    return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
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
        setOver(inZone(pl.position));
      } else if (pl.type === "drop") {
        setOver(false);
        if (!inZone(pl.position)) return;
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
  }, [inZone, setApp]);

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
    <div className="tool-container">
      <div className="tool-header">
        <h2>Mac 应用授权</h2>
      </div>

      {error && <div className="error-banner">⚠ {error}</div>}
      {notice && <div className="notice-banner">✓ {notice}</div>}

      <div className="trust-body">
        <div
          ref={zoneRef}
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
