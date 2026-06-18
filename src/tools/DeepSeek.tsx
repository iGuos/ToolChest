import { useCallback, useEffect, useState } from "react";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

// 套 DeepSeek 网页端，用「独立原生窗口」而不是 tab 内嵌的子 webview。
// 原因：在 macOS 上子 webview 叠在主界面之上，两层都处理 mousemove/光标，
// 系统反复重判，鼠标会在 箭头↔手、箭头↔I形 间狂闪（wry#175 / tauri#8770），
// 且无可靠纯前端解法。独立窗口是顶层窗口、下面没有别的 webview，光标行为正常。
const DS_URL = "https://chat.deepseek.com";
const LABEL = "deepseek";

type Status = "opening" | "open" | "error";

export default function DeepSeek() {
  const [status, setStatus] = useState<Status>("opening");
  const [err, setErr] = useState("");

  // recreate=true 用于「刷新」：先关掉旧窗口再开新的（cookie 持久化，不掉登录）
  const open = useCallback(async (recreate: boolean) => {
    setErr("");
    try {
      const existing = await WebviewWindow.getByLabel(LABEL);
      if (existing) {
        if (recreate) {
          await existing.close();
        } else {
          await existing.show();
          await existing.unminimize();
          await existing.setFocus();
          setStatus("open");
          return;
        }
      }
      setStatus("opening");
      const w = new WebviewWindow(LABEL, {
        url: DS_URL,
        title: "DeepSeek",
        width: 1040,
        height: 780,
        center: true,
      });
      w.once("tauri://created", () => setStatus("open"));
      w.once("tauri://error", (e) => {
        setErr(String((e as { payload?: unknown }).payload ?? e));
        setStatus("error");
      });
    } catch (e) {
      setErr(String(e));
      setStatus("error");
    }
  }, []);

  // 打开此 tab 即开窗；关闭此 tab 即关窗（窗口与标签绑定）
  useEffect(() => {
    open(false);
    return () => {
      WebviewWindow.getByLabel(LABEL)
        .then((w) => w?.close())
        .catch(() => {});
    };
  }, [open]);

  return (
    <div className="tool-container">
      <div className="tool-header">
        <h2>DeepSeek 网页端</h2>
        <div className="tool-actions">
          <button className="btn btn-ghost" onClick={() => open(false)}>
            显示窗口
          </button>
          <button className="btn btn-ghost" onClick={() => open(true)}>
            刷新
          </button>
        </div>
      </div>
      <div className="ds-host">
        <div className="ds-placeholder">
          {status === "error"
            ? `⚠ 打开 DeepSeek 窗口失败：${err}`
            : status === "opening"
            ? "正在打开 DeepSeek 窗口…"
            : "DeepSeek 已在独立窗口打开。被挡住时点「显示窗口」；关闭此标签会一并关闭该窗口。"}
        </div>
      </div>
    </div>
  );
}
