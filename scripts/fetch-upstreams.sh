#!/usr/bin/env bash
# 拉取上游源码到 vendor/，pin 到本项目已验证的提交。
#
#   vendor/luci-app-mt5700   项目前端 + Rust 后端（GPL-3.0）
#   vendor/luci              openwrt/luci（Apache-2.0），只取 luci-base 运行时与 bootstrap 主题图标
#
# 用法：
#   scripts/fetch-upstreams.sh                 # 首次拉取（浅克隆 + 检出 pin 提交）
#   LUCI_APP_COMMIT=<sha> scripts/...          # 覆盖 pin
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
V="$ROOT/vendor"

LUCI_APP_REPO="${LUCI_APP_REPO:-https://github.com/LianXia233/luci-app-mt5700.git}"
LUCI_APP_COMMIT="${LUCI_APP_COMMIT:-eba64994fec392b818afdcbca148b0e1dd8dc96f}"   # 1.14.2 / 后端 1.5.0
LUCI_REPO="${LUCI_REPO:-https://github.com/openwrt/luci.git}"
LUCI_COMMIT="${LUCI_COMMIT:-ad0b5676921df503d322454029839a856d17a07c}"           # openwrt-24.10

clone_pin() {
	local repo="$1" commit="$2" dest="$3"
	if [ -d "$dest/.git" ]; then
		echo "== 复用已有 $dest"
	else
		echo "== 克隆 $repo -> $dest"
		git clone --filter=blob:none --no-checkout "$repo" "$dest"
	fi
	echo "== 检出 $commit"
	git -C "$dest" fetch --depth 1 origin "$commit" 2>/dev/null || git -C "$dest" fetch origin
	git -C "$dest" checkout --force "$commit"
	git -C "$dest" log -1 --format='   %h %ad %s' --date=short
}

mkdir -p "$V"
clone_pin "$LUCI_APP_REPO" "$LUCI_APP_COMMIT" "$V/luci-app-mt5700"
clone_pin "$LUCI_REPO"     "$LUCI_COMMIT"     "$V/luci"

echo "== 完成。下一步：scripts/apply-patches.sh"
