import { useState, useEffect, useRef, useCallback } from "react";
import PortScanner from "./tools/PortScanner";
import HostsEditor from "./tools/HostsEditor";
import HttpClient from "./tools/HttpClient";
import TrustApp from "./tools/TrustApp";

interface ToolMeta {
  id: string;
  name: string;
  icon: string;
}

const HOME_ID = "home";

// 可作为 tab 打开的工具（首页另算，恒常驻）
const TOOLS: ToolMeta[] = [
  { id: "port-scanner", name: "端口查询", icon: "⚡" },
  { id: "http-client", name: "HTTP 请求测试", icon: "🌐" },
  { id: "hosts-editor", name: "Hosts 编辑器", icon: "📝" },
  { id: "trust-app", name: "Mac 应用授权", icon: "🔓" },
];

const tabName = (id: string) =>
  id === HOME_ID ? "首页" : TOOLS.find((t) => t.id === id)?.name ?? id;

function Home({ onOpen }: { onOpen: (id: string) => void }) {
  return (
    <div className="home">
      <div className="home-hero">
        <div className="home-logo">百宝箱</div>
        <div className="home-tagline">开发者工具箱 · 选择一个工具开始</div>
      </div>
      <div className="home-grid">
        {TOOLS.map((t) => (
          <button key={t.id} className="home-card" onClick={() => onOpen(t.id)}>
            <span className="home-card-icon">{t.icon}</span>
            <span className="home-card-name">{t.name}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

function renderTool(
  id: string,
  onOpen: (id: string) => void,
  onDirty: (id: string, dirty: boolean) => void
) {
  switch (id) {
    case HOME_ID:
      return <Home onOpen={onOpen} />;
    case "port-scanner":
      return <PortScanner />;
    case "http-client":
      return <HttpClient />;
    case "hosts-editor":
      return <HostsEditor onDirty={(d) => onDirty("hosts-editor", d)} />;
    case "trust-app":
      return <TrustApp />;
    default:
      return null;
  }
}

export default function App() {
  // 已打开的 tab（首页恒常驻，不可关闭）与当前激活的 tab
  const [openTabs, setOpenTabs] = useState<string[]>([HOME_ID]);
  const [activeTab, setActiveTab] = useState<string>(HOME_ID);
  // 右键菜单（针对某个 tab）
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(
    null
  );
  // 拖拽：drag 为正在跟随鼠标的卡片状态，dragRef 记录手势细节
  const [drag, setDrag] = useState<{ id: string; x: number; y: number } | null>(
    null
  );
  const dragRef = useRef<{
    id: string;
    startX: number;
    startY: number;
    active: boolean;
    pointerId: number;
  } | null>(null);
  // 哪些 tab 有未保存改动（用于 tab 上的变更指示灯）
  const [dirtyTabs, setDirtyTabs] = useState<Set<string>>(new Set());
  const markDirty = useCallback((id: string, dirty: boolean) => {
    setDirtyTabs((prev) => {
      if (dirty === prev.has(id)) return prev;
      const next = new Set(prev);
      if (dirty) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);

  // 打开一个工具：已开则只切换，未开则追加到末尾
  const openTool = (id: string) => {
    setOpenTabs((prev) => (prev.includes(id) ? prev : [...prev, id]));
    setActiveTab(id);
  };

  // 关闭 tab：若关闭的是当前激活项，则落到左侧相邻 tab
  const closeTab = (id: string) => {
    if (id === HOME_ID) return; // 首页不可关闭
    const idx = openTabs.indexOf(id);
    const next = openTabs.filter((t) => t !== id);
    setOpenTabs(next);
    markDirty(id, false); // 关闭即清掉它的未保存标记
    if (activeTab === id) {
      setActiveTab(next[idx - 1] ?? next[0] ?? HOME_ID);
    }
  };

  // 关闭其他：仅保留首页与目标 tab（目标为首页时只留首页）
  const closeOthers = (id: string) => {
    const keep = id === HOME_ID ? [HOME_ID] : [HOME_ID, id];
    setOpenTabs(keep);
    setActiveTab(id);
  };

  // 关闭全部：只留首页
  const closeAll = () => {
    setOpenTabs([HOME_ID]);
    setActiveTab(HOME_ID);
  };

  // 把 id 这个 tab 移动到 overId 所在位置（首页固定最左，不参与）
  const reorder = (id: string, overId: string) => {
    setOpenTabs((prev) => {
      const from = prev.indexOf(id);
      const to = prev.indexOf(overId);
      if (from < 0 || to < 0 || from === to) return prev;
      const next = [...prev];
      next.splice(from, 1);
      next.splice(to, 0, id);
      return next;
    });
  };

  // 卡片式拖拽（基于 pointer 事件，自定义跟随鼠标的浮层）
  const startDrag = (e: React.PointerEvent, id: string) => {
    if (id === HOME_ID || e.button !== 0) return;
    if ((e.target as HTMLElement).closest(".tab-close")) return; // 点的是关闭按钮
    dragRef.current = {
      id,
      startX: e.clientX,
      startY: e.clientY,
      active: false,
      pointerId: e.pointerId,
    };
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const moveDrag = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    // 超过阈值才算开始拖动，避免误触
    if (
      !d.active &&
      Math.abs(e.clientX - d.startX) < 5 &&
      Math.abs(e.clientY - d.startY) < 5
    )
      return;
    d.active = true;
    setDrag({ id: d.id, x: e.clientX, y: e.clientY });
    const overId = (
      document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null
    )?.closest<HTMLElement>("[data-tab-id]")?.dataset.tabId;
    if (overId && overId !== HOME_ID && overId !== d.id) reorder(d.id, overId);
  };

  const endDrag = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (d) {
      try {
        e.currentTarget.releasePointerCapture(d.pointerId);
      } catch {
        /* 捕获可能已释放 */
      }
    }
    dragRef.current = null;
    setDrag(null);
  };

  // 打开右键菜单后，点击空白处 / 滚动 / Esc 关闭它
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenu(null);
    };
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-header">
          <div className="logo">百宝箱</div>
          <div className="logo-sub">Developer Tools</div>
        </div>
        <nav className="sidebar-nav">
          <button
            className={`tool-btn${activeTab === HOME_ID ? " active" : ""}`}
            onClick={() => openTool(HOME_ID)}
          >
            <span className="tool-icon">🏠</span>
            <span className="tool-name">首页</span>
          </button>
          {TOOLS.map((tool) => (
            <button
              key={tool.id}
              className={`tool-btn${activeTab === tool.id ? " active" : ""}`}
              onClick={() => openTool(tool.id)}
            >
              <span className="tool-icon">{tool.icon}</span>
              <span className="tool-name">{tool.name}</span>
            </button>
          ))}
        </nav>
      </aside>

      <main className="content">
        {/* 顶部标签栏：每个已打开的工具一个 tab */}
        <div className="tab-bar">
          <div className="tabs">
            {openTabs.map((id) => {
              const active = id === activeTab;
              return (
                <div
                  key={id}
                  data-tab-id={id}
                  className={`tab${active ? " active" : ""}${
                    drag?.id === id ? " dragging" : ""
                  }${id !== HOME_ID ? " draggable" : ""}`}
                  onClick={() => setActiveTab(id)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    setMenu({ id, x: e.clientX, y: e.clientY });
                  }}
                  onPointerDown={(e) => startDrag(e, id)}
                  onPointerMove={moveDrag}
                  onPointerUp={endDrag}
                  onPointerCancel={endDrag}
                  title={tabName(id)}
                >
                  <span
                    className={`tab-dot${active ? " on" : ""}${
                      dirtyTabs.has(id) ? " dirty" : ""
                    }`}
                  />
                  <span className="tab-label">{tabName(id)}</span>
                  {id !== HOME_ID && (
                    <span
                      className="tab-close"
                      onClick={(e) => {
                        e.stopPropagation();
                        closeTab(id);
                      }}
                      title="关闭"
                    >
                      ×
                    </span>
                  )}
                </div>
              );
            })}
          </div>
        </div>

        {/* tab 右键菜单 */}
        {menu && (
          <div
            className="tab-menu"
            style={{ left: menu.x, top: menu.y }}
            onClick={(e) => e.stopPropagation()}
          >
            <button
              className="tab-menu-item"
              disabled={menu.id === HOME_ID}
              onClick={() => {
                closeTab(menu.id);
                setMenu(null);
              }}
            >
              关闭当前
            </button>
            <button
              className="tab-menu-item"
              disabled={openTabs.filter((t) => t !== HOME_ID && t !== menu.id).length === 0}
              onClick={() => {
                closeOthers(menu.id);
                setMenu(null);
              }}
            >
              关闭其他
            </button>
            <button
              className="tab-menu-item"
              disabled={openTabs.length <= 1}
              onClick={() => {
                closeAll();
                setMenu(null);
              }}
            >
              关闭全部
            </button>
          </div>
        )}

        {/* 拖拽时跟随鼠标的卡片 */}
        {drag && (
          <div
            className="tab-ghost"
            style={{ left: drag.x + 12, top: drag.y + 10 }}
          >
            <span className="tab-dot on" />
            <span className="tab-label">{tabName(drag.id)}</span>
          </div>
        )}

        {/* 已打开的工具全部保持挂载，非激活的用 CSS 隐藏，
            以便切换 tab 时状态（如 Claude 监听循环、日志）不丢失。
            关闭 tab 才会从 openTabs 移除并卸载。 */}
        <div className="tab-panes">
          {openTabs.map((id) => (
            <div
              key={id}
              className={`tool-pane${id === activeTab ? "" : " hidden"}`}
            >
              {renderTool(id, openTool, markDirty)}
            </div>
          ))}
        </div>
      </main>
    </div>
  );
}
