//! core-log：日志收集（`docs/06` §4.5）。`core-log.write` 收各插件日志，
//! 按插件分文件（`data/logs/<插件>.log`），单文件 10 MB、保留最近 5 份；
//! `core-log.read` 供查询（`ling logs` 经管理端口 → 本插件）。

use std::io::Write;
use std::path::{Path, PathBuf};

use ling_sdk::{json, CallError, Context, Plugin};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const KEEP: usize = 5;

pub struct CoreLog {
    logs_dir: Option<PathBuf>,
}

impl Plugin for CoreLog {
    fn id(&self) -> &str {
        "core-log"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ling_sdk::ActivateError> {
        let logs_dir = ctx.data_dir().join("logs");
        std::fs::create_dir_all(&logs_dir)
            .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;

        let dir_for_write = logs_dir.clone();
        ctx.register_method(
            "core-log.write",
            Box::new(move |args| {
                let plugin = args["plugin"].as_str().unwrap_or("host").to_string();
                let level = args["level"].as_str().unwrap_or("info").to_string();
                let message = args["message"]
                    .as_str()
                    .ok_or_else(|| CallError::new("core-log.bad_args", "缺少 message"))?;
                write_line(&dir_for_write, &plugin, &level, message)?;
                Ok(json!("ok"))
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;

        let dir_for_read = logs_dir.clone();
        ctx.register_method(
            "core-log.read",
            Box::new(move |args| {
                let plugin = args["plugin"]
                    .as_str()
                    .ok_or_else(|| CallError::new("core-log.bad_args", "缺少 plugin"))?;
                let limit = args["limit"].as_u64().unwrap_or(50).min(1000) as usize;
                let lines = read_tail(&dir_for_read, plugin, limit)?;
                Ok(json!({"plugin": plugin, "lines": lines}))
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;

        let dir_for_rotate = logs_dir.clone();
        ctx.register_method(
            "core-log.rotate",
            Box::new(move |args| {
                let plugin = args["plugin"]
                    .as_str()
                    .ok_or_else(|| CallError::new("core-log.bad_args", "缺少 plugin"))?;
                rotate(&dir_for_rotate, plugin);
                Ok(json!("ok"))
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))?;

        contribute_log_page(&ctx);
        self.logs_dir = Some(logs_dir);
        Ok(())
    }

    fn deactivate(&mut self) {
        self.logs_dir = None;
    }
}

fn log_file(dir: &Path, plugin: &str) -> PathBuf {
    let safe: String = plugin
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{safe}.log"))
}

fn write_line(dir: &Path, plugin: &str, level: &str, message: &str) -> Result<(), CallError> {
    let path = log_file(dir, plugin);
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= MAX_BYTES {
        rotate(dir, plugin);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| CallError::new("core-log.io", e.to_string()))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    writeln!(file, "{ts} [{level}] {message}")
        .map_err(|e| CallError::new("core-log.io", e.to_string()))
}

fn rotate(dir: &Path, plugin: &str) {
    let base = log_file(dir, plugin);
    if !base.exists() {
        return;
    }
    let stem = base
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("plugin.log")
        .to_string();
    // 轮转：.n → .(n+1)；当前 → .1；删除超出保留份数的旧档
    for i in (1..KEEP).rev() {
        let from = dir.join(format!("{stem}.{i}"));
        let to = dir.join(format!("{stem}.{}", i + 1));
        if from.exists() {
            let _ = std::fs::rename(from, to);
        }
    }
    let _ = std::fs::rename(&base, dir.join(format!("{stem}.1")));
    let _ = std::fs::remove_file(dir.join(format!("{stem}.{}", KEEP + 1)));
}

fn read_tail(dir: &Path, plugin: &str, limit: usize) -> Result<Vec<String>, CallError> {
    let path = log_file(dir, plugin);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| CallError::new("core-log.io", e.to_string()))?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let start = lines.len().saturating_sub(limit);
    Ok(lines[start..].to_vec())
}

fn contribute_log_page(ctx: &Context) {
    let asset_dir = ctx.data_dir().join("ui");
    let _ = std::fs::create_dir_all(&asset_dir);
    let _ = std::fs::write(asset_dir.join("index.html"), LOG_HTML);
    let dir = asset_dir.display().to_string();
    let _ = ctx.call(
        "core-ui",
        "core-ui.ui.contribute",
        json!({
            "plugin": "core-log",
            "type": "page",
            "path": "/logs",
            "title": "日志",
            "icon": "📜",
            "order": 400,
            "asset_dir": dir,
        }),
    );
    let _ = ctx.call(
        "core-ui",
        "core-ui.ui.contribute",
        json!({
            "plugin": "core-log",
            "type": "menu",
            "path": "/logs",
            "title": "日志",
            "icon": "📜",
            "order": 400,
        }),
    );
}

const LOG_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>日志</title>
<style>body{font:14px/1.5 system-ui,sans-serif;padding:16px}pre{background:#8881;padding:12px;overflow:auto}</style></head>
<body><h2>日志</h2>
<input id="plugin" placeholder="插件编号（如 core-ui）" value="core-ui">
<button id="load">读取</button>
<pre id="out">—</pre>
<script>
function token(){const q=new URLSearchParams(location.search).get('token');if(q)return q;return localStorage.getItem('ling_token')||''}
async function rpc(plugin,method,params){const res=await fetch('/rpc',{method:'POST',headers:{'Content-Type':'application/json','X-Ling-Token':token()},body:JSON.stringify({id:'1',plugin,method,params:params||{}})});const d=await res.json();if(!d.ok)throw new Error(d.error?d.error.code+': '+d.error.message:'失败');return d.result}
document.getElementById('load').onclick=async()=>{try{const r=await rpc('core-log','core-log.read',{plugin:document.getElementById('plugin').value,limit:200});
document.getElementById('out').textContent=r.lines.length?r.lines.join('\n'):'（无日志）'}catch(e){document.getElementById('out').textContent='读取失败：'+e.message}};
</script></body></html>
"#;

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(CoreLog { logs_dir: None }))
}
