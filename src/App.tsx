import { useState } from "react";
import PortScanner from "./tools/PortScanner";
import ClaudeWatcher from "./tools/ClaudeWatcher";

interface Tool {
  id: string;
  name: string;
  icon: string;
  component: React.ReactNode;
}

export default function App() {
  const [activeTool, setActiveTool] = useState("port-scanner");

  const tools: Tool[] = [
    {
      id: "port-scanner",
      name: "端口查询",
      icon: "⚡",
      component: <PortScanner />,
    },
    {
      id: "claude-watcher",
      name: "Claude 自动授权",
      icon: "🤖",
      component: <ClaudeWatcher />,
    },
  ];

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-header">
          <div className="logo">百宝箱</div>
          <div className="logo-sub">Developer Tools</div>
        </div>
        <nav className="sidebar-nav">
          {tools.map((tool) => (
            <button
              key={tool.id}
              className={`tool-btn${activeTool === tool.id ? " active" : ""}`}
              onClick={() => setActiveTool(tool.id)}
            >
              <span className="tool-icon">{tool.icon}</span>
              <span className="tool-name">{tool.name}</span>
            </button>
          ))}
        </nav>
      </aside>
      <main className="content">
        {/* All tools stay mounted; inactive ones are hidden via CSS so their
            state (e.g. the Claude watcher's monitoring loop & logs) survives
            tab switches instead of being unmounted/reset. */}
        {tools.map((tool) => (
          <div
            key={tool.id}
            className={`tool-pane${activeTool === tool.id ? "" : " hidden"}`}
          >
            {tool.component}
          </div>
        ))}
      </main>
    </div>
  );
}
