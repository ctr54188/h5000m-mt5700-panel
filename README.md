# h5000m-mt5700-panel

把 [luci-app-mt5700](https://github.com/LianXia233/luci-app-mt5700)（MT5700M 5G 模组管理面板）
移植到 **Debian / 通用 Linux** 的移植层：**不改前端一行代码**，只补上它需要的 LuCI 运行时与
ubus RPC 后端，让这套 12 页的模组管理界面能在 OpenWrt 之外（本机用在 Hiveton H5000M 的
Debian 13 镜像上）原样跑起来。

> 配套仓库：[`h5000m-debian`](../h5000m-debian) —— H5000M 的 Debian 13 镜像构建
> （MT7987 内核补丁、DTS、rootfs、打包）。本仓库的产物会被装进那个镜像。

---

## 1. 它长什么样 / 能干什么

12 个页面全部可用，数据来自真实模组（AT 指令）：

| 页面 | 说明 |
| --- | --- |
| 网络状态 | RSRP/RSRQ/SINR/综合信号、实时速率与曲线、连接状态、载波聚合、模组 12 路温度、流量统计、IP/DNS、调制方式 |
| 网络设置 | LTE/NR 锁频（频段/频点/小区）、邻区扫描、5G SA/NSA 选项、网络拒绝原因 |
| 拨号设置 | 自动拨号、APN、USB/网口工作模式、PDP 上下文 |
| 全网扫频 | `AT^CELLSCAN` 全网扫描 + 一键锁定 |
| 定时锁频 | 日/夜时段自动切换锁频策略（UCI 驱动后端守护） |
| 模组设置 | 硬件信息、SIM/USIM、射频模式、系统重置 |
| 模组升级 | FOTA / 本地固件升级 |
| 短信中心 / 短信设置 | PDU 编解码、长短信分片、存储箱、SMSC、USSD |
| AT 调试终端 | 原始 AT 交互 + 快捷指令 + 命令审计 |
| 运行日志 | 模组拨号 / 接口网络 / 通知记录三类日志 |
| 服务配置 | 后台守护进程状态（开机自启、串口/TCP 探测、通知联动） |

---

## 2. 架构

```
浏览器
  │ HTTP :8181（静态资源 + ubus JSON-RPC）
  ▼
mt5700-web                     ← 本仓库新增（Rust，单文件服务，仅依赖 serde_json）
  │ TCP newline-JSON 127.0.0.1:8765
  ▼
at-webserver                   ← 上游 Rust 后端（AT 会话/URC/短信/扫频/定时锁频）
  │ 串口 115200（/dev/ttyUSB1 = PCUI，自动探测）
  ▼
MT5700M-CN 模组
```

* **静态资源**：LuCI 运行时（`luci.js`/`rpc.js`/`uci.js`/`fs.js`/`ui.js`/`form.js`…）+
  项目前端（`at-webserver/*.js`、12 个视图）原样提供。
* **ubus JSON-RPC**：`mt5700-web` 实现前端需要的对象
  `session / system / luci / uci / file / service / rc / log / mt5700`，
  其中 `mt5700.at/events/logs` 转发给 `at-webserver`，
  `mt5700.netrate/usb` 按上游 `mt5700.uc` 的逻辑直读 sysfs（不发 AT，不占用 AT 通道）。
* **外壳**：`index.html` 等价 LuCI 的 `header.ut` + `footer.ut` + `view.ut` 三件套 ——
  注入 `L.env`，然后 `ui.instantiateView('at-webserver/<page>')`，
  因此视图的 `load()/render()` 生命周期与在 OpenWrt 上完全一致。

细节见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)。

---

## 3. 快速开始

### 3.1 在设备上安装（已有 Debian rootfs）

```sh
# 下载 release 里的分发包（或自己 make dist 出来的）
sha256sum -c h5000m-mt5700-panel-*.tar.gz.sha256
tar xzf h5000m-mt5700-panel-*.tar.gz -C /
systemctl daemon-reload
systemctl enable --now at-webserver mt5700-web
```

打开 `http://<设备IP>:8181/`。

> 默认**无鉴权**，请只在可信内网暴露；需要认证时给 `mt5700-web` 加 `--auth 用户:口令`
> （改 `/etc/systemd/system/mt5700-web.service` 的 ExecStart）。

### 3.2 从源码构建

依赖：`git`、`cargo`/`rustc`（≥1.88，上游后端依赖 icu 2.x）、`tar`。
交叉编译到 aarch64 另需 `gcc-aarch64-linux-gnu`。

```sh
make deps     # 拉取上游源码到 vendor/（pin 到已验证提交）
make build    # 编译两个二进制（默认 TARGET=aarch64-unknown-linux-gnu）
make dist     # 组装 dist/ 并打包 build/*.tar.gz
make deploy HOST=root@192.168.5.169   # 覆盖安装到真机并重启服务
```

本机（同架构）编译：

```sh
TARGET= make build && TARGET= make dist
```

交叉编译（x86_64 主机 → aarch64 设备）：

```sh
sudo apt-get install -y gcc-aarch64-linux-gnu
make build TARGET=aarch64-unknown-linux-gnu
```

产物：

```
dist/usr/bin/mt5700-web                 面板服务（HTTP 8181 + ubus 后端）
dist/usr/bin/at-webserver-rust          上游后端（AT + RPC 8765）
dist/usr/bin/uci                        极简 uci 兼容层（供 at-webserver 读配置）
dist/etc/config/at-webserver            后端配置
dist/etc/systemd/system/*.service       两个 systemd 单元
dist/usr/share/mt5700-panel/www/        面板站点（外壳 + LuCI 运行时 + 项目前端）
build/h5000m-mt5700-panel-<ver>-<target>.tar.gz
```

### 3.3 版本 pin

| 上游 | 提交 | 说明 |
| --- | --- | --- |
| `LianXia233/luci-app-mt5700` | `eba64994fec392b818afdcbca148b0e1dd8dc96f` | 1.14.2 / 后端 1.5.0 |
| `openwrt/luci` | `ad0b5676921df503d322454029839a856d17a07c` | `openwrt-24.10`（与 ImmortalWrt 24.10 对应） |

覆盖方式：`LUCI_APP_COMMIT=... LUCI_COMMIT=... make deps`。

---

## 4. 注意事项（踩过的坑，必读）

1. **不要加载 LuCI 主题的 `cascade.css`。**
   主题用 `background` 简写 + `!important` 覆盖控件（上游 CSS 里对这一点都有注释），实测后果：
   * 所有 `<select>` 文字被裁掉一半（高度/行高被压扁）；
   * 按钮、输入框与页面卡片风格割裂。
   因此外壳样式（`overlay/www/panel-shell.css`）直接复用项目 `mt5700.css` 的 `--mt5700-*`
   设计 token，只保留主题的图标资源。

2. **`uci` 语义**：`at-webserver` 通过 `uci show at-webserver` / `uci set` / `uci commit` 读写配置，
   而 Debian 上没有 UCI。本仓库提供一个 Shell 兼容层（`overlay/root/usr/bin/uci`，支持
   `show/set/commit`，会忽略行内注释），同时面板侧的 `uci` RPC 由 `mt5700-web` 直接读写
   `/etc/config/<pkg>`（UCI 文本格式）。两者读写同一份文件，注意别同时手改。

3. **`file.write` 白名单**：只允许写 `/etc/config/`、`/tmp/`、`/var/`、`/root/`；
   `file.exec` 只允许白名单里的只读诊断命令（`ls`/`ip`/`iw`/`systemctl`/`journalctl`…）。

4. **`uci.changes` 返回空**：写入即落盘，没有“未保存变更”的概念（页面的保存按钮仍会走
   `uci.set` + `commit` 流程，只是结果立刻生效）。

5. **`log.read` 的来源是 journal**（OpenWrt 上通常是 logd）。字段已按前端解析需求对齐
   （`{log: "<文本>"}`，返回最后 N 行）。

6. **串口自动探测**：`at-webserver` 会按 sysfs 接口名（PCUI/Application/GPS）打分选口，
   拿不到 sysfs 信息时回退到编号偏好。本仓库打了补丁修掉上游一个真实故障（见下）。

7. **服务名固定**：`service`/`rc` RPC 只允许操作 `at-webserver`、`mt5700-web` 两个 unit，
   避免面板被当成通用 systemd 遥控器。

---

## 5. 上游补丁

`patches/` 下的补丁在 `make build` 时自动应用到 `vendor/luci-app-mt5700`（幂等）。

### `0001-serialdetect-split-lines-on-cr.patch`

**故障**：`serialdetect.rs::probe_at()` 只按 `\n` 分行判定结束行（`OK`/`ERROR`…）。
模组上一次会话异常关闭时，串口缓冲区里会留下**不带换行的 NUL 垃圾**（`^@^@^@`），
于是应答被拼成一行 `"\0\0\0\0\0\0\rAT\rOK"` → 整行匹配失败 → 报
「候选串口都没有正常应答 AT」，明明能用的 `/dev/ttyUSB1` 被跳过，自动探测整体失败
（真机复现：手动 `printf 'AT\r' > /dev/ttyUSB1` 立刻回 `OK`，而 auto 模式必失败）。

**修复**：同时按 `\r` 与 `\n` 分行，并跳过空行。AT 应答本就是 `\r\n` 结尾，
按 `\r` 分行是标准做法，对正常模组无副作用。

---

## 6. GitHub Actions

`.github/workflows/build.yml`：

| Job | 作用 |
| --- | --- |
| `build-aarch64`（ubuntu-latest，交叉） | 安装 `gcc-aarch64-linux-gnu` → `make deps/build/dist` → 上传 `*.tar.gz` + sha256 |
| `build-native-arm`（ubuntu-24.04-arm，原生，可失败） | 在 arm64 runner 上原生编译，验证交叉结果一致 |
| `release`（`v*` tag） | 汇总产物，创建 GitHub Release 并附上分发包 |

打 tag 即出 release：

```sh
git tag v2.0.0 && git push origin v2.0.0
```

---

## 6.1 与镜像仓库的关系（单向）

本仓库**不触发**任何其它仓库的 workflow，也不依赖它们编译。

镜像仓库 [`h5000m-debian`](../h5000m-debian) 有一个可选的「带面板」workflow
（`image-with-panel.yml`），它在需要时会**单向**来本仓库：

* 优先下载本仓库的 **Release 资产**（`h5000m-mt5700-panel-*.tar.gz`）；
* 取不到时回退：`git clone` 本仓库 → `make build` → 本地打包。

因此给本仓库打 tag 出 Release（`git tag v2.0.0 && git push origin v2.0.0`）会让镜像侧
拿到「已编译好的面板包」，但**打 tag 不会触发镜像仓库的任何构建**。

## 7. 目录结构

```
src/mt5700-web/         面板服务源码（Rust，本仓库新增）
overlay/
  www/index.html        外壳（等价 header.ut/footer.ut/view.ut）
  www/panel-shell.css   外壳样式（复用项目设计 token）
  root/…                uci 兼容层、/etc/config/at-webserver、两个 systemd 单元
patches/                上游补丁（自动应用）
scripts/                fetch-upstreams / apply-patches / build / assemble / install-device
docs/                   架构与移植笔记
vendor/                 make deps 生成（不入库）
build/, dist/           构建产物（不入库）
```

---

## 8. 许可与归属

* 本仓库的移植代码（`src/mt5700-web`、`overlay/`、`scripts/`）以 **GPL-3.0** 发布，
  与上游 `luci-app-mt5700`（GPL-3.0）保持一致。
* 运行/构建时会拉取并分发以下上游代码，版权归其作者：
  * `LianXia233/luci-app-mt5700` — GPL-3.0（前端与 AT 后端，本项目**未修改**其前端）
  * `openwrt/luci` — Apache-2.0（LuCI 运行时）
* 本项目与上述上游无隶属关系，只是移植层。
