import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

// DeepSeek 网页端走独立原生窗口（避免 macOS 上子 webview 叠加导致的光标闪烁）。
// 点侧栏/首页的 DeepSeek 即调用此函数，不占用 tab：
//   - 窗口已存在 → 显示并前置；
//   - 不存在 → 新建。
// cookie 持久化，登录态会保留。
const DS_URL = "https://chat.deepseek.com";
const LABEL = "deepseek";

export async function openDeepSeek() {
  try {
    const existing = await WebviewWindow.getByLabel(LABEL);
    if (existing) {
      await existing.show();
      await existing.unminimize();
      await existing.setFocus();
      return;
    }
    new WebviewWindow(LABEL, {
      url: DS_URL,
      title: "DeepSeek",
      width: 1040,
      height: 780,
      center: true,
    });
  } catch (e) {
    console.error("打开 DeepSeek 窗口失败", e);
  }
}
