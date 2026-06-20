import { useEffect } from "react";

/**
 * 弹框统一交互：按 Esc 关闭。
 * 配合「点遮罩层不关闭，只认 × 和 Esc」的约定使用——遮罩层不再绑定关闭点击。
 */
export function useEscToClose(active: boolean, onClose: () => void) {
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, onClose]);
}
