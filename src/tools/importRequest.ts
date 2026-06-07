// 把「Copy as cURL / fetch / fetch(Node.js) / PowerShell」粘贴的文本解析成请求。
// 自动识别格式，尽力解析 method / url / headers / body。

export interface ParsedRequest {
  method: string;
  url: string;
  headers: { key: string; value: string }[];
  body: string;
  contentType: string;
  format: string; // 识别到的格式，用于反馈
}

// ── shell 分词：处理单/双引号、转义、行尾续行符 ──
function tokenizeShell(input: string): string[] {
  const s = input.replace(/\\\r?\n/g, " ");
  const tokens: string[] = [];
  let cur = "";
  let q: "'" | '"' | null = null;
  let started = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (q) {
      if (c === q) q = null;
      else if (q === '"' && c === "\\" && i + 1 < s.length) cur += s[++i];
      else cur += c;
    } else if (c === "'" || c === '"') {
      q = c;
      started = true;
    } else if (c === "\\" && i + 1 < s.length) {
      cur += s[++i];
      started = true;
    } else if (/\s/.test(c)) {
      if (started) {
        tokens.push(cur);
        cur = "";
        started = false;
      }
    } else {
      cur += c;
      started = true;
    }
  }
  if (started) tokens.push(cur);
  return tokens;
}

function parseCurl(text: string): ParsedRequest {
  const all = tokenizeShell(text);
  const ci = all.findIndex((x) => x === "curl" || x.endsWith("/curl"));
  const tokens = ci >= 0 ? all.slice(ci + 1) : all;

  let method = "";
  let url = "";
  let body = "";
  let contentType = "";
  let basic = "";
  const headers: { key: string; value: string }[] = [];
  // 取值型 flag：消费下一个 token
  const valueFlags = new Set([
    "-X", "--request", "-H", "--header", "-d", "--data", "--data-raw",
    "--data-binary", "--data-ascii", "--data-urlencode", "-u", "--user",
    "-b", "--cookie", "--url", "-A", "--user-agent", "-e", "--referer",
  ]);

  for (let i = 0; i < tokens.length; i++) {
    const a = tokens[i];
    const takeNext = () => tokens[++i] ?? "";
    if (a === "-X" || a === "--request") method = takeNext();
    else if (a === "-H" || a === "--header") {
      const h = takeNext();
      const idx = h.indexOf(":");
      if (idx >= 0) {
        const k = h.slice(0, idx).trim();
        const v = h.slice(idx + 1).trim();
        headers.push({ key: k, value: v });
        if (k.toLowerCase() === "content-type") contentType = v;
      }
    } else if (
      a === "-d" || a === "--data" || a === "--data-raw" ||
      a === "--data-binary" || a === "--data-ascii" || a === "--data-urlencode"
    ) {
      body += (body ? "&" : "") + takeNext();
    } else if (a === "-u" || a === "--user") basic = takeNext();
    else if (a === "-b" || a === "--cookie") headers.push({ key: "Cookie", value: takeNext() });
    else if (a === "-A" || a === "--user-agent") headers.push({ key: "User-Agent", value: takeNext() });
    else if (a === "-e" || a === "--referer") headers.push({ key: "Referer", value: takeNext() });
    else if (a === "--url") url = takeNext();
    else if (valueFlags.has(a)) takeNext(); // 其它取值 flag：吞掉值，忽略
    else if (a.startsWith("-")) {
      /* 无值 flag（--compressed/-L/-k 等）：忽略 */
    } else if (!url) url = a;
  }

  if (basic) headers.push({ key: "Authorization", value: "Basic " + btoa(basic) });
  if (!method) method = body ? "POST" : "GET";
  return { method: method.toUpperCase(), url, headers, body, contentType, format: "cURL" };
}

// 从 start 处的 { 匹配到对应的 }，尊重字符串
function matchBraces(s: string, start: number): string | null {
  let depth = 0;
  let q: string | null = null;
  for (let i = start; i < s.length; i++) {
    const c = s[i];
    if (q) {
      if (c === "\\") i++;
      else if (c === q) q = null;
    } else if (c === '"' || c === "'" || c === "`") q = c;
    else if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (depth === 0) return s.slice(start, i + 1);
    }
  }
  return null;
}

function parseFetch(text: string): ParsedRequest {
  const urlMatch = text.match(/fetch\(\s*["'`]([^"'`]+)["'`]/);
  const url = urlMatch ? urlMatch[1] : "";

  let method = "GET";
  let body = "";
  let contentType = "";
  const headers: { key: string; value: string }[] = [];

  const from = urlMatch ? (urlMatch.index ?? 0) + urlMatch[0].length : 0;
  const objStart = text.indexOf("{", from);
  if (objStart >= 0) {
    const objStr = matchBraces(text, objStart);
    if (objStr) {
      try {
        const opts = JSON.parse(objStr);
        if (opts.method) method = String(opts.method);
        if (opts.body != null)
          body = typeof opts.body === "string" ? opts.body : JSON.stringify(opts.body);
        if (opts.headers && typeof opts.headers === "object") {
          for (const [k, v] of Object.entries(opts.headers)) {
            headers.push({ key: k, value: String(v) });
            if (k.toLowerCase() === "content-type") contentType = String(v);
          }
        }
      } catch {
        // 非严格 JSON：忽略，至少保留 URL
      }
    }
  }
  return { method: method.toUpperCase(), url, headers, body, contentType, format: "fetch" };
}

function parsePowerShell(text: string): ParsedRequest {
  const pick = (re: RegExp) => (text.match(re) || [])[1] || "";
  const url = pick(/-Uri\s+["']([^"']+)["']/i);
  let method = pick(/-Method\s+["']([^"']+)["']/i);
  const contentType = pick(/-ContentType\s+["']([^"']+)["']/i);
  const body = pick(/-Body\s+["']([\s\S]*?)["']\s*(?:`|\r?\n|$)/i);

  const headers: { key: string; value: string }[] = [];
  const hdrBlock = text.match(/-Headers\s+@\{([\s\S]*?)\}/i);
  if (hdrBlock) {
    const re = /["']([^"']+)["']\s*=\s*["']([^"']*)["']/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(hdrBlock[1]))) {
      // 跳过 HTTP/2 伪头
      if (!m[1].startsWith(":")) headers.push({ key: m[1], value: m[2] });
    }
  }
  if (contentType) headers.push({ key: "Content-Type", value: contentType });
  return {
    method: (method || (body ? "POST" : "GET")).toUpperCase(),
    url,
    headers,
    body,
    contentType,
    format: "PowerShell",
  };
}

export function parseRequest(text: string): ParsedRequest {
  const s = text.trim();
  if (!s) throw new Error("内容为空");

  let parsed: ParsedRequest;
  if (/^curl[\s'"]/.test(s)) parsed = parseCurl(s);
  else if (/\bfetch\s*\(/.test(s)) parsed = parseFetch(s);
  else if (/Invoke-WebRequest|Invoke-RestMethod/i.test(s)) parsed = parsePowerShell(s);
  else if (s.includes("curl")) parsed = parseCurl(s);
  else throw new Error("无法识别格式，支持 cURL / fetch / PowerShell");

  if (!parsed.url) throw new Error(`已识别为 ${parsed.format}，但未能解析出 URL`);
  return parsed;
}
