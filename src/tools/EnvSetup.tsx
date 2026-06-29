import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Plat = "ios" | "android";
interface EnvItem {
  key: string;
  name: string;
  ok: boolean;
  detail: string;
  fixKind: "none" | "auto" | "copy" | "link";
  fixValue: string;
  fixLabel: string;
}

const IS_MAC = /Mac/i.test(navigator.userAgent);

export default function EnvSetup() {
  const [plat, setPlat] = useState<Plat>(IS_MAC ? "ios" : "android");
  const [items, setItems] = useState<EnvItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [log, setLog] = useState<{ key: string; ok: boolean; text: string } | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  const check = useCallback(async (p: Plat) => {
    setLoading(true);
    setLog(null);
    try {
      setItems(await invoke<EnvItem[]>("env_check", { platform: p }));
    } catch {
      setItems([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    check(plat);
  }, [plat, check]);

  const runFix = async (it: EnvItem) => {
    if (it.fixKind === "auto") {
      setBusyKey(it.key);
      setLog(null);
      try {
        const out = await invoke<string>("env_fix", { key: it.key });
        setLog({ key: it.key, ok: true, text: out });
        await check(plat);
      } catch (e) {
        setLog({ key: it.key, ok: false, text: String(e) });
      } finally {
        setBusyKey(null);
      }
    } else if (it.fixKind === "link") {
      invoke("open_external", { url: it.fixValue }).catch(() => {});
    } else if (it.fixKind === "copy") {
      navigator.clipboard?.writeText(it.fixValue).catch(() => {});
      setCopied(it.key);
      window.setTimeout(() => setCopied((c) => (c === it.key ? null : c)), 1500);
    }
  };

  const readyCount = items.filter((i) => i.ok).length;

  return (
    <div className="tool-container">
      <div className="tool-header">
        <div>
          <h2>环境配置</h2>
          <p className="tool-subtitle">
            {IS_MAC ? "检测并一键准备 iOS / Android 的打包环境。" : "检测并一键准备 Android 的打包环境。"}
          </p>
        </div>
      </div>

      <div className="env-body">
        <div className="env-tabs">
          {/* iOS 只能在 macOS 构建：非 mac 不展示 iOS Tab，直接只看 Android */}
          {IS_MAC && (
            <>
              <button className={`env-tab${plat === "ios" ? " active" : ""}`} onClick={() => setPlat("ios")}>
                 iOS
              </button>
              <button
                className={`env-tab${plat === "android" ? " active" : ""}`}
                onClick={() => setPlat("android")}
              >
                🤖 Android
              </button>
            </>
          )}
          {!IS_MAC && <span className="env-tab active" style={{ cursor: "default" }}>🤖 Android</span>}
          <button className="btn btn-ghost btn-sm env-recheck" onClick={() => check(plat)} disabled={loading}>
            {loading ? "检测中…" : "重新检测"}
          </button>
        </div>

        {!loading && (
          <div className="env-summary dim">
            {readyCount}/{items.length} 项就绪{readyCount === items.length && items.length > 0 ? " · 环境已齐备 ✅" : ""}
          </div>
        )}

        <ul className="env-list">
          {items.map((it) => (
            <li key={it.key} className={`env-item${it.ok ? " ok" : ""}`}>
              <span className={`env-dot${it.ok ? " on" : ""}`} />
              <div className="env-item-main">
                <div className="env-item-name">{it.name}</div>
                <div className="env-item-detail dim">{it.detail}</div>
                {log && log.key === it.key && (
                  <pre className={`env-log${log.ok ? " ok" : " err"}`}>{log.text}</pre>
                )}
              </div>
              {!it.ok && it.fixKind !== "none" && (
                <button
                  className="btn btn-sm btn-primary env-fix"
                  onClick={() => runFix(it)}
                  disabled={busyKey === it.key}
                >
                  {busyKey === it.key
                    ? "安装中…"
                    : copied === it.key
                    ? "已复制"
                    : it.fixLabel || (it.fixKind === "auto" ? "一键安装" : it.fixKind === "link" ? "打开" : "复制")}
                </button>
              )}
              {it.ok && <span className="env-ok-tag">已就绪</span>}
            </li>
          ))}
        </ul>

        <div className="env-note dim">
          <p>· 安全项(Rust 目标、CocoaPods、命令行工具)可「一键安装」;Xcode / Android Studio / NDK 体积大,点链接去官方下载。</p>
          <p>· 全部就绪后,用 <code>pnpm install:ios</code> / <code>pnpm install:android</code> 一键打包装机。</p>
          <p>· 首次还需各跑一次 <code>pnpm tauri ios init</code> / <code>pnpm tauri android init</code> 生成原生工程。</p>
        </div>
      </div>
    </div>
  );
}
