//! core-plugin-manager：插件管理（`docs/06` §4.3）。界面数据与操作全部经
//! 管理端口（本机 + `session.json` 口令，与 `ling` 命令行同一协议）转发内核。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use ling_sdk::{json, CallError, Context, Plugin, Value};

pub struct CorePluginManager {
    data_root: PathBuf,
}

impl Plugin for CorePluginManager {
    fn id(&self) -> &str {
        "core-plugin-manager"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ling_sdk::ActivateError> {
        self.data_root = ctx
            .data_dir()
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .ok_or_else(|| ling_sdk::ActivateError::new("无法推导数据根目录"))?
            .to_path_buf();

        let pairs: &[(&str, &str)] = &[
            ("core-plugin-manager.install", "kernel.plugins.install"),
            ("core-plugin-manager.uninstall", "kernel.plugins.uninstall"),
            ("core-plugin-manager.enable", "kernel.plugins.enable"),
            ("core-plugin-manager.disable", "kernel.plugins.disable"),
            ("core-plugin-manager.reload", "kernel.plugins.reload"),
            ("core-plugin-manager.restart", "kernel.plugins.restart"),
            (
                "core-plugin-manager.builtin.install",
                "kernel.plugins.builtin.install",
            ),
            (
                "core-plugin-manager.caps.revoke",
                "kernel.plugins.revoke_capability",
            ),
            (
                "core-plugin-manager.caps.grant",
                "kernel.plugins.grant_capability",
            ),
        ];
        for (name, kernel_method) in pairs {
            let data_root = self.data_root.clone();
            let kernel_method = kernel_method.to_string();
            ctx.register_method(
                name,
                Box::new(move |args| {
                    admin_raw(&data_root, &kernel_method, args).map(|_| json!("ok"))
                }),
                Box::new(|| {}),
                None,
            )
            .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;
        }

        // 列表类方法直接返回内核应答。
        for (name, kernel_method) in [
            ("core-plugin-manager.list", "kernel.plugins.list"),
            (
                "core-plugin-manager.builtin.available",
                "kernel.plugins.builtin.available",
            ),
        ] {
            let data_root = self.data_root.clone();
            let kernel_method = kernel_method.to_string();
            ctx.register_method(
                name,
                Box::new(move |_| admin_raw(&data_root, &kernel_method, json!({}))),
                Box::new(|| {}),
                None,
            )
            .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;
        }

        // 最近调和步骤（订阅 kernel.plan.step，内存环形缓冲）。
        let recent: std::sync::Arc<std::sync::Mutex<Vec<Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recent_cb = std::sync::Arc::clone(&recent);
        ctx.subscribe(
            "kernel.plan.step",
            Box::new(move |payload| {
                let mut list = recent_cb.lock().unwrap();
                list.push(payload);
                if list.len() > 200 {
                    list.remove(0);
                }
            }),
            Box::new(|| {}),
        )
        .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;
        let recent_read = std::sync::Arc::clone(&recent);
        ctx.register_method(
            "core-plugin-manager.plan.recent",
            Box::new(move |_| Ok(json!({"steps": recent_read.lock().unwrap().clone()}))),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;

        Ok(())
    }

    fn deactivate(&mut self) {}
}

fn admin_raw(data_root: &std::path::Path, method: &str, params: Value) -> Result<Value, CallError> {
    let registry: Value = std::fs::read_to_string(data_root.join("registry.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let port = registry["management_port"].as_u64().ok_or_else(|| {
        CallError::new(
            "core-plugin-manager.no_admin",
            "读不到管理端口（registry.json）",
        )
    })? as u16;
    let token = std::fs::read_to_string(data_root.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["token"].as_str().map(str::to_string))
        .ok_or_else(|| CallError::new("core-plugin-manager.no_token", "读不到会话口令"))?;

    let stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| CallError::new("core-plugin-manager.io", e.to_string()))?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let mut writer = stream
        .try_clone()
        .map_err(|e| CallError::new("core-plugin-manager.io", e.to_string()))?;
    writeln!(writer, "{}", json!({"op": "auth", "token": token}))
        .map_err(|e| CallError::new("core-plugin-manager.io", e.to_string()))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| CallError::new("core-plugin-manager.io", e.to_string()))?;
    let authed: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    if authed["ok"] != json!(true) {
        return Err(CallError::new("E_AUTH", "管理端口认证失败"));
    }
    writeln!(
        writer,
        "{}",
        json!({"id": "1", "method": method, "params": params})
    )
    .map_err(|e| CallError::new("core-plugin-manager.io", e.to_string()))?;
    line.clear();
    reader
        .read_line(&mut line)
        .map_err(|e| CallError::new("core-plugin-manager.io", e.to_string()))?;
    let response: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    if response["ok"] == json!(true) {
        Ok(response["result"].clone())
    } else {
        let code = response["error"]["code"].as_str().unwrap_or("E_INTERNAL");
        let message = response["error"]["message"]
            .as_str()
            .unwrap_or("管理端口调用失败");
        Err(CallError::new(code, message))
    }
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(CorePluginManager {
        data_root: PathBuf::new(),
    }))
}
