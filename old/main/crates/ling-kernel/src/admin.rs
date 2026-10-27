//! 管理端口（`docs/04` §7、`docs-common/01` §2.3）：仅绑定 `127.0.0.1`，
//! 实际端口写 `ling/registry.json`，会话口令写 `ling/session.json`（0600）。
//! 协议：首行 `{"op":"auth","token":…}` 认证，之后每行一个请求
//! `{"id":…,"method":"kernel.*","params":…}`，应答一行
//! `{"id":…,"ok":true,"result":…}` / `{"id":…,"ok":false,"error":{code,message}}`。

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ling_sdk::{codes, json, Value};

use crate::error::KernelError;
use crate::kernel::Kernel;
use crate::registry;

pub struct AdminServer {
    pub port: u16,
    shutdown: Arc<AtomicBool>,
}

impl AdminServer {
    /// 置位后服务端停止接受新连接并退出读循环（`ling serve` 主线程据此退出）。
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

pub fn start(kernel: Arc<Kernel>, port: u16) -> Result<AdminServer, KernelError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| KernelError::io("bind admin port", e.to_string()))?;
    let actual = listener
        .local_addr()
        .map_err(|e| KernelError::io("admin local_addr", e.to_string()))?
        .port();

    let mut stored = registry::read(kernel.data_root());
    stored.management_port = Some(actual);
    registry::write(kernel.data_root(), &stored)
        .map_err(|e| KernelError::io("write registry.json", e.to_string()))?;

    let token = generate_token();
    write_session(kernel.data_root(), &token)?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = conn else { continue };
            let kernel = Arc::clone(&kernel);
            let token = token.clone();
            let flag = Arc::clone(&flag);
            std::thread::spawn(move || handle_conn(stream, kernel, token, flag));
        }
    });

    Ok(AdminServer {
        port: actual,
        shutdown,
    })
}

fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::getrandom(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_session(data_root: &Path, token: &str) -> Result<(), KernelError> {
    let path = data_root.join("session.json");
    let text = json!({"token": token}).to_string();
    std::fs::write(&path, text)
        .map_err(|e| KernelError::io("write session.json", e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| KernelError::io("chmod session.json", e.to_string()))?;
    }
    Ok(())
}

fn read_line(reader: &mut BufReader<TcpStream>, shutdown: &AtomicBool) -> Option<String> {
    let mut line = String::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return None;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => return Some(line),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return None,
        }
    }
}

fn handle_conn(stream: TcpStream, kernel: Arc<Kernel>, token: String, shutdown: Arc<AtomicBool>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);

    let Some(first) = read_line(&mut reader, &shutdown) else {
        return;
    };
    let authed = serde_json::from_str::<Value>(&first)
        .ok()
        .filter(|v| v["op"] == "auth" && v["token"].as_str() == Some(token.as_str()))
        .is_some();
    if !authed {
        let _ = write_response(
            &mut writer,
            &json!({
                "ok": false,
                "error": {"code": codes::E_AUTH, "message": "认证失败"}
            }),
        );
        return;
    }
    if write_response(&mut writer, &json!({"ok": true, "result": "authed"})).is_err() {
        return;
    }

    while !shutdown.load(Ordering::SeqCst) {
        let Some(line) = read_line(&mut reader, &shutdown) else {
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = dispatch(&kernel, &line, &shutdown);
        if write_response(&mut writer, &response).is_err() {
            return;
        }
    }
}

fn write_response(writer: &mut TcpStream, response: &Value) -> std::io::Result<()> {
    writeln!(writer, "{response}")
}

fn dispatch(kernel: &Arc<Kernel>, line: &str, shutdown: &AtomicBool) -> Value {
    let request = serde_json::from_str::<Value>(line).unwrap_or(Value::Null);
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or("").to_string();
    let params = &request["params"];
    let outcome = match method.as_str() {
        "kernel.plugins.list" => Ok(json!({
            "plugins": kernel.plugin_list().iter().map(|p| json!({
                "id": p.id, "version": p.version, "state": p.state
            })).collect::<Vec<_>>()
        })),
        "kernel.plugins.install" => params["path"]
            .as_str()
            .map(Path::new)
            .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.path"))
            .and_then(|p| kernel.install(p).map(|id| json!({"installed": id}))),
        "kernel.plugins.uninstall" => params["id"]
            .as_str()
            .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.id"))
            .and_then(|id| {
                let purge = params["purge"].as_bool().unwrap_or(false);
                kernel
                    .uninstall(id, purge)
                    .map(|r| json!({"uninstalled": id, "complete": r.complete(), "summary": r.summary()}))
            }),
        "kernel.plugins.enable" => params["id"]
            .as_str()
            .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.id"))
            .and_then(|id| kernel.enable(id).map(|()| json!({"enabled": id}))),
        "kernel.plugins.disable" => params["id"]
            .as_str()
            .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.id"))
            .and_then(|id| kernel.disable(id).map(|()| json!({"disabled": id}))),
        "kernel.plugins.reload" => params["id"]
            .as_str()
            .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.id"))
            .and_then(|id| {
                kernel
                    .reload(id)
                    .map(|r| json!({"reloaded": id, "version": r.version, "downstream": r.downstream}))
            }),
        "kernel.plugins.restart" => params["id"]
            .as_str()
            .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.id"))
            .and_then(|id| kernel.restart(id).map(|()| json!({"restarted": id}))),
        "kernel.plugins.revoke_capability" => capability_op(kernel, params, true),
        "kernel.plugins.grant_capability" => capability_op(kernel, params, false),
        "kernel.plugins.builtin.available" => Ok(json!({"plugins": kernel.builtin_available()})),
        "kernel.plugins.builtin.install" => params["id"]
            .as_str()
            .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.id"))
            .and_then(|id| kernel.install_builtin(id).map(|installed| json!({"installed": installed}))),
        "kernel.logs.read" => params["plugin"]
            .as_str()
            .ok_or_else(|| {
                KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.plugin")
            })
            .and_then(|plugin| {
                let limit = params["limit"].as_u64().unwrap_or(50).min(1000);
                kernel
                    .call(
                        "core-log",
                        "core-log.read",
                        json!({"plugin": plugin, "limit": limit}),
                    )
                    .map_err(|e| KernelError::Unsupported(format!("{e}")))
            }),
        "kernel.shutdown" => {
            let stopped = kernel.shutdown().unwrap_or_default();
            shutdown.store(true, Ordering::SeqCst);
            Ok(json!({"stopped": stopped}))
        }
        _ => Err(KernelError::Unsupported(format!(
            "未知方法：{method}"
        ))),
    };
    match outcome {
        Ok(result) => json!({"id": id, "ok": true, "result": result}),
        Err(e) => {
            let code = e.ecode().unwrap_or(match &e {
                KernelError::NotFound(_) => codes::E_PLUGIN_MISSING,
                KernelError::AlreadyInstalled(_) => "E_ALREADY_INSTALLED",
                KernelError::AlreadyLoaded(_) => "E_ALREADY_LOADED",
                _ => "E_INTERNAL",
            });
            json!({"id": id, "ok": false, "error": {"code": code, "message": e.to_string()}})
        }
    }
}

fn capability_op(kernel: &Arc<Kernel>, params: &Value, revoke: bool) -> Result<Value, KernelError> {
    let plugin = params["plugin"]
        .as_str()
        .ok_or_else(|| KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.plugin"))?;
    let cap_name = params["capability"].as_str().ok_or_else(|| {
        KernelError::manifest(codes::E_MANIFEST_INVALID, "缺少 params.capability")
    })?;
    let cap = ling_sdk::Capability::parse(cap_name).ok_or_else(|| {
        KernelError::manifest(codes::E_CAPABILITY_UNKNOWN, format!("未知能力：{cap_name}"))
    })?;
    if revoke {
        kernel.revoke_capability(plugin, cap)?;
    } else {
        kernel.grant_capability(plugin, cap)?;
    }
    Ok(json!({
        "plugin": plugin,
        "capability": cap_name,
        "revoked": revoke,
    }))
}
