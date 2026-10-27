//! core-ui：界面壳（`docs/06` §4.1）——本机网页服务、`/rpc` 网关、界面贡献
//! 注册与自动撤下。默认安装三件套之一，可卸载（卸掉后命令行与管理端口照常）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ling_sdk::{json, CallError, Context, EventCallback, Plugin, Value};

const UI_PORT_PREFERRED: u16 = 7910;
const UI_PORT_FALLBACK_MAX: u16 = 7920;

#[derive(Clone)]
struct Contribution {
    plugin: String,
    kind: String,
    path: String,
    title: String,
    icon: String,
    order: u32,
    asset_dir: Option<PathBuf>,
}

struct ServerState {
    contributions: Mutex<Vec<Contribution>>,
}

pub struct CoreUi {
    shutdown: Arc<AtomicBool>,
}

impl Plugin for CoreUi {
    fn id(&self) -> &str {
        "core-ui"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ling_sdk::ActivateError> {
        let data_root = ctx
            .data_dir()
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or_else(|| ling_sdk::ActivateError::new("无法推导数据根目录"))?
            .to_path_buf();

        let listener = bind_ui_port().map_err(ling_sdk::ActivateError::new)?;
        let port = listener
            .local_addr()
            .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?
            .port();
        write_ui_port(&data_root, port).map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        self.shutdown = Arc::clone(&shutdown);

        let state = Arc::new(ServerState {
            contributions: Mutex::new(Vec::new()),
        });

        {
            let ctx_srv = ctx.clone();
            let state_srv = Arc::clone(&state);
            let data_root_srv = data_root.clone();
            std::thread::spawn(move || {
                serve_loop(listener, shutdown, ctx_srv, state_srv, data_root_srv);
            });
        }

        register_contribute(&ctx, &state)?;
        register_retract(&ctx, &state)?;
        register_list(&ctx, &state)?;
        watch_plugin_state(&ctx, &state)?;

        Ok(())
    }

    fn deactivate(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

fn bind_ui_port() -> Result<TcpListener, String> {
    for port in UI_PORT_PREFERRED..=UI_PORT_FALLBACK_MAX {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return Ok(listener);
        }
    }
    Err(format!(
        "127.0.0.1:{UI_PORT_PREFERRED}–{UI_PORT_FALLBACK_MAX} 全部被占用"
    ))
}

fn write_ui_port(data_root: &Path, port: u16) -> std::io::Result<()> {
    let path = data_root.join("registry.json");
    let mut registry: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(json!({}));
    if !registry.is_object() {
        registry = json!({});
    }
    registry["ui_port"] = json!(port);
    let text = serde_json::to_string_pretty(&registry).map_err(std::io::Error::other)?;
    let tmp = data_root.join(".registry.json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

fn register_contribute(
    ctx: &Context,
    state: &Arc<ServerState>,
) -> Result<(), ling_sdk::ActivateError> {
    let state = Arc::clone(state);
    ctx.register_method(
        "core-ui.ui.contribute",
        Box::new(move |args| {
            let plugin = args["plugin"]
                .as_str()
                .ok_or_else(|| CallError::new("core-ui.bad_args", "缺少 plugin（贡献者编号）"))?
                .to_string();
            let kind = args["type"].as_str().unwrap_or("page").to_string();
            if kind != "page" && kind != "menu" {
                return Err(CallError::new(
                    "core-ui.bad_args",
                    format!("未知贡献类型：{kind}"),
                ));
            }
            let path = args["path"]
                .as_str()
                .ok_or_else(|| CallError::new("core-ui.bad_args", "缺少 path"))?
                .to_string();
            if !path.starts_with('/') || path.contains("..") {
                return Err(CallError::new(
                    "core-ui.bad_path",
                    "path 必须以 / 开头且不含 ..",
                ));
            }
            let title = args["title"].as_str().unwrap_or(&path).to_string();
            let icon = args["icon"].as_str().unwrap_or("").to_string();
            let order = args["order"].as_u64().unwrap_or(100) as u32;
            let asset_dir = args["asset_dir"].as_str().map(PathBuf::from);
            if kind == "page" && asset_dir.is_none() {
                return Err(CallError::new("core-ui.bad_args", "页面贡献缺少 asset_dir"));
            }
            if let Some(dir) = &asset_dir {
                if !dir.is_dir() {
                    return Err(CallError::new(
                        "core-ui.bad_assets",
                        format!("资源目录不存在：{}", dir.display()),
                    ));
                }
            }
            let mut list = state.contributions.lock().unwrap();
            list.retain(|c| !(c.plugin == plugin && c.path == path));
            list.push(Contribution {
                plugin,
                kind,
                path,
                title,
                icon,
                order,
                asset_dir,
            });
            Ok(json!("ok"))
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_retract(
    ctx: &Context,
    state: &Arc<ServerState>,
) -> Result<(), ling_sdk::ActivateError> {
    let state = Arc::clone(state);
    ctx.register_method(
        "core-ui.ui.retract",
        Box::new(move |args| {
            let plugin = args["plugin"]
                .as_str()
                .ok_or_else(|| CallError::new("core-ui.bad_args", "缺少 plugin"))?
                .to_string();
            let mut list = state.contributions.lock().unwrap();
            let before = list.len();
            match args["path"].as_str() {
                Some(path) => list.retain(|c| !(c.plugin == plugin && c.path == path)),
                None => list.retain(|c| c.plugin != plugin),
            }
            Ok(json!({"removed": before - list.len()}))
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_list(ctx: &Context, state: &Arc<ServerState>) -> Result<(), ling_sdk::ActivateError> {
    let state = Arc::clone(state);
    ctx.register_method(
        "core-ui.ui.list",
        Box::new(move |_| {
            let list = state.contributions.lock().unwrap();
            let mut items: Vec<Value> = list
                .iter()
                .map(|c| {
                    json!({
                        "plugin": c.plugin,
                        "type": c.kind,
                        "path": c.path,
                        "title": c.title,
                        "icon": c.icon,
                        "order": c.order,
                    })
                })
                .collect();
            items.sort_by_key(|v| {
                (
                    v["order"].as_u64().unwrap_or(100),
                    v["title"].as_str().unwrap_or("").to_string(),
                )
            });
            Ok(json!({"contributions": items}))
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn watch_plugin_state(
    ctx: &Context,
    state: &Arc<ServerState>,
) -> Result<(), ling_sdk::ActivateError> {
    let state = Arc::clone(state);
    let cb: EventCallback = Box::new(move |payload| {
        let plugin = payload["plugin"].as_str().unwrap_or("");
        let phase = payload["state"].as_str().unwrap_or("");
        if matches!(phase, "deactivated" | "uninstalled") && !plugin.is_empty() {
            let mut list = state.contributions.lock().unwrap();
            list.retain(|c| c.plugin != plugin);
        }
    });
    ctx.subscribe("kernel.plugin.state", cb, Box::new(|| {}))
        .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn serve_loop(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    ctx: Context,
    state: Arc<ServerState>,
    data_root: PathBuf,
) {
    let _ = listener.set_nonblocking(true);
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let ctx = ctx.clone();
                let state = Arc::clone(&state);
                let data_root = data_root.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, &ctx, &state, &data_root);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    ctx: &Context,
    state: &ServerState,
    data_root: &Path,
) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let (path, _query) = split_target(&target);

    // 根页面和内置静态资源免认证（token-gate 页面需要先看到才能输入 token）
    if method == "GET" || method == "HEAD" {
        if path == "/" {
            let token = read_session_token(data_root);
            let response = respond_html(&mut stream, 200, SHELL_HTML)?;
            if !token.is_empty() {
                let _ = write!(
                    response,
                    "Set-Cookie: ling_token={token}; Path=/; SameSite=Strict\r\n"
                );
            }
            return finish(response, SHELL_HTML.as_bytes());
        }
        if path == "/__ling/app.js" {
            return respond_bytes(
                &mut stream,
                200,
                "application/javascript",
                APP_JS.as_bytes(),
            );
        }
        if path == "/__ling/style.css" {
            return respond_bytes(&mut stream, 200, "text/css", STYLE_CSS.as_bytes());
        }
    }

    let token = read_session_token(data_root);
    let supplied = extract_token(&target, &headers);
    let authed = !token.is_empty() && supplied.as_deref() == Some(token.as_str());

    if !authed {
        return respond_json(
            &mut stream,
            401,
            &json!({"ok": false, "error": {"code": "E_AUTH", "message": "认证失败"}}),
        );
    }

    if method == "POST" && path == "/rpc" {
        let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let id = request["id"].clone();
        let plugin = request["plugin"].as_str().unwrap_or("").to_string();
        let method_name = request["method"].as_str().unwrap_or("").to_string();
        let params = request["params"].clone();
        let response = match ctx.call(&plugin, &method_name, params) {
            Ok(result) => json!({"id": id, "ok": true, "result": result}),
            Err(e) => {
                json!({"id": id, "ok": false, "error": {"code": e.code(), "message": e.message()}})
            }
        };
        return respond_json(&mut stream, 200, &response);
    }

    if method == "GET" || method == "HEAD" {
        if let Some(file) = resolve_page_file(state, path) {
            let bytes = std::fs::read(&file).unwrap_or_default();
            let ctype = match file.extension().and_then(|e| e.to_str()) {
                Some("js") => "application/javascript",
                Some("css") => "text/css",
                Some("json") => "application/json",
                Some("svg") => "image/svg+xml",
                _ => "text/html; charset=utf-8",
            };
            return respond_bytes(&mut stream, 200, ctype, &bytes);
        }
        return respond_bytes(&mut stream, 404, "text/plain; charset=utf-8", b"404");
    }

    respond_bytes(&mut stream, 405, "text/plain; charset=utf-8", b"405")
}

fn resolve_page_file(state: &ServerState, path: &str) -> Option<PathBuf> {
    let list = state.contributions.lock().unwrap();
    for c in list.iter().filter(|c| c.kind == "page") {
        let prefix = format!("{}/", c.path);
        let Some(dir) = &c.asset_dir else { continue };
        if path == c.path || path.starts_with(&prefix) {
            let rel = path
                .strip_prefix(&c.path)
                .unwrap_or("")
                .trim_start_matches('/');
            let rel = if rel.is_empty() { "index.html" } else { rel };
            if rel.contains("..") {
                return None;
            }
            let candidate = dir.join(rel);
            if let (Ok(canon), Ok(base)) = (candidate.canonicalize(), dir.canonicalize()) {
                if canon.starts_with(&base) && canon.is_file() {
                    return Some(canon);
                }
            }
        }
    }
    None
}

fn split_target(target: &str) -> (&str, &str) {
    match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    }
}

fn extract_token(target: &str, headers: &HashMap<String, String>) -> Option<String> {
    if let Some(t) = headers.get("x-ling-token") {
        return Some(t.clone());
    }
    let (_, query) = split_target(target);
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("token=") {
            return Some(v.to_string());
        }
    }
    if let Some(cookie) = headers.get("cookie") {
        for part in cookie.split(';') {
            if let Some(v) = part.trim().strip_prefix("ling_token=") {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn read_session_token(data_root: &Path) -> String {
    std::fs::read_to_string(data_root.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["token"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn respond_json(stream: &mut TcpStream, status: u16, value: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    respond_bytes(stream, status, "application/json", &body)
}

fn respond_bytes(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_html<'a>(
    stream: &'a mut TcpStream,
    status: u16,
    body: &'static str,
) -> std::io::Result<&'a mut TcpStream> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    Ok(stream)
}

fn finish(stream: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    stream.write_all(body)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

const SHELL_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>ling</title>
<link rel="stylesheet" href="/__ling/style.css">
</head>
<body>
<nav id="nav">
  <div class="brand">ling</div>
  <a href="#" data-builtin="plugins" class="active">插件</a>
</nav>
<main>
  <div id="token-gate" hidden>
    <h2>需要会话口令</h2>
    <p>口令在 <code>ling/session.json</code>；粘贴后进入（存 localStorage）。</p>
    <input id="token-input" type="password" placeholder="token">
    <button id="token-save">进入</button>
  </div>
  <iframe id="frame" src="/__ling/loading"></iframe>
  <div id="plugins-view"></div>
</main>
<script src="/__ling/app.js"></script>
</body>
</html>
"##;

const STYLE_CSS: &str = r#"
:root { color-scheme: light dark; }
* { box-sizing: border-box; }
body { margin: 0; font: 14px/1.5 system-ui, sans-serif; display: flex; min-height: 100vh; }
nav { width: 200px; border-right: 1px solid #8884; padding: 12px; display: flex; flex-direction: column; gap: 4px; }
nav .brand { font-weight: 700; font-size: 18px; margin-bottom: 12px; }
nav a { color: inherit; text-decoration: none; padding: 6px 8px; border-radius: 6px; }
nav a.active, nav a:hover { background: #8882; }
main { flex: 1; display: flex; flex-direction: column; }
iframe { flex: 1; border: 0; width: 100%; }
#plugins-view { padding: 16px; overflow: auto; }
#plugins-view table { border-collapse: collapse; width: 100%; }
#plugins-view th, #plugins-view td { border: 1px solid #8884; padding: 6px 10px; text-align: left; }
#plugins-view button { margin-right: 6px; }
#token-gate { padding: 40px; }
#token-gate input { display: block; margin: 12px 0; padding: 6px; width: 280px; }
.badge { display: inline-block; padding: 1px 8px; border-radius: 999px; background: #8883; font-size: 12px; }
"#;

const APP_JS: &str = r#"
const view = document.getElementById('plugins-view');
const frame = document.getElementById('frame');
const gate = document.getElementById('token-gate');

function token() {
  const q = new URLSearchParams(location.search).get('token');
  if (q) { localStorage.setItem('ling_token', q); return q; }
  return localStorage.getItem('ling_token') || '';
}

async function rpc(plugin, method, params) {
  const res = await fetch('/rpc', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Ling-Token': token() },
    body: JSON.stringify({ id: '1', plugin, method, params: params || {} }),
  });
  const data = await res.json();
  if (!data.ok) throw new Error(data.error ? data.error.code + ': ' + data.error.message : '请求失败');
  return data.result;
}

async function showPlugins() {
  frame.style.display = 'none';
  view.style.display = 'block';
  let rows = '<h2>插件</h2>';
  try {
    const list = await rpc('core-plugin-manager', 'core-plugin-manager.list');
    rows += '<table><tr><th>编号</th><th>版本</th><th>状态</th><th>操作</th></tr>';
    for (const p of list.plugins || []) {
      rows += `<tr><td>${p.id}</td><td>${p.version}</td><td><span class="badge">${p.state}</span></td><td>
        <button data-op="disable" data-id="${p.id}">停用</button>
        <button data-op="enable" data-id="${p.id}">启用</button>
        <button data-op="reload" data-id="${p.id}">重载</button>
      </td></tr>`;
    }
    rows += '</table>';
    const avail = await rpc('core-plugin-manager', 'core-plugin-manager.builtin.available');
    if ((avail.plugins || []).length) {
      rows += '<h2>可安装（内置）</h2><table><tr><th>编号</th><th>名称</th><th>说明</th><th></th></tr>';
      for (const b of avail.plugins) {
        rows += `<tr><td>${b.id}</td><td>${b.name}</td><td>${b.description || ''}</td>
          <td><button data-install="${b.id}">安装</button></td></tr>`;
      }
      rows += '</table>';
    }
  } catch (e) {
    rows += `<p>插件管理不可用：${e.message}</p>`;
  }
  view.innerHTML = rows;
  view.querySelectorAll('button[data-op]').forEach(btn => btn.onclick = async () => {
    try { await rpc('core-plugin-manager', 'core-plugin-manager.' + btn.dataset.op, { id: btn.dataset.id }); showPlugins(); }
    catch (e) { alert(e.message); }
  });
  view.querySelectorAll('button[data-install]').forEach(btn => btn.onclick = async () => {
    try { await rpc('core-plugin-manager', 'core-plugin-manager.builtin.install', { id: btn.dataset.install }); showPlugins(); }
    catch (e) { alert(e.message); }
  });
}

function showPage(path) {
  view.style.display = 'none';
  frame.style.display = 'block';
  frame.src = path + '/index.html?token=' + encodeURIComponent(token());
}

async function boot() {
  if (!token()) { gate.hidden = false; frame.style.display = 'none'; view.style.display = 'none'; return; }
  gate.hidden = true;
  const nav = document.getElementById('nav');
  nav.querySelectorAll('a[data-page]').forEach(a => a.remove());
  try {
    const { contributions } = await rpc('core-ui', 'core-ui.ui.list');
    for (const c of contributions.filter(c => c.type === 'menu')) {
      const a = document.createElement('a');
      a.href = '#'; a.dataset.page = c.path;
      a.textContent = (c.icon ? c.icon + ' ' : '') + c.title;
      a.onclick = (e) => { e.preventDefault(); showPage(c.path); setActive(a); };
      nav.appendChild(a);
    }
  } catch (e) { /* 贡献列表失败不阻塞内置页 */ }
  showPlugins();
}

function setActive(el) {
  document.querySelectorAll('nav a').forEach(a => a.classList.remove('active'));
  el.classList.add('active');
}

document.querySelector('nav a[data-builtin="plugins"]').onclick = (e) => {
  e.preventDefault(); showPlugins(); setActive(e.currentTarget);
};
document.getElementById('token-save').onclick = () => {
  localStorage.setItem('ling_token', document.getElementById('token-input').value.trim());
  boot();
};

boot();
"#;

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(CoreUi {
        shutdown: Arc::new(AtomicBool::new(false)),
    }))
}
