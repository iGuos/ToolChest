import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

// DeepSeek 各页面走独立原生窗口（避免 macOS 上子 webview 叠加导致的光标闪烁）。
// 实际建窗在 Rust 端（open_deepseek 命令）：先隐藏 + 设主题背景色，页面加载完再显示，
// 以消除远程页面渲染前的白屏闪烁。这里负责算好「居中到当前窗口」的坐标和主题背景色。
// cookie 持久化，登录态会保留。
export type DeepSeekTarget = "chat" | "api";
const TARGETS: Record<DeepSeekTarget, { url: string; label: string; title: string }> = {
  chat: { url: "https://chat.deepseek.com", label: "deepseek", title: "DeepSeek 对话" },
  api: { url: "https://platform.deepseek.com", label: "deepseek-api", title: "DeepSeek API 开放平台" },
};
const W = 1040;
const H = 780;

// 连点拦截：同一个站点窗口正在打开时，忽略后续点击。
// 否则首次建窗尚未注册（get_webview_window 仍查不到），第二次点击会再建一个同名 label
// 的窗口而冲突报错。按 label 记录"正在打开"状态即可。
const opening = new Set<string>();

// 取当前主题背景色，作为新窗口的初始背景，避免远程页面渲染前先闪一下白底。
function currentBg(): string {
  try {
    const v = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim();
    return v || "#1e1e1e";
  } catch {
    return "#1e1e1e";
  }
}

export async function openDeepSeek(target: DeepSeekTarget = "chat") {
  const { url, label, title } = TARGETS[target];
  if (opening.has(label)) return; // 连点拦截：该窗口正在打开，忽略本次
  opening.add(label);

  // 居中到当前窗口中心：取当前窗口外框位置/尺寸（物理像素）/缩放比换算成逻辑像素。
  let x: number | undefined;
  let y: number | undefined;
  try {
    const cur = getCurrentWindow();
    const [p, s, scale] = await Promise.all([cur.outerPosition(), cur.outerSize(), cur.scaleFactor()]);
    const winX = p.x / scale;
    const winY = p.y / scale;
    const winW = s.width / scale;
    const winH = s.height / scale;
    x = Math.round(winX + (winW - W) / 2);
    y = Math.round(winY + (winH - H) / 2);
  } catch {
    /* 取不到当前窗口几何 → Rust 端退回屏幕居中 */
  }

  try {
    await invoke("open_deepseek", {
      label,
      url,
      title,
      width: W,
      height: H,
      x,
      y,
      bg: currentBg(),
    });
  } catch (e) {
    console.error("打开 DeepSeek 窗口失败", e);
  } finally {
    opening.delete(label);
  }
}
