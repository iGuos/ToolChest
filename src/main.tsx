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

// macOS / iOS 的系统 WebView 默认让输入框「首字母大写 + 自动更正(弹建议框) + 拼写检查」。
// 这是系统文本输入行为，开发类输入(消息/IP/密码/端口/设备名/备注)都不需要——
// 这里给所有 input/textarea 设标准属性把它们关掉(不引入任何东西，只是禁用大写/更正)。
// 覆盖当前与后续动态渲染的所有 input/textarea。
function tameInput(node: Element) {
  if (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement) {
    node.setAttribute("autocapitalize", "off");
    node.setAttribute("autocorrect", "off");
    node.setAttribute("spellcheck", "false");
  }
}
function tameAll(root: ParentNode) {
  root.querySelectorAll("input, textarea").forEach(tameInput);
}
tameAll(document);
new MutationObserver((muts) => {
  for (const m of muts) {
    m.addedNodes.forEach((n) => {
      if (n instanceof Element) {
        tameInput(n);
        tameAll(n);
      }
    });
  }
}).observe(document.documentElement, { childList: true, subtree: true });

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <LanProvider>
      <App />
    </LanProvider>
  </React.StrictMode>
);
