import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import PortScanner from "./tools/PortScanner";
import HostsEditor from "./tools/HostsEditor";
import HttpClient from "./tools/HttpClient";
import TrustApp from "./tools/TrustApp";
import TodoList from "./tools/TodoList";
import LanShare from "./tools/LanShare";
import LanIncomingModal from "./tools/LanIncomingModal";
import DeepSeek from "./tools/DeepSeekTab";
import ProxyTool from "./tools/ProxyTool";
import { useLan } from "./tools/lanContext";
import { useEscToClose, useDragReorder } from "./hooks";
import { isEnabled as autostartIsEnabled, enable as autostartEnable, disable as autostartDisable } from "@tauri-apps/plugin-autostart";

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
  { id: "todo", name: "待办事项", icon: "📋" },
  { id: "lan-share", name: "局域网互传", icon: "📡" },
  { id: "proxy", name: "请求代理", icon: "🔀" },
  { id: "deepseek", name: "DeepSeek", icon: "🤖" },
];

const tabName = (id: string) =>
  id === HOME_ID ? "首页" : TOOLS.find((t) => t.id === id)?.name ?? id;

// ── 应用设置（localStorage 持久化，结构预留以便后续扩展）──
const SETTINGS_KEY = "baibao.settings.v1";
type ThemeMode = "light" | "dark" | "system";
type CloseAction = "tray" | "quit";
interface Settings {
  hiddenTools: string[]; // 在侧栏/首页隐藏的工具 id
  order: string[]; // 工具自定义排序（id 顺序；不在此列的按 TOOLS 默认序追加）
  theme: ThemeMode; // 主题：浅色 / 深色 / 跟随系统
  closeAction: CloseAction; // 关闭按钮：关到托盘后台常驻 / 直接退出
}
const DEFAULT_SETTINGS: Settings = { hiddenTools: [], order: [], theme: "dark", closeAction: "tray" };
const CLOSE_OPTIONS: { id: CloseAction; name: string }[] = [
  { id: "tray", name: "最小化到托盘" },
  { id: "quit", name: "直接退出" },
];
const THEME_OPTIONS: { id: ThemeMode; name: string }[] = [
  { id: "light", name: "浅色" },
  { id: "dark", name: "深色" },
  { id: "system", name: "跟随系统" },
];

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (raw) return { ...DEFAULT_SETTINGS, ...JSON.parse(raw) };
  } catch {
    /* 损坏则回退默认 */
  }
  return DEFAULT_SETTINGS;
}

// 按设置里的 order 排好工具；order 里没有的（新加的工具）按 TOOLS 默认序追加到末尾
function orderTools(order: string[]): ToolMeta[] {
  const byId = new Map(TOOLS.map((t) => [t.id, t]));
  const result: ToolMeta[] = [];
  for (const id of order) {
    const t = byId.get(id);
    if (t) {
      result.push(t);
      byId.delete(id);
    }
  }
  for (const t of TOOLS) if (byId.has(t.id)) result.push(t);
  return result;
}

function Home({
  onOpen,
  tools,
}: {
  onOpen: (id: string) => void;
  tools: ToolMeta[];
}) {
  return (
    <div className="home">
      <div className="home-hero">
        <div className="home-logo">百宝箱</div>
        <div className="home-tagline">开发者工具箱 · 选择一个工具开始</div>
      </div>
      <div className="home-grid">
        {tools.map((t) => (
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
  onDirty: (id: string, dirty: boolean) => void,
  homeTools: ToolMeta[]
) {
  switch (id) {
    case HOME_ID:
      return <Home onOpen={onOpen} tools={homeTools} />;
    case "port-scanner":
      return <PortScanner />;
    case "http-client":
      return <HttpClient />;
    case "hosts-editor":
      return <HostsEditor onDirty={(d) => onDirty("hosts-editor", d)} />;
    case "trust-app":
      return <TrustApp />;
    case "todo":
      return <TodoList />;
    case "lan-share":
      return <LanShare />;
    case "deepseek":
      return <DeepSeek />;
    case "proxy":
      return <ProxyTool />;
    default:
      return null;
  }
}

export default function App() {
  const { totalUnread } = useLan(); // 局域网未读消息总数（用于侧栏红点）
  // 已打开的 tab（首页恒常驻，不可关闭）与当前激活的 tab
  const [openTabs, setOpenTabs] = useState<string[]>([HOME_ID]);
  const [activeTab, setActiveTab] = useState<string>(HOME_ID);
  // 右键菜单（针对某个 tab）
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(
    null
  );
  // 应用设置 + 设置弹框开关
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [settingsOpen, setSettingsOpen] = useState(false);
  useEffect(() => {
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    } catch {
      /* 忽略写入失败 */
    }
  }, [settings]);
  // 应用主题：跟随系统时解析 prefers-color-scheme，并监听系统切换
  useEffect(() => {
    const root = document.documentElement;
    const apply = () => {
      const t =
        settings.theme === "system"
          ? window.matchMedia("(prefers-color-scheme: dark)").matches
            ? "dark"
            : "light"
          : settings.theme;
      root.dataset.theme = t;
      root.style.background = t === "light" ? "#ffffff" : "#1e1e1e";
    };
    apply();
    if (settings.theme === "system") {
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      mq.addEventListener("change", apply);
      return () => mq.removeEventListener("change", apply);
    }
  }, [settings.theme]);
  // 同步「关闭按钮行为」到后端（启动时 + 变更时）：后端在窗口关闭事件读取此设置
  useEffect(() => {
    invoke("set_close_to_tray", { enabled: settings.closeAction !== "quit" }).catch(() => {});
  }, [settings.closeAction]);
  const hiddenTools = new Set(settings.hiddenTools);
  const orderedTools = orderTools(settings.order);
  const visibleTools = orderedTools.filter((t) => !hiddenTools.has(t.id));
  const toggleToolVisible = (id: string) =>
    setSettings((s) => {
      const hid = new Set(s.hiddenTools);
      if (hid.has(id)) hid.delete(id);
      else hid.add(id);
      return { ...s, hiddenTools: [...hid] };
    });
  // 把 fromId 这张卡拖到 overId 的位置
  const moveTool = (fromId: string, overId: string) =>
    setSettings((s) => {
      const ids = orderTools(s.order).map((t) => t.id);
      const from = ids.indexOf(fromId);
      const to = ids.indexOf(overId);
      if (from < 0 || to < 0 || from === to) return s;
      ids.splice(from, 1);
      ids.splice(to, 0, fromId);
      return { ...s, order: ids };
    });
  const resetSettings = () => setSettings({ ...DEFAULT_SETTINGS });
  useEscToClose(settingsOpen, () => setSettingsOpen(false));

  // 开机自启动：打开设置时检测当前状态，切换时调用插件 enable/disable（macOS / Windows 通用）
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartBusy, setAutostartBusy] = useState(false);
  useEffect(() => {
    if (!settingsOpen) return;
    autostartIsEnabled()
      .then(setAutostart)
      .catch(() => setAutostart(null));
  }, [settingsOpen]);
  const toggleAutostart = async (next: boolean) => {
    setAutostartBusy(true);
    try {
      if (next) await autostartEnable();
      else await autostartDisable();
      setAutostart(await autostartIsEnabled());
    } catch {
      // 失败时回读真实状态，避免开关与实际不一致
      autostartIsEnabled().then(setAutostart).catch(() => {});
    } finally {
      setAutostartBusy(false);
    }
  };

  // 功能菜单卡片拖拽排序（整张卡片可拖，点勾选框不触发拖拽）。复用通用拖拽 hook。
  const toolDnd = useDragReorder({
    selector: "[data-tool-card-id]",
    dataAttr: "toolCardId",
    ignoreTargetSelector: ".settings-card-check",
    onReorder: moveTool,
  });

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

  // 打开一个工具：已开则只切换，未开则追加到末尾。
  // DeepSeek 现在先开 tab（tab 内是官网式二选一入口），点卡片再开对应站点窗口。
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

  // 顶部 tab 拖拽排序（首页固定最左，不参与拖拽，也不可作为放置目标）。复用通用拖拽 hook。
  const tabDnd = useDragReorder({
    selector: "[data-tab-id]",
    dataAttr: "tabId",
    canDrag: (id) => id !== HOME_ID,
    canDropOn: (id) => id !== HOME_ID,
    ignoreTargetSelector: ".tab-close",
    onReorder: reorder,
  });

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
          {visibleTools.map((tool) => (
            <button
              key={tool.id}
              className={`tool-btn${activeTab === tool.id ? " active" : ""}`}
              onClick={() => openTool(tool.id)}
            >
              <span className="tool-icon">{tool.icon}</span>
              <span className="tool-name">{tool.name}</span>
              {tool.id === "lan-share" && totalUnread > 0 && (
                <span className="lan-badge nav">{totalUnread}</span>
              )}
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          <button className="tool-btn" onClick={() => setSettingsOpen(true)}>
            <span className="tool-icon">⚙️</span>
            <span className="tool-name">设置</span>
          </button>
        </div>
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
                    tabDnd.drag?.id === id ? " dragging" : ""
                  }${id !== HOME_ID ? " draggable" : ""}`}
                  onClick={() => setActiveTab(id)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    setMenu({ id, x: e.clientX, y: e.clientY });
                  }}
                  onPointerDown={(e) => tabDnd.onPointerDown(e, id)}
                  onPointerMove={tabDnd.onPointerMove}
                  onPointerUp={tabDnd.onPointerEnd}
                  onPointerCancel={tabDnd.onPointerEnd}
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
        {tabDnd.drag && (
          <div
            className="tab-ghost"
            style={{ left: tabDnd.drag.x + 12, top: tabDnd.drag.y + 10 }}
          >
            <span className="tab-dot on" />
            <span className="tab-label">{tabName(tabDnd.drag.id)}</span>
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
              {renderTool(id, openTool, markDirty, visibleTools)}
            </div>
          ))}
        </div>
      </main>

      {/* 设置弹框 */}
      {settingsOpen && (
        <div className="modal-overlay">
          <div className="modal">
            <div className="modal-head">
              <h3>设置</h3>
              <button className="modal-close" onClick={() => setSettingsOpen(false)}>×</button>
            </div>
            <div className="settings-section">
              <div className="settings-section-title">功能菜单</div>
              <div className="dim" style={{ fontSize: 12, marginBottom: 8 }}>
                勾选是否显示，拖拽卡片调整顺序（侧边栏与首页同步生效）
              </div>
              <div className="settings-cards">
                {orderedTools.map((t) => (
                  <div
                    key={t.id}
                    data-tool-card-id={t.id}
                    className={`settings-card${toolDnd.drag?.id === t.id ? " dragging" : ""}${
                      hiddenTools.has(t.id) ? " off" : ""
                    }`}
                    onPointerDown={(e) => toolDnd.onPointerDown(e, t.id)}
                    onPointerMove={toolDnd.onPointerMove}
                    onPointerUp={toolDnd.onPointerEnd}
                    onPointerCancel={toolDnd.onPointerEnd}
                    title="拖拽排序"
                  >
                    <input
                      className="settings-card-check"
                      type="checkbox"
                      checked={!hiddenTools.has(t.id)}
                      onChange={() => toggleToolVisible(t.id)}
                      onClick={(e) => e.stopPropagation()}
                    />
                    <span className="tool-icon">{t.icon}</span>
                    <span className="settings-card-name">{t.name}</span>
                  </div>
                ))}
              </div>
            </div>
            <div className="settings-section">
              <div className="settings-section-title">主题</div>
              <div className="theme-options">
                {THEME_OPTIONS.map((opt) => (
                  <button
                    key={opt.id}
                    className={`theme-opt${settings.theme === opt.id ? " active" : ""}`}
                    onClick={() => setSettings((s) => ({ ...s, theme: opt.id }))}
                  >
                    {opt.name}
                  </button>
                ))}
              </div>
            </div>
            <div className="settings-section">
              <div className="settings-section-title">通用</div>
              <div className="settings-field">
                <span>开机时自动启动</span>
                <label className="switch">
                  <input
                    type="checkbox"
                    checked={!!autostart}
                    disabled={autostart === null || autostartBusy}
                    onChange={(e) => toggleAutostart(e.target.checked)}
                  />
                  <span className="switch-slider" />
                </label>
              </div>
              <div className="settings-field">
                <div className="settings-field-label">
                  <span>点击关闭按钮时</span>
                  <span className="settings-field-hint">
                    {settings.closeAction === "quit"
                      ? "直接退出应用"
                      : "隐藏到系统托盘，后台继续运行（互传 / 代理不中断）"}
                  </span>
                </div>
                <div className="theme-options">
                  {CLOSE_OPTIONS.map((opt) => (
                    <button
                      key={opt.id}
                      className={`theme-opt${settings.closeAction === opt.id ? " active" : ""}`}
                      onClick={() => setSettings((s) => ({ ...s, closeAction: opt.id }))}
                    >
                      {opt.name}
                    </button>
                  ))}
                </div>
              </div>
            </div>
            <div className="modal-actions">
              <button className="btn btn-ghost settings-reset" onClick={resetSettings}>
                重置
              </button>
              <button className="btn btn-primary" onClick={() => setSettingsOpen(false)}>
                完成
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 功能菜单拖拽时跟随鼠标的卡片浮层 */}
      {toolDnd.drag &&
        (() => {
          const t = TOOLS.find((x) => x.id === toolDnd.drag!.id);
          return t ? (
            <div
              className="settings-card-ghost"
              style={{ left: toolDnd.drag.x + 12, top: toolDnd.drag.y + 10 }}
            >
              <span className="tool-icon">{t.icon}</span>
              <span className="settings-card-name">{t.name}</span>
            </div>
          ) : null;
        })()}

      {/* 局域网收件确认：全局渲染，任何 tab 下都可见 */}
      <LanIncomingModal />
    </div>
  );
}
