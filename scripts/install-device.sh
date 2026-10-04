#!/usr/bin/env bash
# 把 dist/ 覆盖到真机并重启服务（开发期快速迭代用）。
#   scripts/install-device.sh <user@host> [dist.tar.gz]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST="${1:?用法: install-device.sh root@192.168.x.x [tar.gz]}"
TAR="${2:-$(ls -t "$ROOT"/build/*.tar.gz | head -1)}"
echo "== 上传 $TAR -> $HOST"
cat "$TAR" | ssh "$HOST" 'tar xzf - -C / && chmod 0755 /usr/bin/mt5700-web /usr/bin/at-webserver-rust /usr/bin/uci && systemctl daemon-reload && systemctl restart at-webserver mt5700-web && sleep 3 && systemctl is-active at-webserver mt5700-web && wget -qO- http://127.0.0.1:8181/health'
echo
echo "== 完成：http://${HOST#*@}:8181/"
