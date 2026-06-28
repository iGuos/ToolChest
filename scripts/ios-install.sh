#!/usr/bin/env bash
# 把已构建的 iOS 包安装到「已连接的真机」并启动。
# 由 `pnpm install:ios` 调用（前置 `pnpm dist:ios` 已生成 IPA）。
set -euo pipefail

IPA="src-tauri/gen/apple/build/arm64/Baibao Toolbox.ipa"
BUNDLE_ID="com.baibao.toolbox"

if [ ! -f "$IPA" ]; then
  echo "❌ 未找到 IPA：$IPA（请先 pnpm dist:ios）" >&2
  exit 1
fi

# 从 devicectl 输出里挑第一台「可用」的 iPhone/iPad，取其 UUID 标识
DEVICE_ID=$(
  xcrun devicectl list devices 2>/dev/null \
    | grep -iE "iphone|ipad" \
    | grep -i "available" \
    | grep -oiE "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}" \
    | head -1
)

if [ -z "${DEVICE_ID:-}" ]; then
  echo "❌ 未发现已连接的 iPhone/iPad。请插上数据线、解锁屏幕并在手机上点「信任此电脑」。" >&2
  exit 1
fi

echo "📲 安装到设备 $DEVICE_ID …"
xcrun devicectl device install app --device "$DEVICE_ID" "$IPA"
echo "🚀 启动应用 …"
# 启动尽力而为：锁屏等原因导致启动失败时不算整体失败（安装已完成，手动点开即可）
if xcrun devicectl device process launch --device "$DEVICE_ID" "$BUNDLE_ID" 2>/dev/null; then
  echo "✅ 已安装并启动"
else
  echo "✅ 已安装（自动启动失败，多为手机锁屏；解锁后手动点开即可）"
fi
