# 架构与实现细节

## 1. 为什么能「不重写界面」

上游前端（`htdocs/luci-static/resources/**`）只依赖一个很小的 LuCI 面：

| 依赖 | 用处 | 谁提供 |
| --- | --- | --- |
| `E(...)`（446 处） | DOM 构造器 | `luci.js`（vendor） |
| `L.rpc.declare({object,method,params,expect})` | RPC | `rpc.js`（vendor）+ 本项目的 `/ubus/` 后端 |
| `L.uci.*` | 读写 UCI 配置 | `uci.js`（vendor）+ 本项目 `uci` 对象 |
| `L.fs.*` | 读日志/写文件 | `fs.js`（vendor）+ 本项目 `file` 对象 |
| `L.view.extend` / `baseclass` / `poll` | 视图基类与轮询 | `luci.js` 内置模块（`baseclass`/`dom`/`poll`/`request`/`session`/`view`） |
| `L.Class`、`L.createObjectURL` | 类系统 / 对象 URL 垫片 | `luci.js` + 项目自带 `compat.js` |

上游**不使用** `L.ui` / `form` / `widgets` / `network` 等重量级组件，因此不需要移植 LuCI 的
CBI 表单体系 —— 这也是移植代价很小的根本原因。

## 2. 视图调度（与 LuCI 完全一致）

LuCI 的 `ucode/template/view.ut` 内容是：

```html
<div id="view">
  <div class="spinning">Loading view…</div>
  <script>L.require('ui').then(function(ui){ ui.instantiateView('{{ view }}'); });</script>
</div>
```

本项目的 `overlay/www/index.html` 做同样的事：

```js
L = new LuCI({ media, resource, ubuspath, dispatchpath, sessionid, nodespec, ... });
L.require('ui').then(ui => ui.instantiateView('at-webserver/' + page)).then(v => new v());
```

`LuCI.view.__init__` 负责「清空 `#view` → 等 `luci-loaded` → `load()` → `render()` →
把结果塞进 `#view` → 附加 LuCI 底栏」，所以视图代码的行为与 OpenWrt 上一致。
（LuCI 通用底栏对项目页面无意义，面板样式里已隐藏。）

## 3. ubus JSON-RPC 后端（mt5700-web）

请求（LuCI `rpc.js` 的标准 ubus 帧）：

```json
{"jsonrpc":"2.0","method":"call","params":["<sid>","<object>","<method>",{...}],"id":1}
```

应答：`{"jsonrpc":"2.0","id":1,"result":[0,{...}]}`，出错时 `result` 为 `[<code>,"<msg>"]`。
注意 LuCI 的 `expect` 会**解包首个键**（`expect:{values:{}}` → 返回 `reply.values`），
所以后端必须按 OpenWrt `rpcd` 的形状返回：

| 对象 | 方法 | 返回形状 |
| --- | --- | --- |
| `mt5700` | `at` / `events` / `logs` | `{success,data,error}`（原样透传上游后端） |
| `mt5700` | `netrate` | `{success,device,rx_bytes,tx_bytes}` |
| `mt5700` | `usb` | `{success,speed_mbps,product,version}` |
| `uci` | `get` | `{values:{<sid>:{".type","name",".anonymous",<opt>:…}}}` |
| `uci` | `set` / `delete` / `add` / `order` | `{}`（`add` 返回 `{section:"cfgXXXXXX"}`） |
| `uci` | `changes` | `{changes:{}}` |
| `file` | `read` | `{data:"<原始文本>"}`（rpcd 语义，非 base64） |
| `file` | `write` | `{path}` |
| `file` | `list` | `{entries:[{name,type,size,mode,atime,mtime,ctime,inode,uid,gid}]}` |
| `file` | `stat` | 扁平字段（同上，无外层包裹） |
| `file` | `exec` | `{code,stdout,stderr}` |
| `service`/`rc` | `list` | `{"<unit>":{"instances":{"<unit>":{"running":bool,"pid":n}},"enabled":bool,"running":bool}}` |
| `service`/`rc` | `set`/`init` | `{}`（映射 `systemctl start|stop|restart|enable|disable`） |
| `log` | `read` | `{log:"<文本>"}` |
| `session`/`system`/`luci` | — | 单机桩：会话恒有效、ACL 全开、`board` 返回主机名与机型 |

`mt5700.at` 的参数会带上 `auth_key`（读 `/etc/config/at-webserver` 的 `websocket_auth_key`），
与上游 `mt5700.uc` 行为一致；`_rid` 仅用于上游 RPC 的临时文件区分，本实现直接长连接转发，
不再依赖 `nc` 与 `/tmp` 临时文件（上游在 OpenWrt 上必须那么做）。

## 4. 运行时文件

```
/usr/bin/at-webserver-rust        上游后端
/usr/bin/mt5700-web               面板服务（HTTP 8181 + ubus 后端）
/usr/bin/uci                      uci 兼容层（show/set/commit）
/etc/config/at-webserver          串口/拨号/通知/定时锁频配置
/etc/systemd/system/at-webserver.service
/etc/systemd/system/mt5700-web.service
/usr/share/mt5700-panel/www/      站点根目录（--root 指向这里）
```

## 5. 安全边界

* 面板默认监听 `0.0.0.0:8181`，无鉴权；建议用防火墙只在 LAN 暴露
  （H5000M 镜像里用 nftables 在 5G 上行口丢弃 8181），或加 `--auth`。
* `file.write` 白名单：`/etc/config/`、`/tmp/`、`/var/`、`/root/`。
* `file.exec` 白名单：只读诊断命令。
* `service`/`rc` 只能操作两个已知 unit。
* 后端（8765）只监听 `127.0.0.1`，不对外。
