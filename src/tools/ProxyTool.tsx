import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useLan } from "./lanContext";

type Opt = { value: string; label: string; online?: boolean };

// 跟随主题的自定义下拉（替代原生 select，原生在深色主题下样式不可控）。
// 支持键盘上下键导航 + 回车选择 + Esc 关闭，并可显示在线状态小圆点。
function Dropdown({
  value,
  options,
  placeholder,
  emptyHint,
  onChange,
}: {
  value: string;
  options: Opt[];
  placeholder: string;
  emptyHint?: string;
  onChange: (v: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1); // 键盘高亮项索引
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [open]);
  // 打开时把高亮定位到当前选中项
  useEffect(() => {
    if (open) setActive(Math.max(0, options.findIndex((o) => o.value === value)));
  }, [open]); // eslint-disable-line react-hooks/exhaustive-deps

  const sel = options.find((o) => o.value === value);
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") return setOpen(false);
    if (!open && (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      return setOpen(true);
    }
    if (!open) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(options.length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const o = options[active];
      if (o) {
        onChange(o.value);
        setOpen(false);
      }
    }
  };
  return (
    <div className={`dropdown${open ? " open" : ""}`} ref={ref}>
      <button type="button" className="dropdown-btn" onClick={() => setOpen((o) => !o)} onKeyDown={onKey}>
        <span className={sel ? "dropdown-val" : "dim"}>
          {sel && sel.online !== undefined && (
            <span className={`peer-dot${sel.online ? " on" : ""}`} />
          )}
          {sel ? sel.label : placeholder}
        </span>
        <svg className="dropdown-caret" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
          <polyline points="6 9 12 15 18 9" />
        </svg>
      </button>
      {open && (
        <div className="dropdown-menu">
          {options.length === 0 ? (
            <div className="dropdown-empty dim">{emptyHint ?? "暂无选项"}</div>
          ) : (
            options.map((o, i) => (
              <button
                type="button"
                key={o.value}
                className={`dropdown-item${o.value === value ? " sel" : ""}${i === active ? " active" : ""}`}
                onMouseEnter={() => setActive(i)}
                onClick={() => {
                  onChange(o.value);
                  setOpen(false);
                }}
              >
                {o.online !== undefined && <span className={`peer-dot${o.online ? " on" : ""}`} />}
                {o.label}
              </button>
            ))
          )}
        </div>
      )}
    </div>
  );
}

// 可点击复制的代码块：点一下复制内容，并就地闪现「已复制」。
function Copyable({ text, title }: { text: string; title?: string }) {
  const [done, setDone] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand("copy");
      } catch {
        /* ignore */
      }
      document.body.removeChild(ta);
    }
    setDone(true);
    window.setTimeout(() => setDone(false), 1200);
  };
  return (
    <button type="button" className={`device-code copyable${done ? " copied" : ""}`} title={title ?? "点击复制"} onClick={copy}>
      <span>{text}</span>
      {done ? (
        <span className="copy-flag">已复制</span>
      ) : (
        <svg className="copy-ic" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
          <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
        </svg>
      )}
    </button>
  );
}

type Msg = { text: string; kind: "err" | "ok" | "info" };
type ProxyStatus = { role: number; socksPort: number; httpPort: number; port: number; conns: number; bytes: number };
type HostHit = { target: string; count: number; lastMs: number };

// 人类可读的字节/速率
function fmtBytes(n: number): string {
  if (n < 1024) return `${Math.round(n)} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`;
}

// 流量折线图（实时速率采样）。纯 SVG，无依赖。
function Sparkline({ data }: { data: number[] }) {
  const W = 460;
  const H = 90;
  const max = Math.max(1, ...data);
  const n = Math.max(data.length, 2);
  const pts = data.map((v, i) => {
    const x = (i / (n - 1)) * W;
    const y = H - (v / max) * (H - 8) - 2;
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  });
  const area = pts.length ? `0,${H} ${pts.join(" ")} ${((data.length - 1) / (n - 1) * W).toFixed(1)},${H}` : "";
  return (
    <svg className="proxy-chart" viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none">
      {data.length >= 2 && (
        <>
          <polygon points={area} className="proxy-chart-area" />
          <polyline points={pts.join(" ")} className="proxy-chart-line" />
        </>
      )}
    </svg>
  );
}

// 访问密码持久化：服务端记一个；客户端按「服务端 fingerprint」各记一个，
// 这样停止/切换/重开后能自动回填上次用过的密码，免重复输入。
const PW_KEY = "baibao.proxy.pw.v1";
interface PwStore {
  server: string;
  client: Record<string, string>;
}
function loadPwStore(): PwStore {
  try {
    const o = JSON.parse(localStorage.getItem(PW_KEY) || "{}");
    return {
      server: typeof o.server === "string" ? o.server : "",
      client: o.client && typeof o.client === "object" ? o.client : {},
    };
  } catch {
    return { server: "", client: {} };
  }
}

// 请求代理（独立功能）：让没装 VPN 的设备经另一台「已联 VPN」的设备访问内网。
export default function ProxyTool() {
  const { peers } = useLan();
  const pwStore = useRef<PwStore>(loadPwStore());
  const [proxy, setProxy] = useState<ProxyStatus>({ role: 0, socksPort: 0, httpPort: 0, port: 0, conns: 0, bytes: 0 });
  const [serverView, setServerView] = useState<"traffic" | "hosts">("traffic"); // 服务端视图：流量折线 / 访问网址
  const [samples, setSamples] = useState<number[]>([]); // 实时速率采样(字节/秒)，画折线
  const [hosts, setHosts] = useState<HostHit[]>([]); // 访问过的目标列表
  const lastBytesRef = useRef<{ bytes: number; t: number } | null>(null);
  const [mode, setMode] = useState<"server" | "client">("server");
  const [pw, setPw] = useState(() => pwStore.current.server); // 默认服务端，回填上次服务端密码
  const [port, setPort] = useState("53318");
  const [serverFp, setServerFp] = useState("");
  const [src, setSrc] = useState<"lan" | "custom">("lan"); // 客户端找服务端的方式：内网网段 / 自定义 IP
  const [subnets, setSubnets] = useState<Opt[]>([]); // 本机网卡推导出的可选 /24 网段
  const [subnet, setSubnet] = useState(""); // 选中的网段前缀，如 192.168.1
  const [customIp, setCustomIp] = useState(""); // 自定义服务端 IP
  const [wantIp, setWantIp] = useState(""); // 自定义探测中：待回填 fingerprint 的 IP
  const [busy, setBusy] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [scanProg, setScanProg] = useState<{ done: number; total: number } | null>(null);
  const [msg, setMsg] = useState<Msg | null>(null);
  const [shake, setShake] = useState(0); // 自增触发错误提示抖动动画
  const [pwVisible, setPwVisible] = useState(false);
  const [sysProxy, setSysProxy] = useState(false); // 是否已接管系统代理(仅客户端)
  const [sysBusy, setSysBusy] = useState(false); // 系统代理设置中（弹授权框，耗时）
  const [apply, setApply] = useState<"sys" | "app">("app"); // 代理生效方式：默认指定应用，可切到系统代理
  const [appPickMode, setAppPickMode] = useState<"file" | "running">("file"); // 指定应用：选文件 / 选运行中
  const [runningApps, setRunningApps] = useState<{ name: string; path: string }[]>([]);
  const [loadingApps, setLoadingApps] = useState(false);
  const [launching, setLaunching] = useState(false); // 正在带代理启动应用
  const [launched, setLaunched] = useState<string[]>([]); // 最近通过代理启动的应用
  const msgTimer = useRef<number | null>(null);

  // 统一出消息：err 常驻(可手动关)，ok/info 4 秒后自动消失。
  const show = (text: string, kind: Msg["kind"]) => {
    if (msgTimer.current) window.clearTimeout(msgTimer.current);
    setMsg({ text, kind });
    if (kind === "err") setShake((s) => s + 1);
    else msgTimer.current = window.setTimeout(() => setMsg(null), 4000);
  };
  const fail = (text: string) => show(text, "err");
  useEffect(() => () => {
    if (msgTimer.current) window.clearTimeout(msgTimer.current);
  }, []);

  // 写入持久化（按当前角色/所选服务端归位）
  const persistPw = (val: string) => {
    const s = pwStore.current;
    if (mode === "server") s.server = val;
    else if (serverFp) s.client[serverFp] = val;
    try {
      localStorage.setItem(PW_KEY, JSON.stringify(s));
    } catch {
      /* ignore */
    }
  };
  // 用户编辑密码：更新输入并持久化
  const updatePw = (val: string) => {
    setPw(val);
    persistPw(val);
  };
  // 取某角色/服务端已保存的密码
  const savedPw = (m: "server" | "client", fp: string) =>
    m === "server" ? pwStore.current.server : fp ? pwStore.current.client[fp] ?? "" : "";

  // 选择服务端设备：回填该服务端上次用过的密码
  const selectServer = (fp: string) => {
    setServerFp(fp);
    setPw(savedPw("client", fp));
  };

  // 切换角色：回填该角色上次用过的密码（服务端=你设的、客户端=对应服务端的）
  const switchMode = (m: "server" | "client") => {
    setMode(m);
    setPw(savedPw(m, serverFp));
    setPwVisible(false);
    setMsg(null);
  };

  // 生成 21 位随机密码（大小写字母 + 数字，区分大小写），并显示出来方便告知客户端
  const genPw = () => {
    const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    const a = new Uint32Array(21);
    crypto.getRandomValues(a);
    updatePw(Array.from(a, (n) => chars[n % chars.length]).join(""));
    setPwVisible(true);
  };

  const refresh = async () => {
    try {
      setProxy(await invoke<ProxyStatus>("lan_proxy_status"));
    } catch {
      /* ignore */
    }
  };
  useEffect(() => {
    refresh();
  }, []);

  // 运行中：每 1 秒取状态；服务端据累计字节算实时速率，采样画折线
  useEffect(() => {
    if (proxy.role === 0) {
      setSamples([]);
      lastBytesRef.current = null;
      return;
    }
    lastBytesRef.current = null; // 重新建立基准
    setSamples([]);
    const tick = async () => {
      try {
        const s = await invoke<ProxyStatus>("lan_proxy_status");
        setProxy(s);
        const now = Date.now();
        const prev = lastBytesRef.current;
        lastBytesRef.current = { bytes: s.bytes, t: now };
        if (prev && s.role === 1) {
          const dt = (now - prev.t) / 1000;
          const rate = dt > 0 ? Math.max(0, (s.bytes - prev.bytes) / dt) : 0;
          setSamples((arr) => [...arr, rate].slice(-60));
        }
      } catch {
        /* ignore */
      }
    };
    const t = window.setInterval(tick, 1000);
    return () => window.clearInterval(t);
  }, [proxy.role]);

  // 服务端「网址」视图：每 2 秒拉一次访问目标列表
  useEffect(() => {
    if (proxy.role !== 1 || serverView !== "hosts") return;
    const load = () => invoke<HostHit[]>("lan_proxy_hosts").then(setHosts).catch(() => {});
    load();
    const t = window.setInterval(load, 2000);
    return () => window.clearInterval(t);
  }, [proxy.role, serverView]);

  // 扫描进度事件
  useEffect(() => {
    let un: (() => void) | undefined;
    listen<{ done: number; total: number }>("lan://scan-progress", (e) => setScanProg(e.payload)).then((u) => (un = u));
    return () => un?.();
  }, []);

  // 载入本机网卡，推导可选 /24 网段（按前缀去重；VPN/虚拟网卡标注）
  useEffect(() => {
    invoke<{ ip: string; name: string; isVpn: boolean }[]>("lan_interfaces")
      .then((ifs) => {
        const seen = new Set<string>();
        const opts: Opt[] = [];
        for (const i of ifs) {
          const p = i.ip.split(".").slice(0, 3).join(".");
          if (seen.has(p)) continue;
          seen.add(p);
          opts.push({ value: p, label: `${p}.x（${i.isVpn ? "VPN/虚拟" : i.name}）` });
        }
        setSubnets(opts);
        setSubnet((cur) => cur || (opts[0]?.value ?? ""));
      })
      .catch(() => {});
  }, []);

  // 自定义探测成功后：peers 更新时回填对应设备的 fingerprint 并自动选中
  useEffect(() => {
    if (!wantIp) return;
    const p = peers.find((x) => x.ip === wantIp);
    if (p) {
      setServerFp(p.fingerprint);
      // 自定义探测：保留已输入的密码；为空则回填该服务端上次用过的
      setPw((cur) => cur || (pwStore.current.client[p.fingerprint] ?? ""));
      setWantIp("");
    }
  }, [peers, wantIp]);

  // 只扫选中的那个 /24 网段，避免全量扫描的压力
  const scan = async () => {
    if (scanning || !subnet) return;
    setScanning(true);
    setScanProg(null);
    setMsg(null);
    try {
      const n = await invoke<number>("lan_scan_subnet", { prefix: subnet });
      show(n > 0 ? `扫描完成，本网段发现 ${n} 台设备` : "扫描完成，本网段未发现设备", n > 0 ? "ok" : "info");
    } catch (e) {
      fail(`扫描失败：${String(e)}`);
    } finally {
      setScanning(false);
      setScanProg(null);
    }
  };

  // 切换「来源」：清掉已选服务端，避免跨来源串台
  const switchSrc = (s: "lan" | "custom") => {
    setSrc(s);
    setServerFp("");
    setWantIp("");
    setMsg(null);
  };

  // 自定义 IP：探测 /info 确认是百宝箱设备并登记（拿到证书指纹），随后自动选中
  const addCustom = async () => {
    const ip = customIp.trim();
    if (!/^(\d{1,3}\.){3}\d{1,3}$/.test(ip)) return fail("请输入正确的服务端 IP，如 192.168.1.20");
    setBusy(true);
    setMsg(null);
    try {
      await invoke("lan_add_peer", { ip, port: null });
      setWantIp(ip);
      show(`已找到 ${ip} 的设备，可直接连接`, "ok");
    } catch (e) {
      fail(String(e));
    } finally {
      setBusy(false);
    }
  };

  const start = async () => {
    if (!pw.trim()) return fail("请填写访问密码");
    if (mode === "client" && !serverFp)
      return fail(src === "lan" ? "请先扫描并选择服务端设备" : "请先填 IP 并点「连接」");
    const portNum = Number(port);
    if (!Number.isInteger(portNum) || portNum < 1 || portNum > 65535) return fail("端口需为 1–65535");
    setBusy(true);
    setMsg(null);
    try {
      if (mode === "server") {
        await invoke("lan_proxy_start_server", { password: pw, port: portNum });
        await refresh();
        show(`服务端已开启 · 隧道端口 ${portNum}，把访问密码告诉客户端即可`, "ok");
      } else {
        // 客户端先做连通性自检（可达 + 证书匹配 + 密码正确），不通不开启
        await invoke("lan_proxy_test", { fingerprint: serverFp, password: pw, port: portNum });
        const sp = await invoke<number>("lan_proxy_start_client", { fingerprint: serverFp, password: pw, port: portNum });
        await refresh();
        show(`已连接服务端 · 本地 SOCKS5 127.0.0.1:${sp}`, "ok");
      }
      persistPw(pw); // 连接成功后记住本次密码（停止后仍保留，免重输）
    } catch (e) {
      fail(String(e));
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  // 设置/还原系统代理，返回是否成功（失败含用户取消授权）
  const applySys = async (on: boolean): Promise<boolean> => {
    setSysBusy(true);
    try {
      await invoke("lan_set_system_proxy", {
        enable: on,
        socksPort: proxy.socksPort || 53319,
        httpPort: proxy.httpPort || 53320,
      });
      setSysProxy(on);
      show(on ? "已设为系统代理；多数程序自动走，停止/切回指定应用时自动还原。" : "已还原系统代理。", "ok");
      return true;
    } catch (e) {
      fail(String(e));
      return false;
    } finally {
      setSysBusy(false);
    }
  };

  // 切换生效方式：选「系统代理」即刻接管系统代理；切回「指定应用」则还原。
  const chooseApply = async (next: "sys" | "app") => {
    if (next === apply || sysBusy) return;
    if (next === "sys") {
      setApply("sys");
      const ok = await applySys(true); // macOS 这步会弹一次授权
      if (!ok) setApply("app"); // 失败/取消则回退
    } else {
      setApply("app");
      if (sysProxy) await applySys(false); // 切回指定应用：还原系统代理
    }
  };

  // 带代理启动指定路径的应用
  const launchPath = async (path: string) => {
    if (launching || !path) return;
    setLaunching(true);
    try {
      const r = await invoke<string>("lan_proxy_launch_app", { path, port: proxy.socksPort || 53319 });
      setLaunched((l) => [r, ...l.filter((x) => x !== r)].slice(0, 5));
      show(r, "ok");
    } catch (e) {
      fail(String(e));
    } finally {
      setLaunching(false);
    }
  };

  // 方式一：从文件选择器挑一个 App
  const pickAndLaunch = async () => {
    if (launching) return;
    try {
      const path = await invoke<string | null>("lan_proxy_pick_app");
      if (path) await launchPath(path);
    } catch (e) {
      fail(String(e));
    }
  };

  // 方式二：列出当前运行中的有界面应用，直接挑选
  const loadRunningApps = async () => {
    if (loadingApps) return;
    setLoadingApps(true);
    try {
      setRunningApps(await invoke<{ name: string; path: string }[]>("lan_proxy_running_apps"));
    } catch (e) {
      fail(String(e));
    } finally {
      setLoadingApps(false);
    }
  };

  const stop = async () => {
    setBusy(true);
    try {
      if (sysProxy) {
        // 停止前先还原系统代理，避免系统代理指向已关闭的端口导致断网
        try {
          await invoke("lan_set_system_proxy", {
            enable: false,
            socksPort: proxy.socksPort || 53319,
            httpPort: proxy.httpPort || 53320,
          });
        } catch {
          /* 还原失败也继续停止 */
        }
        setSysProxy(false);
      }
      setApply("app"); // 复位为默认，下次连接从「指定应用」开始
      await invoke("lan_proxy_stop");
      show("已停止代理。", "info");
    } finally {
      await refresh();
      setBusy(false);
    }
  };

  // 列出所有已发现/已知设备（不按网段过滤：多归属设备的上报 IP 可能不在所扫网段，
  // 过滤会导致「选中后下拉又空了」）。网段只用于触发扫描，结果一律保留可选。
  const deviceOptions: Opt[] = peers.map((p) => ({
    value: p.fingerprint,
    label: `${p.remark || p.alias}（${p.ip}）`,
    online: !!p.online,
  }));
  const selectedPeer = peers.find((p) => p.fingerprint === serverFp);

  const onPwKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !busy) start();
  };

  return (
    <div className="tool-container">
      <div className="tool-header">
        <div>
          <h2>请求代理</h2>
          <p className="tool-subtitle">经一台「已联 VPN」的设备代理访问内网。</p>
        </div>
      </div>

      <div className="proxy-tool-body">
        {proxy.role !== 0 ? (
          <div className="proxy-card proxy-status">
            <span className="proxy-badge on">
              运行中
              {proxy.conns > 0 && <em className="proxy-conns">{proxy.conns} 个活跃连接</em>}
            </span>
            {proxy.role === 1 ? (
              <>
                <p>
                  正作为<b>服务端</b>运行 · 隧道端口 <Copyable text={String(proxy.port || 53318)} title="复制端口" />
                  <br />
                  客户端凭访问密码连接本机。
                </p>
                <div className="proxy-apply">
                  <div className="proxy-apply-seg proxy-sub-seg">
                    <button
                      type="button"
                      className={`proxy-src${serverView === "traffic" ? " active" : ""}`}
                      onClick={() => setServerView("traffic")}
                    >
                      流量
                    </button>
                    <button
                      type="button"
                      className={`proxy-src${serverView === "hosts" ? " active" : ""}`}
                      onClick={() => setServerView("hosts")}
                    >
                      访问网址
                    </button>
                  </div>
                  {serverView === "traffic" ? (
                    <div className="proxy-traffic">
                      <Sparkline data={samples} />
                      <div className="proxy-traffic-stat">
                        <span>实时 <b>{fmtBytes(samples[samples.length - 1] ?? 0)}/s</b></span>
                        <span>累计 <b>{fmtBytes(proxy.bytes)}</b></span>
                        <span>活跃 <b>{proxy.conns}</b></span>
                      </div>
                    </div>
                  ) : (
                    <div className="proxy-running">
                      <div className="proxy-running-head">
                        <span className="dim">访问目标（{hosts.length}）· 仅记录 host:port，不查看内容</span>
                      </div>
                      <ul className="proxy-hostlog">
                        {hosts.map((h) => (
                          <li key={h.target}>
                            <span className="proxy-host-target" title={h.target}>{h.target}</span>
                            <span className="proxy-host-count">×{h.count}</span>
                          </li>
                        ))}
                        {hosts.length === 0 && <li className="dropdown-empty dim">暂无访问记录</li>}
                      </ul>
                    </div>
                  )}
                </div>
              </>
            ) : (
              <p className="proxy-socks-line">
                正作为<b>客户端</b>运行 · 手动配置代理可用：
                <br />
                <span className="dim">HTTP </span>
                <Copyable text={`127.0.0.1:${proxy.httpPort || 53320}`} title="复制 HTTP 代理地址" />
                <span className="dim">　SOCKS5 </span>
                <Copyable text={`127.0.0.1:${proxy.socksPort}`} title="复制 SOCKS5 地址" />
                <br />
                <span className="dim">域名由服务端解析（远程 DNS），适合被污染的站点。</span>
              </p>
            )}
            {proxy.role === 2 && (
              <div className="proxy-apply">
                <div className="proxy-label">生效方式</div>
                <div className="proxy-apply-seg">
                  <button
                    type="button"
                    className={`proxy-src${apply === "app" ? " active" : ""}`}
                    disabled={sysBusy}
                    onClick={() => chooseApply("app")}
                  >
                    指定应用
                  </button>
                  <button
                    type="button"
                    className={`proxy-src${apply === "sys" ? " active" : ""}`}
                    disabled={sysBusy}
                    onClick={() => chooseApply("sys")}
                  >
                    系统代理
                  </button>
                </div>
                {apply === "app" ? (
                  <div className="proxy-applist">
                    <div className="proxy-apply-seg proxy-sub-seg">
                      <button
                        type="button"
                        className={`proxy-src${appPickMode === "file" ? " active" : ""}`}
                        onClick={() => setAppPickMode("file")}
                      >
                        选择应用文件
                      </button>
                      <button
                        type="button"
                        className={`proxy-src${appPickMode === "running" ? " active" : ""}`}
                        onClick={() => {
                          setAppPickMode("running");
                          loadRunningApps();
                        }}
                      >
                        运行中的应用
                      </button>
                    </div>
                    {appPickMode === "file" ? (
                      <button className="btn btn-ghost btn-sm" disabled={launching} onClick={pickAndLaunch}>
                        {launching ? "启动中…" : "＋ 选择应用并通过代理启动"}
                      </button>
                    ) : (
                      <div className="proxy-running">
                        <div className="proxy-running-head">
                          <span className="dim">{loadingApps ? "读取中…" : `运行中的应用（${runningApps.length}）`}</span>
                          <button className="btn btn-ghost btn-sm" disabled={loadingApps} onClick={loadRunningApps}>
                            刷新
                          </button>
                        </div>
                        <ul className="proxy-running-list">
                          {runningApps.map((a) => (
                            <li key={a.path}>
                              <button
                                type="button"
                                className="proxy-running-item"
                                disabled={launching}
                                title={`通过代理启动：${a.path}`}
                                onClick={() => launchPath(a.path)}
                              >
                                {a.name}
                              </button>
                            </li>
                          ))}
                          {!loadingApps && runningApps.length === 0 && (
                            <li className="dropdown-empty dim">未读到应用，点「刷新」</li>
                          )}
                        </ul>
                      </div>
                    )}
                    <p className="proxy-apply-hint dim">
                      浏览器带 <code>--proxy-server</code> 启动；其他程序注入代理环境变量。已在运行的实例需先完全退出再启动。
                    </p>
                    {launched.length > 0 && (
                      <ul className="proxy-launched">
                        {launched.map((l, i) => (
                          <li key={i}>
                            <span className="peer-dot on" />
                            {l}
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>
                ) : (
                  <div className="proxy-applist">
                    <p className="proxy-apply-hint dim">
                      {sysBusy
                        ? "正在设置系统代理…"
                        : sysProxy
                        ? "✓ 已设为系统代理，多数程序自动走；切回「指定应用」或停止时自动还原。"
                        : "切到此项即接管系统代理。若浏览器装了管代理的扩展，会覆盖系统代理需先关闭。"}
                      {sysBusy && <span className="proxy-spin" aria-hidden />}
                    </p>
                  </div>
                )}
              </div>
            )}
            <button className="btn btn-ghost" disabled={busy} onClick={stop}>
              停止
            </button>
          </div>
        ) : (
          <div className={`proxy-card${busy ? " card-busy" : ""}`}>
            <div className="proxy-label">选择角色</div>
            <div className="proxy-seg">
              <button
                type="button"
                className={`proxy-seg-item${mode === "server" ? " active" : ""}`}
                onClick={() => switchMode("server")}
              >
                <b>服务端</b>
                <span className="dim">出口 · 本机已联 VPN</span>
              </button>
              <button
                type="button"
                className={`proxy-seg-item${mode === "client" ? " active" : ""}`}
                onClick={() => switchMode("client")}
              >
                <b>客户端</b>
                <span className="dim">本地代理入口</span>
              </button>
            </div>

            {mode === "client" && (
              <>
                <div className="proxy-row">
                  <span className="proxy-label">服务端来源</span>
                  <div className="proxy-row-main proxy-src-seg">
                    <button
                      type="button"
                      className={`proxy-src${src === "lan" ? " active" : ""}`}
                      onClick={() => switchSrc("lan")}
                    >
                      内网网段
                    </button>
                    <button
                      type="button"
                      className={`proxy-src${src === "custom" ? " active" : ""}`}
                      onClick={() => switchSrc("custom")}
                    >
                      自定义 IP
                    </button>
                  </div>
                </div>

                {src === "lan" ? (
                  <>
                    <div className="proxy-row">
                      <span className="proxy-label">选择网段</span>
                      <div className="proxy-row-main">
                        <Dropdown
                          value={subnet}
                          options={subnets}
                          placeholder="选择网段…"
                          emptyHint="未检测到网卡"
                          onChange={(v) => {
                            setSubnet(v);
                            setServerFp("");
                          }}
                        />
                        <button className="btn btn-ghost btn-sm" disabled={scanning || !subnet} onClick={scan}>
                          {scanning ? (scanProg ? `扫描 ${scanProg.done}/${scanProg.total}` : "扫描中…") : "扫描本网段"}
                        </button>
                      </div>
                    </div>
                    <div className="proxy-row">
                      <span className="proxy-label">服务端设备</span>
                      <div className="proxy-row-main">
                        <Dropdown
                          value={serverFp}
                          options={deviceOptions}
                          placeholder="扫描后在此选择…"
                          emptyHint="先选网段并扫描"
                          onChange={selectServer}
                        />
                      </div>
                    </div>
                  </>
                ) : (
                  <div className="proxy-row">
                    <span className="proxy-label">服务端 IP</span>
                    <div className="proxy-row-main">
                      <input
                        className="kv-input"
                        style={{ flex: 1 }}
                        value={customIp}
                        onChange={(e) => setCustomIp(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" && !busy) addCustom();
                        }}
                        placeholder="如 192.168.1.20"
                      />
                      <button className="btn btn-ghost btn-sm" disabled={busy || !customIp.trim()} onClick={addCustom}>
                        连接
                      </button>
                    </div>
                  </div>
                )}

                {serverFp && selectedPeer && (
                  <p className="proxy-picked">
                    <span className="peer-dot on" />
                    已选服务端：{selectedPeer.remark || selectedPeer.alias}（{selectedPeer.ip}）
                  </p>
                )}
              </>
            )}

            <div className="proxy-row">
              <span className="proxy-label">访问密码</span>
              <div className="proxy-row-main">
                <input
                  className="kv-input"
                  style={{ flex: 1 }}
                  type={pwVisible ? "text" : "password"}
                  value={pw}
                  onChange={(e) => updatePw(e.target.value)}
                  onKeyDown={onPwKey}
                  placeholder={mode === "server" ? "设一个密码（客户端用它连接）" : "对方服务端的密码"}
                />
                <button
                  className="lan-icon-btn"
                  title={pwVisible ? "隐藏密码" : "显示密码"}
                  aria-label="显示/隐藏密码"
                  onClick={() => setPwVisible((v) => !v)}
                >
                  {pwVisible ? (
                    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" />
                      <line x1="1" y1="1" x2="23" y2="23" />
                    </svg>
                  ) : (
                    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M1 12s4-7 11-7 11 7 11 7-4 7-11 7-11-7-11-7z" />
                      <circle cx="12" cy="12" r="3" />
                    </svg>
                  )}
                </button>
                {mode === "server" && (
                  <button className="lan-icon-btn" title="生成 21 位随机密码" aria-label="生成随机密码" onClick={genPw}>
                    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <polyline points="16 3 21 3 21 8" />
                      <line x1="4" y1="20" x2="21" y2="3" />
                      <polyline points="21 16 21 21 16 21" />
                      <line x1="15" y1="15" x2="21" y2="21" />
                      <line x1="4" y1="4" x2="9" y2="9" />
                    </svg>
                  </button>
                )}
              </div>
            </div>

            <div className="proxy-row">
              <span className="proxy-label">{mode === "server" ? "监听端口" : "服务端端口"}</span>
              <input
                className="kv-input proxy-row-main"
                inputMode="numeric"
                value={port}
                onChange={(e) => setPort(e.target.value.replace(/[^0-9]/g, "").slice(0, 5))}
                onKeyDown={onPwKey}
                placeholder="53318"
              />
            </div>

            <button className="btn btn-primary proxy-start" disabled={busy} onClick={start}>
              {busy
                ? mode === "server"
                  ? "启动中…"
                  : "连接中…"
                : mode === "server"
                ? "开启服务端"
                : "连接服务端"}
            </button>
            {busy && <div className="proxy-card-mask" aria-hidden />}
          </div>
        )}

        {msg && (
          <p key={shake} className={`proxy-msg proxy-msg-${msg.kind}${msg.kind === "err" ? " shake" : ""}`}>
            <span className="proxy-msg-ic">{msg.kind === "err" ? "⚠" : msg.kind === "ok" ? "✓" : "•"}</span>
            <span className="proxy-msg-text">{msg.text}</span>
            <button className="proxy-msg-x" title="关闭" aria-label="关闭" onClick={() => setMsg(null)}>
              ×
            </button>
          </p>
        )}

        <div className="proxy-help dim">
          <p>· 服务端不另开 VPN，只替客户端发请求，走系统现有 VPN。</p>
          <p>· 默认关闭；服务端 / 客户端二选一，需放行端口 53318。</p>
          <p>· 全程 TLS 加密 + 证书固定 + 密码鉴权。</p>
        </div>
      </div>
    </div>
  );
}
