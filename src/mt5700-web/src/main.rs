//! mt5700-web —— Debian 侧 MT5700M 5G 模组管理面板（移植 luci-app-mt5700）。
//!
//! 组成：
//!   1. 静态文件服务：原样提供 luci-base 运行时 + 项目前端资源（/luci-static/...）
//!   2. ubus JSON-RPC 后端（POST /ubus/）：实现 LuCI 前端依赖的全部对象
//!        session / system / luci / uci / file / service / rc / log / mt5700
//!      其中 mt5700.at / events / logs 转发到 at-webserver 的 TCP newline-JSON RPC，
//!      netrate / usb 直读 sysfs（移植自项目 root/usr/share/rpcd/ucode/mt5700.uc）。
//!   3. 兼容入口：POST /rpc（简单转发）、GET /api/at?cmd=
//!
//! 无第三方依赖（仅 serde_json），单二进制部署。

use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const RPC_TIMEOUT: Duration = Duration::from_secs(200);
const UCI_DIR: &str = "/etc/config";
/// 允许写入的路径前缀（读不限，写限这几处）
const WRITE_PREFIXES: &[&str] = &["/etc/config/", "/tmp/", "/var/", "/root/"];
/// file.exec 白名单（只读诊断类命令）
const EXEC_ALLOW: &[&str] = &[
    "ls", "cat", "ip", "iw", "iwconfig", "systemctl", "df", "free", "uptime", "uname", "dmesg",
    "logread", "journalctl", "ubus", "uci", "mmcli", "lsusb", "hostname", "date", "ps",
];

// ---------------------------------------------------------------- 参数

#[derive(Clone)]
struct Cfg {
    bind: String,
    port: u16,
    rpc: String,
    root: PathBuf,
    auth: Option<(String, String)>,
    debug: bool,
}

fn parse_args() -> Cfg {
    let argv: Vec<String> = std::env::args().collect();
    let mut c = Cfg {
        bind: "0.0.0.0".into(),
        port: 8181,
        rpc: "127.0.0.1:8765".into(),
        root: PathBuf::from("/usr/share/mt5700-panel/www"),
        auth: None,
        debug: false,
    };
    let next = |n: usize| argv.get(n + 1).cloned().unwrap_or_default();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--bind" => {
                c.bind = next(i);
                i += 2;
            }
            "--port" => {
                c.port = next(i).parse().unwrap_or(8181);
                i += 2;
            }
            "--rpc" => {
                c.rpc = next(i);
                i += 2;
            }
            "--root" => {
                c.root = PathBuf::from(next(i));
                i += 2;
            }
            "--auth" => {
                let v = next(i);
                if let Some((u, p)) = v.split_once(':') {
                    c.auth = Some((u.into(), p.into()));
                }
                i += 2;
            }
            "--debug" => {
                c.debug = true;
                i += 1;
            }
            "--help" | "-h" => {
                println!(
                    "用法: mt5700-web [--bind 0.0.0.0] [--port 8181] [--rpc 127.0.0.1:8765]\n\
                     \x20              [--root /usr/share/mt5700-panel/www] [--auth 用户:口令] [--debug]"
                );
                std::process::exit(0);
            }
            "--version" => {
                println!("mt5700-web {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => i += 1,
        }
    }
    c
}

fn main() {
    let cfg = parse_args();
    if !cfg.root.is_dir() {
        eprintln!("警告: 静态资源目录不存在: {}", cfg.root.display());
    }
    let listener = match TcpListener::bind((cfg.bind.as_str(), cfg.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("监听 {}:{} 失败: {e}", cfg.bind, cfg.port);
            std::process::exit(1);
        }
    };
    println!(
        "mt5700-web {} 监听 {}:{}，静态目录 {}，RPC 后端 {}",
        env!("CARGO_PKG_VERSION"),
        cfg.bind,
        cfg.port,
        cfg.root.display(),
        cfg.rpc
    );
    if cfg.auth.is_none() {
        println!("提示: 未设置 --auth，面板对可访问该端口的机器开放（建议仅 LAN 暴露）");
    }
    let shared = std::sync::Arc::new(cfg);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let c = shared.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(s, &c) {
                        eprintln!("连接处理失败: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept 失败: {e}"),
        }
    }
}

// ---------------------------------------------------------------- HTTP

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, n: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.as_str())
    }
}

fn read_request(s: &mut TcpStream) -> std::io::Result<Request> {
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.trim_end().split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut body = Vec::new();
    if let Some(cl) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
    {
        let mut buf = vec![0u8; cl.min(8 << 20)];
        r.read_exact(&mut buf)?;
        body = buf;
    }
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn respond(
    s: &mut TcpStream,
    status: &str,
    ctype: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

fn handle(mut s: TcpStream, cfg: &Cfg) -> std::io::Result<()> {
    let req = read_request(&mut s)?;
    if !authorized(&req, &cfg.auth) {
        return respond(
            &mut s,
            "401 Unauthorized",
            "text/plain; charset=utf-8",
            "需要认证".as_bytes(),
            &[("WWW-Authenticate", "Basic realm=\"MT5700M\"")],
        );
    }
    let p = req.path.as_str();
    if req.method == "POST" && (p == "/ubus" || p == "/ubus/" || p == "/cgi-bin/luci/ubus") {
        let body = String::from_utf8_lossy(&req.body).to_string();
        let resp = ubus_entry(&body, cfg);
        return respond(
            &mut s,
            "200 OK",
            "application/json; charset=utf-8",
            resp.as_bytes(),
            &[],
        );
    }
    if req.method == "POST" && p == "/rpc" {
        let payload = String::from_utf8_lossy(&req.body).to_string();
        return match rpc_backend(&cfg.rpc, payload.trim()) {
            Ok(r) => respond(
                &mut s,
                "200 OK",
                "application/json; charset=utf-8",
                r.as_bytes(),
                &[],
            ),
            Err(e) => {
                let b = json!({"error":{"code":-1,"message":e}}).to_string();
                respond(
                    &mut s,
                    "502 Bad Gateway",
                    "application/json; charset=utf-8",
                    b.as_bytes(),
                    &[],
                )
            }
        };
    }
    if req.method == "GET" && p == "/api/at" {
        let mut cmd = String::new();
        for kv in req.query.split('&').filter(|x| !x.is_empty()) {
            if let Some(("cmd", v)) = kv.split_once('=') {
                cmd = url_decode(v);
            }
        }
        let payload = json!({"id":1,"method":"at","params":{"cmd":cmd}}).to_string();
        return match rpc_backend(&cfg.rpc, &payload) {
            Ok(r) => respond(
                &mut s,
                "200 OK",
                "application/json; charset=utf-8",
                r.as_bytes(),
                &[],
            ),
            Err(e) => {
                let b = json!({"error":{"code":-1,"message":e}}).to_string();
                respond(
                    &mut s,
                    "502 Bad Gateway",
                    "application/json; charset=utf-8",
                    b.as_bytes(),
                    &[],
                )
            }
        };
    }
    if req.method == "GET" && p == "/health" {
        return respond(&mut s, "200 OK", "text/plain", b"ok", &[]);
    }
    if req.method != "GET" && req.method != "HEAD" {
        return respond(
            &mut s,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
            &[],
        );
    }
    serve_static(&mut s, p, cfg)
}

fn serve_static(mut s: &mut TcpStream, path: &str, cfg: &Cfg) -> std::io::Result<()> {
    let rel = if path == "/" { "/index.html" } else { path };
    let rel = rel.trim_start_matches('/');
    let mut full = cfg.root.clone();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => full.push(c),
            Component::CurDir => {}
            _ => {
                return respond(&mut s, "400 Bad Request", "text/plain", b"bad path", &[]);
            }
        }
    }
    if full.is_dir() {
        full.push("index.html");
    }
    match std::fs::read(&full) {
        Ok(data) => {
            let ct = mime_of(&full);
            respond(s, "200 OK", ct, &data, &[])
        }
        Err(_) => respond(s, "404 Not Found", "text/plain; charset=utf-8", "未找到".as_bytes(), &[]),
    }
}

fn mime_of(p: &Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" | "log" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------- 工具

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(h, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn b64_decode(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = match T.iter().position(|&t| t == c) {
            Some(v) => v as u32,
            None => continue,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn authorized(req: &Request, auth: &Option<(String, String)>) -> bool {
    let (u, p) = match auth {
        Some(a) => a,
        None => return true,
    };
    let h = match req.header("authorization") {
        Some(h) => h,
        None => return false,
    };
    let b = match h
        .strip_prefix("Basic ")
        .or_else(|| h.strip_prefix("basic "))
    {
        Some(b) => b,
        None => return false,
    };
    match String::from_utf8_lossy(&b64_decode(b))
        .split_once(':')
        .map(|(a, b)| (a.to_string(), b.to_string()))
    {
        Some((au, ap)) => &au == u && &ap == p,
        None => false,
    }
}

/// 把一整行 JSON 转发给 at-webserver 的 TCP RPC，返回应答行。
fn rpc_backend(addr: &str, payload: &str) -> Result<String, String> {
    if payload.is_empty() {
        return Err("空请求".into());
    }
    let mut s = TcpStream::connect(addr).map_err(|e| format!("连接后端 {addr} 失败: {e}"))?;
    s.set_read_timeout(Some(RPC_TIMEOUT)).ok();
    let mut line = payload.to_string();
    line.push('\n');
    s.write_all(line.as_bytes())
        .map_err(|e| format!("写入后端失败: {e}"))?;
    let mut r = BufReader::new(s);
    let mut resp = String::new();
    r.read_line(&mut resp)
        .map_err(|e| format!("读取后端应答失败: {e}"))?;
    if resp.trim().is_empty() {
        return Err("后端无应答（at-webserver 未运行？）".into());
    }
    Ok(resp.trim().to_string())
}

fn read_sysfs(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

// ---------------------------------------------------------------- uci 存储

#[derive(Clone, Debug)]
struct Section {
    stype: String,
    name: String,
    anonymous: bool,
    /// option/list：名 → 值列表（单值为长度 1）
    options: Vec<(String, Vec<String>)>,
}

impl Section {
    fn get(&self, opt: &str) -> Option<&Vec<String>> {
        self.options.iter().find(|(k, _)| k == opt).map(|(_, v)| v)
    }
    fn set(&mut self, opt: &str, vals: Vec<String>) {
        match self.options.iter_mut().find(|(k, _)| k == opt) {
            Some(e) => e.1 = vals,
            None => self.options.push((opt.to_string(), vals)),
        }
    }
    fn del(&mut self, opt: &str) {
        self.options.retain(|(k, _)| k != opt);
    }
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert(".type".into(), json!(self.stype));
        m.insert(".name".into(), json!(self.name));
        m.insert(".anonymous".into(), json!(self.anonymous));
        for (k, v) in &self.options {
            if v.len() == 1 {
                m.insert(k.clone(), json!(v[0]));
            } else {
                m.insert(k.clone(), json!(v));
            }
        }
        Value::Object(m)
    }
}

fn split_uci_line(line: &str) -> Vec<String> {
    // 去掉行内注释（# 前有空白）
    let mut s = line.to_string();
    if let Some(idx) = s.find(" #").or_else(|| s.find("\t#")) {
        s.truncate(idx);
    }
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' && i + 1 < b.len() {
                    cur.push(b[i + 1] as char);
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                } else {
                    cur.push(c as char);
                }
            }
            None => {
                if c == b'\'' || c == b'"' {
                    quote = Some(c);
                } else if c == b' ' || c == b'\t' {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                } else {
                    cur.push(c as char);
                }
            }
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn uci_path(pkg: &str) -> PathBuf {
    Path::new(UCI_DIR).join(pkg)
}

fn uci_load(pkg: &str) -> Result<Vec<Section>, String> {
    let path = uci_path(pkg);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
    };
    let mut sections: Vec<Section> = Vec::new();
    let mut auto = 0usize;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tok = split_uci_line(line);
        if tok.is_empty() {
            continue;
        }
        match tok[0].as_str() {
            "config" => {
                let stype = tok.get(1).cloned().unwrap_or_default();
                let (name, anon) = match tok.get(2) {
                    Some(n) => (n.clone(), false),
                    None => {
                        auto += 1;
                        (format!("cfg{:06x}", 0x100000 + auto), true)
                    }
                };
                sections.push(Section {
                    stype,
                    name,
                    anonymous: anon,
                    options: Vec::new(),
                });
            }
            "option" | "list" => {
                if let (Some(name), Some(val)) = (tok.get(1), tok.get(2)) {
                    if let Some(sec) = sections.last_mut() {
                        if tok[0] == "option" {
                            sec.set(name, vec![val.clone()]);
                        } else {
                            match sec.options.iter_mut().find(|(k, _)| k == name) {
                                Some(e) => e.1.push(val.clone()),
                                None => sec.options.push((name.clone(), vec![val.clone()])),
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(sections)
}

fn uci_save(pkg: &str, sections: &[Section]) -> Result<(), String> {
    let mut out = String::new();
    for sec in sections {
        if sec.anonymous {
            out.push_str(&format!("config {}\n", sec.stype));
        } else {
            out.push_str(&format!("config {} '{}'\n", sec.stype, sec.name));
        }
        for (k, vals) in &sec.options {
            if vals.len() == 1 {
                out.push_str(&format!("\toption {} '{}'\n", k, vals[0]));
            } else {
                for v in vals {
                    out.push_str(&format!("\tlist {} '{}'\n", k, v));
                }
            }
        }
        out.push('\n');
    }
    std::fs::write(uci_path(pkg), out).map_err(|e| format!("写入配置失败: {e}"))
}

// ---------------------------------------------------------------- ubus

fn ubus_entry(body: &str, cfg: &Cfg) -> String {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return json!({"jsonrpc":"2.0","id":null,
                "error":{"code":-32700,"message":format!("请求不是合法 JSON: {e}")}})
            .to_string()
        }
    };
    if let Value::Array(items) = parsed {
        let out: Vec<Value> = items.iter().map(|m| ubus_one(m, cfg)).collect();
        return Value::Array(out).to_string();
    }
    ubus_one(&parsed, cfg).to_string()
}

fn ubus_one(msg: &Value, cfg: &Cfg) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    if method == "list" {
        // 返回对象方法签名（LuCI 的 rpc.list 用）
        let objs: Vec<String> = match &params {
            Value::Array(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => vec![],
        };
        let mut out = Map::new();
        for o in objs {
            out.insert(o.clone(), method_signatures(&o));
        }
        return json!({"jsonrpc":"2.0","id":id,"result":Value::Object(out)});
    }

    let arr = match &params {
        Value::Array(a) => a.clone(),
        _ => vec![],
    };
    if arr.len() < 3 {
        return json!({"jsonrpc":"2.0","id":id,
            "error":{"code":-32602,"message":"params 需要 [session, object, method, args]"}});
    }
    let object = arr[1].as_str().unwrap_or("");
    let m = arr[2].as_str().unwrap_or("");
    let args = arr.get(3).cloned().unwrap_or(json!({}));

    if cfg.debug {
        println!("ubus {object}.{m} <- {}", args);
    }
    match dispatch(object, m, &args, cfg) {
        Ok(v) => {
            if cfg.debug {
                println!("ubus {object}.{m} -> {v}");
            }
            json!({"jsonrpc":"2.0","id":id,"result":[0,v]})
        }
        Err((code, msg)) => {
            if cfg.debug {
                println!("ubus {object}.{m} !! [{code}] {msg}");
            }
            json!({"jsonrpc":"2.0","id":id,"result":[code,msg]})
        }
    }
}

fn method_signatures(obj: &str) -> Value {
    let sig = |args: Value| -> Value {
        let mut m = Map::new();
        m.insert("args".into(), args);
        m.insert("description".into(), json!(""));
        Value::Object(m)
    };
    match obj {
        "mt5700" => json!({
            "at": sig(json!({"cmd":"", "_rid":""})),
            "events": sig(json!({"since":0, "_rid":""})),
            "netrate": sig(json!({"device":""})),
            "usb": sig(json!({"_rid":""})),
            "logs": sig(json!({"since":0,"limit":300,"_rid":""}))
        }),
        "uci" => json!({
            "get": sig(json!({"config":""})),
            "set": sig(json!({"config":"","section":"","values":{}})),
            "delete": sig(json!({"config":"","section":"","options":[]})),
            "add": sig(json!({"config":"","type":"","name":"","values":{}})),
            "order": sig(json!({"config":"","sections":[]})),
            "changes": sig(json!({"config":""})),
            "commit": sig(json!({"config":""})),
            "apply": sig(json!({"timeout":0,"rollback":false})),
            "confirm": sig(json!({}))
        }),
        "file" => json!({
            "read": sig(json!({"path":""})),
            "write": sig(json!({"path":"","data":"","mode":420})),
            "list": sig(json!({"path":""})),
            "stat": sig(json!({"path":""})),
            "remove": sig(json!({"path":""})),
            "exec": sig(json!({"command":"","params":[],"env":[]}))
        }),
        "service" => json!({
            "list": sig(json!({"name":"","instances":[],"type":""})),
            "set": sig(json!({"name":"","action":""})),
            "delete": sig(json!({"name":""}))
        }),
        "rc" => json!({
            "list": sig(json!({})),
            "init": sig(json!({"name":"","action":""}))
        }),
        "log" => json!({"read": sig(json!({"lines":0,"stream":false,"oneshot":false}))}),
        "session" => json!({
            "login": sig(json!({"username":"","password":""})),
            "get": sig(json!({})),
            "access": sig(json!({"scope":"","object":"","function":""})),
            "destroy": sig(json!({}))
        }),
        "luci" => json!({"getFeatures": sig(json!({})), "getVersion": sig(json!({}))}),
        "system" => json!({"board": sig(json!({})), "info": sig(json!({}))}),
        _ => json!({}),
    }
}

type UbErr = (i64, String);
const E_NOTFOUND: i64 = 2;
const E_INVALID: i64 = 4;
const E_PERM: i64 = 6;

fn arg_str(a: &Value, k: &str) -> Option<String> {
    a.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn dispatch(object: &str, m: &str, a: &Value, cfg: &Cfg) -> Result<Value, UbErr> {
    match object {
        "session" => match m {
            "login" => Ok(json!({
                "ubus_rpc_session":"debian", "expires": 0, "timeout": 0,
                "acls": {"access-group": {"superuser": ["*"]}, "ubus": {"*": ["*"]}, "uci": {"*": ["*"]},
                         "file": {"*": ["*"]}, "session": {"*": ["*"]}, "mt5700": {"*": ["*"]}}
            })),
            "get" => Ok(json!({
                "ubus_rpc_session":"debian", "timeout": 0, "expires": 0,
                "acls": {"access-group": {"superuser": ["*"]}, "ubus": {"*": ["*"]}}
            })),
            "access" => Ok(json!({"access": true})),
            "destroy" => Ok(json!({})),
            _ => Err((E_NOTFOUND, format!("session.{m} 未实现"))),
        },
        "system" => match m {
            "board" => Ok(json!({
                "hostname": read_sysfs("/proc/sys/kernel/hostname").unwrap_or_else(|| "h5000m".into()),
                "model": "Hiveton H5000M",
                "system": "Debian GNU/Linux 13 (aarch64)",
                "release": {"distribution":"Debian","version":"13","revision":"aarch64","target":"mediatek/mt7987"},
                "rootfs_type": "ext4"
            })),
            "info" => Ok(json!({
                "uptime": read_sysfs("/proc/uptime").and_then(|s| s.split_whitespace().next().map(|x| x.to_string())).unwrap_or_default(),
                "localtime": 0
            })),
            _ => Err((E_NOTFOUND, format!("system.{m} 未实现"))),
        },
        "luci" => match m {
            "getFeatures" => Ok(json!({
                "fs": {"exec": true, "list": true, "read": true, "write": true},
                "uci": {"configdirs": [UCI_DIR]}
            })),
            "getVersion" => Ok(json!({"version": "24.10", "branch": "debian-port"})),
            _ => Err((E_NOTFOUND, format!("luci.{m} 未实现"))),
        },
        "uci" => uci_rpc(m, a),
        "file" => file_rpc(m, a),
        "mt5700" => mt5700_rpc(m, a, cfg),
        "service" | "rc" => service_rpc(object, m, a),
        "log" => log_rpc(m, a),
        _ => Err((E_NOTFOUND, format!("未知对象 {object}"))),
    }
}

// -------- uci

fn uci_rpc(m: &str, a: &Value) -> Result<Value, UbErr> {
    let pkg = arg_str(a, "config").unwrap_or_default();
    match m {
        "get" => {
            // 兼容 {config} 与 {config:[..]} 两种形式
            let pkgs: Vec<String> = match a.get("config") {
                Some(Value::Array(v)) => v
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect(),
                Some(Value::String(s)) => vec![s.clone()],
                _ => vec![],
            };
            if pkgs.len() > 1 {
                let mut values = Map::new();
                for p in pkgs {
                    values.insert(p.clone(), uci_config_json(&p)?);
                }
                return Ok(json!({"values": values}));
            }
            let p = pkgs.first().cloned().unwrap_or(pkg);
            let sections = match uci_load(&p) {
                Ok(s) => s,
                Err(e) if !std::path::Path::new(UCI_DIR).join(&p).exists() => {
                    let _ = e;
                    Vec::new()
                }
                Err(e) => return Err((E_INVALID, e)),
            };
            // 命中单个 section/option 时按 rpcd 语义返回 value
            if let Some(sec) = arg_str(a, "section") {
                if let Some(opt) = arg_str(a, "option") {
                    if let Some(s) = sections.iter().find(|s| s.name == sec) {
                        if let Some(v) = s.get(&opt) {
                            let val = if v.len() == 1 { json!(v[0]) } else { json!(v) };
                            return Ok(json!({"value": val}));
                        }
                    }
                    return Ok(json!({}));
                }
            }
            let mut values = Map::new();
            for s in &sections {
                values.insert(s.name.clone(), s.to_json());
            }
            Ok(json!({"values": values}))
        }
        "set" => {
            let sec_name = arg_str(a, "section").ok_or((E_INVALID, "缺少 section".into()))?;
            let vals = a.get("values").cloned().unwrap_or(json!({}));
            let mut sections = uci_load(&pkg).unwrap_or_default();
            let mut new_type = None;
            if let Some(t) = vals.get(".type").and_then(|v| v.as_str()) {
                new_type = Some(t.to_string());
            }
            let idx = sections.iter().position(|s| s.name == sec_name);
            let idx = match idx {
                Some(i) => i,
                None => {
                    sections.push(Section {
                        stype: new_type.clone().unwrap_or_else(|| pkg.clone()),
                        name: sec_name.clone(),
                        anonymous: false,
                        options: Vec::new(),
                    });
                    sections.len() - 1
                }
            };
            if let Some(t) = new_type {
                sections[idx].stype = t;
            }
            if let Value::Object(map) = &vals {
                for (k, v) in map {
                    if k.starts_with('.') {
                        continue;
                    }
                    match v {
                        Value::Array(items) => {
                            let list: Vec<String> = items
                                .iter()
                                .map(|x| match x {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .collect();
                            sections[idx].set(k, list);
                        }
                        Value::Null => sections[idx].del(k),
                        Value::String(s) => sections[idx].set(k, vec![s.clone()]),
                        other => sections[idx].set(k, vec![other.to_string()]),
                    }
                }
            }
            uci_save(&pkg, &sections).map_err(|e| (E_INVALID, e))?;
            Ok(json!({}))
        }
        "delete" => {
            let sec_name = arg_str(a, "section").unwrap_or_default();
            let mut sections = uci_load(&pkg).unwrap_or_default();
            let opts = a.get("options");
            match opts {
                Some(Value::Array(items)) if !items.is_empty() => {
                    if let Some(s) = sections.iter_mut().find(|s| s.name == sec_name) {
                        for o in items {
                            if let Some(name) = o.as_str() {
                                s.del(name);
                            }
                        }
                    }
                }
                _ => {
                    sections.retain(|s| s.name != sec_name);
                }
            }
            uci_save(&pkg, &sections).map_err(|e| (E_INVALID, e))?;
            Ok(json!({}))
        }
        "add" => {
            let stype = arg_str(a, "type").unwrap_or_else(|| pkg.clone());
            let name = match arg_str(a, "name") {
                Some(n) if !n.is_empty() => n,
                _ => {
                    let hex = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos())
                        .unwrap_or(0);
                    format!("cfg{:06x}", hex & 0xffffff)
                }
            };
            let mut sections = uci_load(&pkg).unwrap_or_default();
            let mut sec = Section {
                stype,
                name: name.clone(),
                anonymous: false,
                options: Vec::new(),
            };
            if let Some(Value::Object(map)) = a.get("values") {
                for (k, v) in map {
                    if k.starts_with('.') {
                        continue;
                    }
                    match v {
                        Value::String(s) => sec.set(k, vec![s.clone()]),
                        other => sec.set(k, vec![other.to_string()]),
                    }
                }
            }
            sections.push(sec);
            uci_save(&pkg, &sections).map_err(|e| (E_INVALID, e))?;
            Ok(json!({"section": name}))
        }
        "order" => {
            let order: Vec<String> = match a.get("sections") {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect(),
                _ => vec![],
            };
            if !order.is_empty() {
                let mut sections = uci_load(&pkg).unwrap_or_default();
                sections.sort_by_key(|s| {
                    order
                        .iter()
                        .position(|n| n == &s.name)
                        .unwrap_or(usize::MAX)
                });
                uci_save(&pkg, &sections).map_err(|e| (E_INVALID, e))?;
            }
            Ok(json!({}))
        }
        "changes" => Ok(json!({"changes": {}})),
        "commit" | "apply" | "confirm" | "rollback" | "revert" => Ok(json!({})),
        "state" | "configs" => {
            let mut values = Map::new();
            if let Ok(rd) = std::fs::read_dir(UCI_DIR) {
                for e in rd.flatten() {
                    if let Some(n) = e.file_name().to_str() {
                        if let Ok(sections) = uci_load(n) {
                            let mut m = Map::new();
                            for s in &sections {
                                m.insert(s.name.clone(), s.to_json());
                            }
                            values.insert(n.to_string(), Value::Object(m));
                        }
                    }
                }
            }
            Ok(json!({"values": values}))
        }
        _ => Err((E_NOTFOUND, format!("uci.{m} 未实现"))),
    }
}

fn uci_config_json(pkg: &str) -> Result<Value, UbErr> {
    // 配置包不存在（如 LuCI 自身探测 luci 包）不算错误，返回空集合
    let sections = match uci_load(pkg) {
        Ok(s) => s,
        Err(_) => Vec::new(),
    };
    let mut m = Map::new();
    for s in &sections {
        m.insert(s.name.clone(), s.to_json());
    }
    Ok(Value::Object(m))
}

// -------- file

fn safe_write_path(p: &str) -> bool {
    WRITE_PREFIXES.iter().any(|pre| p.starts_with(pre))
}

fn file_stat_json(path: &Path) -> Map<String, Value> {
    let mut m = Map::new();
    if let Ok(md) = std::fs::metadata(path) {
        let ft = md.file_type();
        m.insert(
            "type".into(),
            json!(if ft.is_dir() {
                "directory"
            } else if ft.is_file() {
                "file"
            } else if ft.is_symlink() {
                "link"
            } else {
                "unknown"
            }),
        );
        m.insert("size".into(), json!(md.len()));
        m.insert("mode".into(), json!(mode_of(&md)));
        m.insert("inode".into(), json!(inode_of(&md)));
        m.insert("uid".into(), json!(uid_of(&md)));
        m.insert("gid".into(), json!(gid_of(&md)));
        for (k, v) in [("atime", atime(&md)), ("mtime", mtime(&md)), ("ctime", ctime(&md))] {
            m.insert(k.into(), json!(v));
        }
    } else {
        m.insert("type".into(), json!("broken"));
    }
    m
}

#[cfg(unix)]
fn mode_of(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.mode()
}
#[cfg(unix)]
fn inode_of(md: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.ino()
}
#[cfg(unix)]
fn uid_of(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.uid()
}
#[cfg(unix)]
fn gid_of(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.gid()
}
#[cfg(unix)]
fn atime(md: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    md.atime()
}
#[cfg(unix)]
fn mtime(md: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    md.mtime()
}
#[cfg(unix)]
fn ctime(md: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    md.ctime()
}

fn file_rpc(m: &str, a: &Value) -> Result<Value, UbErr> {
    let path = arg_str(a, "path").unwrap_or_default();
    match m {
        "read" => match std::fs::read_to_string(&path) {
            Ok(t) => Ok(json!({"data": t})),
            Err(_) => {
                // 二进制文件退回 lossy（前端按文本处理）
                match std::fs::read(&path) {
                    Ok(b) => Ok(json!({"data": String::from_utf8_lossy(&b).to_string()})),
                    Err(e) => Err((E_NOTFOUND, format!("读取 {path} 失败: {e}"))),
                }
            }
        },
        "write" => {
            if !safe_write_path(&path) {
                return Err((E_PERM, format!("不允许写入 {path}")));
            }
            let data = arg_str(a, "data").unwrap_or_default();
            let append = a.get("append").and_then(|v| v.as_bool()).unwrap_or(false);
            let r = if append {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .and_then(|mut f| f.write_all(data.as_bytes()))
            } else {
                std::fs::write(&path, data.as_bytes())
            };
            r.map_err(|e| (E_INVALID, format!("写入 {path} 失败: {e}")))?;
            Ok(json!({"path": path}))
        }
        "list" => {
            let mut entries = Vec::new();
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    for e in rd.flatten() {
                        let mut m = file_stat_json(&e.path());
                        m.insert(
                            "name".into(),
                            json!(e.file_name().to_string_lossy().to_string()),
                        );
                        entries.push(Value::Object(m));
                    }
                }
                Err(e) => return Err((E_NOTFOUND, format!("列目录 {path} 失败: {e}"))),
            }
            Ok(json!({"entries": entries}))
        }
        "stat" => Ok(Value::Object(file_stat_json(Path::new(&path)))),
        "remove" => {
            if !safe_write_path(&path) {
                return Err((E_PERM, format!("不允许删除 {path}")));
            }
            let md = std::fs::metadata(&path).map_err(|e| (E_NOTFOUND, e.to_string()))?;
            let r = if md.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            r.map_err(|e| (E_INVALID, e.to_string()))?;
            Ok(json!({"path": path}))
        }
        "md5" => {
            // 简化：用 sha256sum 不可得时返回未实现（前端极少使用）
            Err((E_NOTFOUND, "md5 未实现".into()))
        }
        "exec" => {
            let cmd = arg_str(a, "command").unwrap_or_default();
            let base = Path::new(&cmd)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if !EXEC_ALLOW.contains(&base.as_str()) {
                return Err((E_PERM, format!("命令 {cmd} 不在白名单内")));
            }
            let mut c = std::process::Command::new(&cmd);
            if let Some(Value::Array(ps)) = a.get("params") {
                for p in ps {
                    if let Some(s) = p.as_str() {
                        c.arg(s);
                    }
                }
            }
            match c.output() {
                Ok(o) => Ok(json!({
                    "code": o.status.code().unwrap_or(-1),
                    "stdout": String::from_utf8_lossy(&o.stdout).to_string(),
                    "stderr": String::from_utf8_lossy(&o.stderr).to_string()
                })),
                Err(e) => Err((E_INVALID, format!("执行失败: {e}"))),
            }
        }
        _ => Err((E_NOTFOUND, format!("file.{m} 未实现"))),
    }
}

// -------- mt5700（移植自项目 mt5700.uc）

fn uci_get_opt(pkg: &str, sec: &str, opt: &str) -> Option<String> {
    let sections = uci_load(pkg).ok()?;
    let s = sections.iter().find(|s| s.name == sec)?;
    s.get(opt).and_then(|v| v.first().cloned())
}

fn rpc_port(cfg: &Cfg) -> (String, String) {
    let port = uci_get_opt("at-webserver", "config", "websocket_port")
        .unwrap_or_else(|| "8765".into());
    let key = uci_get_opt("at-webserver", "config", "websocket_auth_key").unwrap_or_default();
    match port.parse::<u16>() {
        Ok(p) => (format!("127.0.0.1:{p}"), key),
        Err(_) => (cfg.rpc.clone(), key),
    }
}

fn mt5700_rpc(m: &str, a: &Value, cfg: &Cfg) -> Result<Value, UbErr> {
    match m {
        "at" => {
            let cmd = arg_str(a, "cmd").unwrap_or_default();
            if cmd.is_empty() {
                return Ok(json!({"success": false, "error": "缺少参数 cmd"}));
            }
            let (addr, key) = rpc_port(cfg);
            let mut params = json!({"cmd": cmd});
            if !key.is_empty() {
                params["auth_key"] = json!(key);
            }
            let payload = json!({"id":1,"method":"at","params":params}).to_string();
            Ok(proxy_result(&addr, &payload))
        }
        "events" => {
            let since = a.get("since").and_then(|v| v.as_i64()).unwrap_or(0).max(0);
            let (addr, key) = rpc_port(cfg);
            let mut params = json!({"since": since});
            if !key.is_empty() {
                params["auth_key"] = json!(key);
            }
            let payload = json!({"id":1,"method":"events","params":params}).to_string();
            Ok(proxy_result(&addr, &payload))
        }
        "logs" => {
            let since = a.get("since").and_then(|v| v.as_i64()).unwrap_or(0).max(0);
            let mut limit = a.get("limit").and_then(|v| v.as_i64()).unwrap_or(300);
            if limit <= 0 || limit > 1200 {
                limit = 300;
            }
            let (addr, key) = rpc_port(cfg);
            let mut params = json!({"since": since, "limit": limit});
            if !key.is_empty() {
                params["auth_key"] = json!(key);
            }
            let payload = json!({"id":1,"method":"logs","params":params}).to_string();
            Ok(proxy_result(&addr, &payload))
        }
        "netrate" => {
            let mut dev = arg_str(a, "device").unwrap_or_default();
            if dev.is_empty() {
                dev = detect_modem_device();
            }
            let rx = read_sysfs(&format!("/sys/class/net/{dev}/statistics/rx_bytes"));
            let tx = read_sysfs(&format!("/sys/class/net/{dev}/statistics/tx_bytes"));
            if rx.is_none() && tx.is_none() {
                return Ok(json!({"success": false, "device": dev,
                    "error": "读不到接口计数器，设备可能不存在或未 up"}));
            }
            Ok(json!({
                "success": true, "device": dev,
                "rx_bytes": rx.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0),
                "tx_bytes": tx.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
            }))
        }
        "usb" => {
            let mut speed = String::new();
            let mut product = String::new();
            let mut version = String::new();
            if let Ok(rd) = std::fs::read_dir("/sys/bus/usb/devices") {
                let mut names: Vec<String> = rd
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                names.sort();
                for n in names {
                    let base = format!("/sys/bus/usb/devices/{n}");
                    let p = match read_sysfs(&format!("{base}/product")) {
                        Some(p) => p,
                        None => continue,
                    };
                    let v = read_sysfs(&format!("{base}/idVendor")).unwrap_or_default();
                    if v == "1d6b" {
                        continue;
                    }
                    speed = read_sysfs(&format!("{base}/speed")).unwrap_or_default();
                    product = p;
                    version = read_sysfs(&format!("{base}/version")).unwrap_or_default();
                    break;
                }
            }
            if speed.is_empty() {
                return Ok(json!({"success": false, "error": "未检测到 USB 模组设备"}));
            }
            let mbps = speed.parse::<f64>().unwrap_or(0.0) as i64;
            Ok(json!({"success": true, "speed_mbps": mbps, "product": product, "version": version}))
        }
        _ => Err((E_NOTFOUND, format!("mt5700.{m} 未实现"))),
    }
}

fn proxy_result(addr: &str, payload: &str) -> Value {
    match rpc_backend(addr, payload) {
        Ok(line) => {
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    return json!({"success": false,
                        "error": format!("后端应答解析失败: {e}；原文: {}", &line[..line.len().min(120)])})
                }
            };
            if let Some(err) = v.get("error") {
                if !err.is_null() {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("RPC 错误");
                    return json!({"success": false, "error": msg});
                }
            }
            v.get("result").cloned().unwrap_or(json!({}))
        }
        Err(e) => json!({"success": false, "error": e}),
    }
}

/// 载流设备识别：先看 uci network.MT5700M，再按名字特征挑 USB 网卡。
fn detect_modem_device() -> String {
    for opt in ["device", "ifname"] {
        if let Some(v) = uci_get_opt("network", "MT5700M", opt) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    let mut best = String::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/net") {
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("enx") || n.starts_with("wwan") || n.starts_with("usb"))
            .collect();
        names.sort();
        for n in names {
            let carrier = read_sysfs(&format!("/sys/class/net/{n}/carrier")).unwrap_or_default();
            if carrier == "1" {
                return n;
            }
            if best.is_empty() {
                best = n;
            }
        }
    }
    if !best.is_empty() {
        return best;
    }
    "eth2".into()
}

// -------- service / rc（映射到 systemd）

const MANAGED_UNITS: &[&str] = &["at-webserver", "mt5700-web"];

fn systemctl(args: &[&str]) -> (i32, String, String) {
    match std::process::Command::new("systemctl").args(args).output() {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (-1, String::new(), e.to_string()),
    }
}

fn unit_state(unit: &str) -> Value {
    let (_, active, _) = systemctl(&["is-active", unit]);
    let (_, enabled, _) = systemctl(&["is-enabled", unit]);
    let (_, pid_out, _) = systemctl(&["show", "-p", "MainPID", "--value", unit]);
    let pid: i64 = pid_out.trim().parse().unwrap_or(0);
    let running = active.trim() == "active";
    json!({
        "instances": {
            unit: { "running": running, "pid": pid }
        },
        "enabled": enabled.trim() == "enabled",
        "running": running
    })
}

fn service_rpc(object: &str, m: &str, a: &Value) -> Result<Value, UbErr> {
    match (object, m) {
        ("service", "list") | ("rc", "list") => {
            let name = arg_str(a, "name").unwrap_or_default();
            let mut out = Map::new();
            let units: Vec<&str> = if name.is_empty() {
                MANAGED_UNITS.to_vec()
            } else if MANAGED_UNITS.contains(&name.as_str()) {
                vec![MANAGED_UNITS.iter().find(|u| **u == name).unwrap()]
            } else {
                return Ok(json!({}));
            };
            for u in units {
                out.insert(u.to_string(), unit_state(u));
            }
            Ok(Value::Object(out))
        }
        ("service", "set") | ("rc", "init") => {
            let name = arg_str(a, "name").unwrap_or_default();
            let action = arg_str(a, "action").unwrap_or_default();
            if !MANAGED_UNITS.contains(&name.as_str()) {
                return Err((E_PERM, format!("不允许操作服务 {name}")));
            }
            let (code, _, err) = match action.as_str() {
                "start" => systemctl(&["start", &name]),
                "stop" => systemctl(&["stop", &name]),
                "restart" => systemctl(&["restart", &name]),
                "enable" => systemctl(&["enable", &name]),
                "disable" => systemctl(&["disable", &name]),
                "reload" => systemctl(&["reload", &name]),
                other => return Err((E_INVALID, format!("不支持的动作 {other}"))),
            };
            if code != 0 {
                return Err((E_INVALID, format!("systemctl {action} {name} 失败: {err}")));
            }
            Ok(json!({}))
        }
        ("service", "delete") => {
            let name = arg_str(a, "name").unwrap_or_default();
            if !MANAGED_UNITS.contains(&name.as_str()) {
                return Err((E_PERM, format!("不允许操作服务 {name}")));
            }
            let _ = systemctl(&["stop", &name]);
            let (_, _, err) = systemctl(&["disable", &name]);
            Ok(json!({"error": err}))
        }
        _ => Err((E_NOTFOUND, format!("{object}.{m} 未实现"))),
    }
}

// -------- log（读日志文件/内核日志）

fn log_rpc(m: &str, a: &Value) -> Result<Value, UbErr> {
    if m != "read" {
        return Err((E_NOTFOUND, format!("log.{m} 未实现")));
    }
    let lines = a.get("lines").and_then(|v| v.as_i64()).unwrap_or(1000).max(1) as usize;
    let stream = a.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    // 优先读本项目通知日志，其次 journalctl
    let mut text = String::new();
    if let Ok(t) = std::fs::read_to_string("/tmp/at-notifications.log") {
        text.push_str(&t);
    }
    let (_, out, _) = systemctl(&[
        "-u",
        "at-webserver",
        "-u",
        "mt5700-web",
        "-n",
        &lines.to_string(),
        "--no-pager",
        "--output=short-iso",
    ]);
    if !out.is_empty() {
        text.push_str(&out);
    }
    // 反向：取最后 lines 行
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    let tail = all[start..].join("\n");
    Ok(json!({"log": tail, "tail": true, "stream": stream, "oneshot": true}))
}

// ---------------------------------------------------------------- 主循环

mod dummy {}
