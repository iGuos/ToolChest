import { useCallback, useEffect, useRef, useState } from "react";
import { Webview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";

// 直接套 DeepSeek 网页端。用 Tauri 原生子 webview（不是 iframe，
// 因为 chat.deepseek.com 带 X-Frame-Options，iframe 会被浏览器拦）。
// 原生 webview 是覆盖在 React DOM 之上的独立层，所以这里要：
//   1. 量出 .ds-host 在窗口里的位置/尺寸，把 webview 摆到同一块区域；
//   2. tab 不激活时把它挪到屏幕外（保留登录态与页面状态，比销毁更省）；
//   3. 容器尺寸变化 / 窗口缩放时重新同步。
const DS_URL = "https://chat.deepseek.com";
const OFFSCREEN = -20000; // 不激活时把 webview 挪到这，等同隐藏

export default function DeepSeek({ active }: { active: boolean }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const wvRef = useRef<Webview | null>(null);
  const readyRef = useRef(false);
  const activeRef = useRef(active);
  activeRef.current = active;
  const [failed, setFailed] = useState(false);
  // 改这个 key 会触发重建 webview，作为「刷新」用（cookie 持久化，不会掉登录）
  const [reloadKey, setReloadKey] = useState(0);

  // 把 webview 摆到 .ds-host 当前所在的区域；不激活则挪到屏幕外
  const sync = useCallback(() => {
    const wv = wvRef.current;
    const host = hostRef.current;
    if (!wv || !readyRef.current || !host) return;
    if (!activeRef.current) {
      wv.setPosition(new LogicalPosition(OFFSCREEN, OFFSCREEN)).catch(() => {});
      return;
    }
    const r = host.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return; // display:none 时全是 0，跳过
    wv.setPosition(new LogicalPosition(Math.round(r.left), Math.round(r.top))).catch(() => {});
    wv.setSize(new LogicalSize(Math.round(r.width), Math.round(r.height))).catch(() => {});
  }, []);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const r = host.getBoundingClientRect();
    const onScreen = active && r.width >= 1 && r.height >= 1;

    const wv = new Webview(getCurrentWindow(), `deepseek-${reloadKey}-${Date.now()}`, {
      url: DS_URL,
      x: onScreen ? Math.round(r.left) : OFFSCREEN,
      y: onScreen ? Math.round(r.top) : OFFSCREEN,
      width: Math.max(1, Math.round(r.width)),
      height: Math.max(1, Math.round(r.height)),
    });
    wvRef.current = wv;
    setFailed(false);

    wv.once("tauri://created", () => {
      readyRef.current = true;
      sync();
    });
    wv.once("tauri://error", (e) => {
      console.error("DeepSeek webview 创建失败", e);
      setFailed(true);
    });
    // 兜底：个别平台不一定发 created 事件，超时后也尝试摆位
    const fallback = window.setTimeout(() => {
      if (!readyRef.current) {
        readyRef.current = true;
        sync();
      }
    }, 800);

    const ro = new ResizeObserver(() => sync());
    ro.observe(host);
    const onResize = () => sync();
    window.addEventListener("resize", onResize);

    return () => {
      window.clearTimeout(fallback);
      ro.disconnect();
      window.removeEventListener("resize", onResize);
      readyRef.current = false;
      wvRef.current = null;
      wv.close().catch(() => {});
    };
  }, [sync, reloadKey]);

  // 激活态变化时立刻重新摆位（切回来/切走）
  useEffect(() => {
    sync();
  }, [active, sync]);

  return (
    <div className="tool-container ds-root">
      <div className="ds-bar">
        <span className="ds-title">DeepSeek 网页端</span>
        <button className="btn btn-ghost btn-sm" onClick={() => setReloadKey((k) => k + 1)}>
          刷新
        </button>
      </div>
      <div className="ds-host" ref={hostRef}>
        {failed ? (
          <div className="ds-placeholder">
            ⚠ 无法加载 DeepSeek 网页端，请检查网络后点「刷新」重试。
          </div>
        ) : (
          <div className="ds-placeholder">正在加载 DeepSeek…</div>
        )}
      </div>
    </div>
  );
}
