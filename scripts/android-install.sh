#!/usr/bin/env bash
# 把已构建的 Android 包安装到「已连接的安卓设备」并启动。
# 由 `pnpm install:android` 调用（前置 `pnpm dist:android` 已生成 APK）。
set -euo pipefail

PKG="com.baibao.toolbox"
OUT="src-tauri/gen/android/app/build/outputs/apk"

# 取 debug APK（优先 universal，其次任意一个）
APK=$(find "$OUT" -name "*.apk" -type f 2>/dev/null | grep -i debug | grep -i universal | head -1)
[ -n "${APK:-}" ] || APK=$(find "$OUT" -name "*.apk" -type f 2>/dev/null | grep -i debug | head -1)
[ -n "${APK:-}" ] || APK=$(find "$OUT" -name "*.apk" -type f 2>/dev/null | head -1)

if [ -z "${APK:-}" ]; then
  echo "❌ 未找到 APK：$OUT（请先 pnpm dist:android，或确认已 tauri android init）" >&2
  exit 1
fi

if ! command -v adb >/dev/null 2>&1; then
  echo "❌ 未找到 adb，请安装 Android SDK Platform-Tools 并加入 PATH。" >&2
  exit 1
fi

# 至少有一台已授权的设备
if ! adb get-state >/dev/null 2>&1; then
  echo "❌ 未检测到已连接的安卓设备。请插线、开启「USB 调试」并在手机上允许调试授权。" >&2
  exit 1
fi

echo "📲 安装 $APK …"
adb install -r "$APK"
echo "🚀 启动 $PKG …"
adb shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1 || true
echo "✅ 已安装并启动"
