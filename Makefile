# H5000M / MT5700M 模组面板（Debian 移植）—— 常用目标
SHELL := /bin/bash
TARGET ?= aarch64-unknown-linux-gnu

.PHONY: all deps build dist check deploy clean update-upstream

all: build dist

## 拉取上游源码（pin 提交）
deps:
	scripts/fetch-upstreams.sh

## 编译两个二进制（默认交叉编译到 aarch64-gnu，与设备 Debian 13 匹配）
build:
	TARGET=$(TARGET) scripts/build.sh

## 组装 dist/ 与 tar.gz 分发包
dist:
	TARGET=$(TARGET) scripts/assemble.sh

## 本机编译 + 语法检查（开发/CI 快速回路）
check:
	TARGET= scripts/build.sh
	cargo clippy --manifest-path src/mt5700-web/Cargo.toml -- -D warnings || true

## 覆盖安装到真机：make deploy HOST=root@192.168.5.169
deploy: dist
	scripts/install-device.sh $(HOST)

## 升级上游 pin 后重新拉取
update-upstream:
	rm -rf vendor
	scripts/fetch-upstreams.sh

clean:
	rm -rf dist build/out src/mt5700-web/target vendor/luci-app-mt5700/src/rust/target
