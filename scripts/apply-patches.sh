#!/usr/bin/env bash
# 把 patches/*.patch 打到 vendor/luci-app-mt5700（幂等：已打过则跳过）。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/vendor/luci-app-mt5700"
[ -d "$APP" ] || { echo "缺少 $APP，先跑 scripts/fetch-upstreams.sh" >&2; exit 1; }

shopt -s nullglob
for p in "$ROOT"/patches/*.patch; do
	name="$(basename "$p")"
	if git -C "$APP" apply --reverse --check "$p" 2>/dev/null; then
		echo "== 已应用，跳过：$name"
		continue
	fi
	echo "== 应用：$name"
	git -C "$APP" apply -p1 "$p"
done
echo "== 补丁处理完成"
