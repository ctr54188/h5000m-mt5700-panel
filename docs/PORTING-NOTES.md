# 移植笔记（踩坑与修复记录）

## 1. 下拉框文字只显示一半 / 按钮风格不一致

**现象**：所有 `<select>` 里的文字被垂直裁掉一半；页面底部的 LuCI 按钮（Save & Apply 等）
与项目卡片风格割裂。

**根因**：LuCI 主题 `cascade.css` 对表单控件使用 `background` 简写 + `!important`，
把项目 CSS 单独写的 `background-color` / `background-image`（自绘箭头）一并重置，
并把 select 的高度/行高/内边距压扁。上游 CSS 里对此有明确注释（`mt5700-logselect select` 段落）。

**修复**：面板**不加载主题 CSS**，外壳改为复用项目 `mt5700.css` 的 `--mt5700-*` token，
并在 `panel-shell.css` 给出确定可用的控件度量（`min-height:40px; line-height:1.5` 等）。
实测（浏览器内取计算样式）：`offsetHeight=42 / clientHeight=40 / line-height=19.5px`，
文字完整；截图确认按钮、卡片、顶栏风格统一。

**附带修掉的两处主题残留**：

* LuCI 通用底栏（`.cbi-page-actions`）：项目页面自带操作按钮 → 隐藏。
* `#modal_overlay`：主题靠 `body.modal-overlay-active` 显隐，不加载主题时它会以空壳常驻
  页面底部（一条白条）→ 补齐 `#modal_overlay{display:none}` +
  `body.modal-overlay-active #modal_overlay{display:flex}`。

## 2. 顶栏与页面风格不一致 + 菜单重复两行

LuCI 主题顶栏（深色 `header` + `#topmenu`）与外层容器 `.main-left/.main-right` 是给
LuCI 主题形态设计的。移植后改为一套顶栏：渐变 logo + 药丸导航（激活态实心蓝）+ 玻璃拟态
sticky 头部，全部使用项目设计 token，避免"主题 + 页面"两套视觉。

## 3. 串口自动探测失败（上游 bug）

见 README §5。要点：分行的终止符只认 `\n`，而应答是 `\r\n`；模组上一次会话异常结束时
残留的 `^@^@^@`（无换行）会把 `OK` 拼成同一行 → 整行匹配失败 → 可用端口被判为无应答。

## 4. Debian 侧没有 UCI

`at-webserver` 用 `uci show/set/commit` 读写配置。Debian 无 UCI，因此：

* 提供 Shell 兼容层 `/usr/bin/uci`（`show` 输出 `pkg.sect.opt='value'`，`set` 原地改写，
  `commit` 空操作；忽略行内 `#` 注释与引号）；
* 面板侧的 `uci` RPC 由 `mt5700-web` 用 Rust 直接解析/写回 UCI 文本格式。

两者操作同一文件，格式兼容（`config <type> '<name>'` / `option k 'v'` / `list k 'v'`）。

## 5. 网络设备名不固定

上游 `mt5700.uc` 的 `netrate` 先查 `network.MT5700M.device|ifname`，取不到就退回 `eth2`。
Debian 上 5G 网卡常是 CDU 网卡名（如 `enx9c544001be4f`），因此本实现额外扫描
`/sys/class/net` 里 `enx*`/`wwan*`/`usb*` 并优先选 carrier=1 的那个。

## 6. 验证方式

* `ubus` 调用计数：单次网络状态页加载约 160+ 次 `mt5700.at`、48 次 `mt5700.netrate`，
  全页面 12/12 渲染无 JS 报错（用浏览器自动化逐页 iframe 挂载检查）。
* 服务页能从 systemd 读到 `at-webserver` 的实际 PID；
  日志页能读到 journal 里的真实 AT 往返记录。
* 分发包在真机上覆盖安装后：`systemctl is-active` 两个服务、`/health`、`ATI` 全部正常。
