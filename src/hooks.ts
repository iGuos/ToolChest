import { useEffect, useRef, useState, type MutableRefObject, type PointerEvent } from "react";

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

export interface DragReorderOptions {
  selector: string; // 命中拖拽项的选择器，如 "[data-tab-id]"
  dataAttr: string; // 对应的 dataset 键（camelCase），如 "tabId"
  onReorder: (fromId: string, overId: string) => void;
  canDrag?: (id: string) => boolean; // 该项是否可拖（默认都可拖）
  canDropOn?: (overId: string) => boolean; // 是否可放到该项上（默认都可）
  ignoreTargetSelector?: string; // pointerdown 命中此选择器则不视为拖拽（如关闭按钮/勾选框）
  threshold?: number; // 触发拖拽的位移阈值（px），默认 5
}

export interface DragReorder {
  drag: { id: string; x: number; y: number } | null; // 当前拖拽项（用于渲染跟随鼠标的浮层）
  suppressNextClick: MutableRefObject<boolean>; // 拖拽后置 true，消费方可据此吞掉随之而来的 click
  onPointerDown: (e: PointerEvent, id: string) => void;
  onPointerMove: (e: PointerEvent) => void;
  onPointerEnd: (e: PointerEvent) => void; // 同时用于 onPointerUp / onPointerCancel
}

/**
 * 通用「指针拖拽排序」：超过阈值才算拖动，拖动时跟随鼠标显示浮层、悬停到目标项实时换位。
 * tab 排序、设置卡片排序、置顶设备排序共用同一套逻辑。
 */
export function useDragReorder(opts: DragReorderOptions): DragReorder {
  const { selector, dataAttr, onReorder, canDrag, canDropOn, ignoreTargetSelector } = opts;
  const threshold = opts.threshold ?? 5;
  const [drag, setDrag] = useState<{ id: string; x: number; y: number } | null>(null);
  const ref = useRef<{
    id: string;
    startX: number;
    startY: number;
    active: boolean;
    pointerId: number;
  } | null>(null);
  const suppressNextClick = useRef(false);

  const onPointerDown = (e: PointerEvent, id: string) => {
    if (e.button !== 0) return;
    if (canDrag && !canDrag(id)) return;
    if (ignoreTargetSelector && (e.target as HTMLElement).closest(ignoreTargetSelector)) return;
    suppressNextClick.current = false; // 清掉上一次可能残留的抑制标志，避免误吞下一次点击
    ref.current = { id, startX: e.clientX, startY: e.clientY, active: false, pointerId: e.pointerId };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent) => {
    const d = ref.current;
    if (!d) return;
    if (!d.active && Math.abs(e.clientX - d.startX) < threshold && Math.abs(e.clientY - d.startY) < threshold)
      return;
    d.active = true;
    setDrag({ id: d.id, x: e.clientX, y: e.clientY });
    const overId = (document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null)?.closest<HTMLElement>(
      selector
    )?.dataset[dataAttr];
    if (overId && overId !== d.id && (!canDropOn || canDropOn(overId))) onReorder(d.id, overId);
  };
  const onPointerEnd = (e: PointerEvent) => {
    const d = ref.current;
    if (d) {
      if (d.active) suppressNextClick.current = true; // 这次是拖拽，消费方应吞掉随后的 click
      try {
        e.currentTarget.releasePointerCapture(d.pointerId);
      } catch {
        /* 捕获可能已释放 */
      }
    }
    ref.current = null;
    setDrag(null);
  };
  return { drag, suppressNextClick, onPointerDown, onPointerMove, onPointerEnd };
}
