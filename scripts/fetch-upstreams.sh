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

# git 网络偶发失败（CI 上见过 "git clone ... Error 128"）：重试 + 回退到完整克隆
retry() { # retry <次数> <命令...>
	local n="$1"; shift
	local i=1
	while true; do
		if "$@"; then return 0; fi
		[ "$i" -ge "$n" ] && return 1
		echo "   [第 $i 次失败，$((i*5))s 后重试] $*" >&2
		sleep $((i*5)); i=$((i+1))
	done
}

clone_pin() {
	local repo="$1" commit="$2" dest="$3"
	if [ -d "$dest/.git" ]; then
		echo "== 复用已有 $dest"
	else
		if [ -e "$dest" ]; then
			# CI 缓存可能只恢复了 vendored 目录的一部分（如 .../src/rust/target），
			# 留下一个「不是 git 仓库」的空壳，直接 clone 会 fatal: destination path
			# already exists and is not an empty directory。这里清掉重来。
			echo "== 清理非 git 残留目录 $dest"
			rm -rf "$dest"
		fi
		echo "== 克隆 $repo -> $dest"
		retry 3 git clone --filter=blob:none --no-checkout "$repo" "$dest" \
			|| retry 2 git clone --no-checkout "$repo" "$dest" \
			|| { echo "!! 克隆 $repo 失败" >&2; return 1; }
	fi
	echo "== 检出 $commit"
	retry 3 git -C "$dest" fetch --depth 1 origin "$commit" \
		|| retry 2 git -C "$dest" fetch origin \
		|| { echo "!! fetch $commit 失败" >&2; return 1; }
	git -C "$dest" checkout --force "$commit" || return 1
	git -C "$dest" log -1 --format='   %h %ad %s' --date=short
}

mkdir -p "$V"
clone_pin "$LUCI_APP_REPO" "$LUCI_APP_COMMIT" "$V/luci-app-mt5700"
clone_pin "$LUCI_REPO"     "$LUCI_COMMIT"     "$V/luci"

echo "== 完成。下一步：scripts/apply-patches.sh"
