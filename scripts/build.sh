#!/usr/bin/env bash
# 编译两个 Rust 二进制：
#   build/out/<target>/at-webserver-rust   上游后端（AT 会话 + RPC 8765）
#   build/out/<target>/mt5700-web          面板服务（HTTP 8181 + ubus JSON-RPC）
#
# 用法：
#   scripts/build.sh                                   # 本机架构（开发用）
#   TARGET=aarch64-unknown-linux-gnu scripts/build.sh   # 交叉编译到设备（CI 默认）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="${TARGET:-}"
PROFILE="${PROFILE:-release}"
OUT="$ROOT/build/out/${TARGET:-host}"
mkdir -p "$OUT"

CARGO_TARGET_ARGS=()
if [ -n "$TARGET" ]; then
	CARGO_TARGET_ARGS=(--target "$TARGET")
	# 交叉编译 aarch64-linux-gnu 时的链接器/C 编译器（ring 等需要）
	case "$TARGET" in
	aarch64-unknown-linux-gnu)
		export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER:-aarch64-linux-gnu-gcc}"
		export CC_aarch64_unknown_linux_gnu="${CC_aarch64_unknown_linux_gnu:-aarch64-linux-gnu-gcc}"
		export AR_aarch64_unknown_linux_gnu="${AR_aarch64_unknown_linux_gnu:-aarch64-linux-gnu-ar}"
		;;
	aarch64-unknown-linux-musl)
		export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER:-aarch64-linux-musl-gcc}"
		export CC_aarch64_unknown_linux_musl="${CC_aarch64_unknown_linux_musl:-aarch64-linux-musl-gcc}"
		;;
	esac
fi

echo "== [1/2] 编译面板服务 mt5700-web"
cargo build --"$PROFILE" "${CARGO_TARGET_ARGS[@]}" --manifest-path "$ROOT/src/mt5700-web/Cargo.toml"

echo "== [2/2] 编译上游后端 at-webserver（先打补丁）"
"$ROOT/scripts/apply-patches.sh"
cargo build --"$PROFILE" "${CARGO_TARGET_ARGS[@]}" --manifest-path "$ROOT/vendor/luci-app-mt5700/src/rust/Cargo.toml"

# 产物目录：支持外部 CARGO_TARGET_DIR（CI 用它做缓存，避免污染 vendor/）
TDIR="${CARGO_TARGET_DIR:-}"
crate_target() { # crate_target <crate 目录>
	if [ -n "$TDIR" ]; then printf '%s' "$TDIR"; else printf '%s/target' "$1"; fi
}
DEST="$(crate_target "$ROOT/src/mt5700-web")/${TARGET:-}/$PROFILE"
[ -n "$TARGET" ] || DEST="$(crate_target "$ROOT/src/mt5700-web")/$PROFILE"
cp -v "$DEST/mt5700-web" "$OUT/mt5700-web"
DEST2="$(crate_target "$ROOT/vendor/luci-app-mt5700/src/rust")/${TARGET:-}/$PROFILE"
[ -n "$TARGET" ] || DEST2="$(crate_target "$ROOT/vendor/luci-app-mt5700/src/rust")/$PROFILE"
cp -v "$DEST2/at-webserver" "$OUT/at-webserver-rust"
echo "== 产物：$OUT"
ls -l "$OUT"
