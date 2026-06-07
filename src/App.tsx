import { useState, useEffect } from "react";
import PortScanner from "./tools/PortScanner";
import ClaudeWatcher from "./tools/ClaudeWatcher";

interface ToolMeta {
  id: string;
  name: string;
  icon: string;
}

const HOME_ID = "home";

// 可作为 tab 打开的工具（首页另算，恒常驻）
const TOOLS: ToolMeta[] = [
  { id: "port-scanner", name: "端口查询", icon: "⚡" },
  { id: "claude-watcher", name: "Claude 自动授权", icon: "🤖" },
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

function renderTool(id: string, onOpen: (id: string) => void) {
  switch (id) {
    case HOME_ID:
      return <Home onOpen={onOpen} />;
    case "port-scanner":
      return <PortScanner />;
    case "claude-watcher":
      return <ClaudeWatcher />;
    default:
      return null;
  }
}

export default function App() {
  // 已打开的 tab（首页恒常驻，不可关闭）与当前激活的 tab
  const [openTabs, setOpenTabs] = useState<string[]>([HOME_ID]);
  const [activeTab, setActiveTab] = useState<string>(HOME_ID);
  // 右键菜单（针对某个 tab）与拖拽中的 tab
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(
    null
  );
  const [dragId, setDragId] = useState<string | null>(null);

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

  // 拖拽经过某个 tab 时实时交换顺序（首页固定在最左，不参与）
  const onDragOver = (e: React.DragEvent, overId: string) => {
    e.preventDefault();
    if (!dragId || dragId === overId || overId === HOME_ID) return;
    setOpenTabs((prev) => {
      const from = prev.indexOf(dragId);
      const to = prev.indexOf(overId);
      if (from < 0 || to < 0 || from === to) return prev;
      const next = [...prev];
      next.splice(from, 1);
      next.splice(to, 0, dragId);
      return next;
    });
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
                  className={`tab${active ? " active" : ""}${
                    dragId === id ? " dragging" : ""
                  }`}
                  onClick={() => setActiveTab(id)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    setMenu({ id, x: e.clientX, y: e.clientY });
                  }}
                  draggable={id !== HOME_ID}
                  onDragStart={() => id !== HOME_ID && setDragId(id)}
                  onDragOver={(e) => onDragOver(e, id)}
                  onDragEnd={() => setDragId(null)}
                  title={tabName(id)}
                >
                  <span className={`tab-dot${active ? " on" : ""}`} />
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

        {/* 已打开的工具全部保持挂载，非激活的用 CSS 隐藏，
            以便切换 tab 时状态（如 Claude 监听循环、日志）不丢失。
            关闭 tab 才会从 openTabs 移除并卸载。 */}
        <div className="tab-panes">
          {openTabs.map((id) => (
            <div
              key={id}
              className={`tool-pane${id === activeTab ? "" : " hidden"}`}
            >
              {renderTool(id, openTool)}
            </div>
          ))}
        </div>
      </main>
    </div>
  );
}
