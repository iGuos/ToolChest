import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { LanProvider } from "./tools/lanContext";
import "./styles.css";

// 屏蔽 webview 自带的右键菜单（Reload 等）。可编辑控件或已选中文本时放行，
// 以保留输入框粘贴、文本复制等系统菜单。
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement | null;
  const editable = t?.closest("input, textarea, [contenteditable='true']");
  const hasSelection = !!window.getSelection()?.toString();
  if (!editable && !hasSelection) e.preventDefault();
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <LanProvider>
      <App />
    </LanProvider>
  </React.StrictMode>
);
