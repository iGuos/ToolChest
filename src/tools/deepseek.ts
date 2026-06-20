import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { getCurrentWindow } from "@tauri-apps/api/window";

// DeepSeek 网页端走独立原生窗口（避免 macOS 上子 webview 叠加导致的光标闪烁）。
// 点侧栏/首页的 DeepSeek 即调用此函数，不占用 tab：
//   - 窗口已存在 → 显示并前置；
//   - 不存在 → 新建，并居中到「当前应用窗口」的中心（而非整个屏幕）。
// cookie 持久化，登录态会保留。
const DS_URL = "https://chat.deepseek.com";
const LABEL = "deepseek";
const W = 1040;
const H = 780;

export async function openDeepSeek() {
  try {
    const existing = await WebviewWindow.getByLabel(LABEL);
    if (existing) {
      await existing.show();
      await existing.unminimize();
      await existing.setFocus();
      return;
    }

    // 居中到当前窗口的中心：取当前窗口外框位置/尺寸（物理像素），除以缩放比换算成
    // 逻辑像素（窗口选项的 x/y/width/height 都用逻辑像素）。取不到时退回屏幕居中。
    let pos: { x: number; y: number } | undefined;
    try {
      const cur = getCurrentWindow();
      const [p, s, scale] = await Promise.all([
        cur.outerPosition(),
        cur.outerSize(),
        cur.scaleFactor(),
      ]);
      const winX = p.x / scale;
      const winY = p.y / scale;
      const winW = s.width / scale;
      const winH = s.height / scale;
      pos = {
        x: Math.round(winX + (winW - W) / 2),
        y: Math.round(winY + (winH - H) / 2),
      };
    } catch {
      /* 取不到当前窗口几何 → 用屏幕居中兜底 */
    }

    new WebviewWindow(LABEL, {
      url: DS_URL,
      title: "DeepSeek",
      width: W,
      height: H,
      ...(pos ? { x: pos.x, y: pos.y } : { center: true }),
    });
  } catch (e) {
    console.error("打开 DeepSeek 窗口失败", e);
  }
}
