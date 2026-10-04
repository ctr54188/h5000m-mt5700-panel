#!/usr/bin/env bash
# 组装可部署目录树 dist/ 并打包。
#
# dist/ 结构即为设备根目录的相对路径，可直接 tar 覆盖安装：
#   tar czf - -C dist . | ssh root@<设备> 'tar xzf - -C / && systemctl restart at-webserver mt5700-web'
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="${TARGET:-}"
BIN="$ROOT/build/out/${TARGET:-host}"
V="$ROOT/vendor"
D="$ROOT/dist"
VER="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/src/mt5700-web/Cargo.toml" | head -1)"

[ -x "$BIN/mt5700-web" ]      || { echo "缺少 $BIN/mt5700-web，先跑 scripts/build.sh" >&2; exit 1; }
[ -x "$BIN/at-webserver-rust" ] || { echo "缺少 $BIN/at-webserver-rust，先跑 scripts/build.sh" >&2; exit 1; }
[ -d "$V/luci" ] || { echo "缺少 vendor/luci，先跑 scripts/fetch-upstreams.sh" >&2; exit 1; }

rm -rf "$D"
mkdir -p "$D"/usr/bin \
         "$D"/etc/config \
         "$D"/etc/systemd/system \
         "$D"/usr/share/mt5700-panel/www/luci-static/resources/view/at-webserver \
         "$D"/usr/share/mt5700-panel/www/luci-static/resources/at-webserver \
         "$D"/usr/share/mt5700-panel/www/luci-static/resources/preload \
         "$D"/usr/share/mt5700-panel/www/luci-static/bootstrap

echo "== 二进制"
install -m0755 "$BIN/mt5700-web" "$BIN/at-webserver-rust" "$D/usr/bin/"

echo "== 系统集成（overlay）"
cp -a "$ROOT/overlay/root/." "$D/"
chmod 0755 "$D/usr/bin/uci"

echo "== 面板外壳"
install -m0644 "$ROOT/overlay/www/index.html" "$ROOT/overlay/www/panel-shell.css" \
	"$D/usr/share/mt5700-panel/www/"

echo "== LuCI 运行时（luci-base）"
RES="$V/luci/modules/luci-base/htdocs/luci-static/resources"
cp -a "$RES/." "$D/usr/share/mt5700-panel/www/luci-static/resources/"
rm -rf "$D/usr/share/mt5700-panel/www/luci-static/resources/view/bootstrap"
cp -a "$V/luci/themes/luci-theme-bootstrap/htdocs/luci-static/bootstrap/logo.svg" \
      "$V/luci/themes/luci-theme-bootstrap/htdocs/luci-static/bootstrap/logo_48.png" \
      "$D/usr/share/mt5700-panel/www/luci-static/bootstrap/" 2>/dev/null || true
# 说明：故意不带主题 cascade.css —— 它会用 !important 覆盖控件，导致下拉框文字被裁、按钮风格不一致

echo "== 项目前端（原样，一行未改）"
cp -a "$V/luci-app-mt5700/htdocs/luci-static/resources/at-webserver/." \
      "$D/usr/share/mt5700-panel/www/luci-static/resources/at-webserver/"
cp -a "$V/luci-app-mt5700/htdocs/luci-static/resources/view/at-webserver/." \
      "$D/usr/share/mt5700-panel/www/luci-static/resources/view/at-webserver/"

echo "== 打包（tar 写到 dist/ 之外，避免边读边写）"
( cd "$D" && find . -type f | sort > .manifest )
mkdir -p "$ROOT/build"
TAR="$ROOT/build/h5000m-mt5700-panel-${VER}-${TARGET:-host}.tar.gz"
tar czf "$TAR" -C "$D" .
( cd "$(dirname "$TAR")" && sha256sum "$(basename "$TAR")" > "$(basename "$TAR").sha256" )
echo "== 结果"
ls -l "$TAR" "$TAR.sha256"
