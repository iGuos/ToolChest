import { openDeepSeek } from "./deepseek";

// DeepSeek 工具页：模拟官网首页的「二选一」入口。
// 点击卡片再打开对应站点（独立窗口，cookie 持久化保留登录态）。
export default function DeepSeek() {
  return (
    <div className="ds-landing">
      <div className="ds-main">
        <div className="ds-hero">
          <div className="ds-logo">deepseek</div>
          <div className="ds-slogan">探索未至之境</div>
        </div>

        <div className="ds-cards">
          <button className="ds-card" onClick={() => openDeepSeek("chat")}>
            <div className="ds-card-title">开始对话</div>
            <div className="ds-card-desc">
              与 DeepSeek 免费对话
              <br />
              体验全新旗舰模型
            </div>
          </button>
          <button className="ds-card" onClick={() => openDeepSeek("api")}>
            <div className="ds-card-title">API 开放平台</div>
            <div className="ds-card-desc">
              调用 DeepSeek 最新模型
              <br />
              快速集成、流畅体验
            </div>
          </button>
        </div>
      </div>
    </div>
  );
}
