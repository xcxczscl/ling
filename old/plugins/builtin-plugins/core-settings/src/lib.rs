//! core-settings：配置存储 + 设置界面（`docs/06` §4.2）。每个插件的设置存
//! 自己的 SQLite（`data/settings.db`），设置项带模式（类型/默认值/取值范围），
//! `set` 时校验。设置页框架经 core-ui 贡献挂载。

use std::path::Path;
use std::sync::{Arc, Mutex};

use ling_sdk::{json, CallError, Context, Plugin, Value};
use rusqlite::{params, Connection, OptionalExtension};

pub struct CoreSettings {
    conn: Option<Arc<Mutex<Connection>>>,
}

impl Plugin for CoreSettings {
    fn id(&self) -> &str {
        "core-settings"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ling_sdk::ActivateError> {
        let db_path = ctx.data_dir().join("settings.db");
        let conn = Arc::new(Mutex::new(
            open_db(&db_path).map_err(ling_sdk::ActivateError::new)?,
        ));

        register_define(&ctx, &conn)?;
        register_get(&ctx, &conn)?;
        register_set(&ctx, &conn)?;
        register_list(&ctx, &conn)?;
        contribute_settings_page(&ctx);

        self.conn = Some(conn);
        Ok(())
    }

    fn deactivate(&mut self) {
        self.conn = None;
    }
}

fn open_db(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema (
            plugin TEXT NOT NULL,
            key    TEXT NOT NULL,
            type   TEXT NOT NULL,
            default_json TEXT NOT NULL,
            min REAL,
            max REAL,
            PRIMARY KEY (plugin, key)
        );
        CREATE TABLE IF NOT EXISTS kv (
            plugin TEXT NOT NULL,
            key    TEXT NOT NULL,
            value_json TEXT NOT NULL,
            PRIMARY KEY (plugin, key)
        );",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn define(
    conn: &Mutex<Connection>,
    plugin: &str,
    key: &str,
    schema: &Value,
) -> Result<(), CallError> {
    let kind = schema["type"]
        .as_str()
        .ok_or_else(|| CallError::new("core-settings.bad_schema", "schema.type 缺失"))?;
    if !matches!(kind, "string" | "number" | "boolean") {
        return Err(CallError::new(
            "core-settings.bad_schema",
            format!("未知类型：{kind}"),
        ));
    }
    let default = schema
        .get("default")
        .cloned()
        .ok_or_else(|| CallError::new("core-settings.bad_schema", "schema.default 缺失"))?;
    validate_value(&default, kind, schema)?;
    let conn = conn.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO schema (plugin, key, type, default_json, min, max)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            plugin,
            key,
            kind,
            default.to_string(),
            schema["min"].as_f64(),
            schema["max"].as_f64(),
        ],
    )
    .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
    Ok(())
}

fn get(conn: &Mutex<Connection>, plugin: &str, key: &str) -> Result<Value, CallError> {
    let conn = conn.lock().unwrap();
    let stored: Option<String> = conn
        .query_row(
            "SELECT value_json FROM kv WHERE plugin = ?1 AND key = ?2",
            params![plugin, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
    if let Some(text) = stored {
        return serde_json::from_str(&text)
            .map_err(|e| CallError::new("core-settings.corrupt", e.to_string()));
    }
    let default: Option<String> = conn
        .query_row(
            "SELECT default_json FROM schema WHERE plugin = ?1 AND key = ?2",
            params![plugin, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
    match default {
        Some(text) => serde_json::from_str(&text)
            .map_err(|e| CallError::new("core-settings.corrupt", e.to_string())),
        None => Err(CallError::new(
            "core-settings.undefined",
            format!("未定义的设置项：{plugin}.{key}"),
        )),
    }
}

fn set(conn: &Mutex<Connection>, plugin: &str, key: &str, value: &Value) -> Result<(), CallError> {
    let (kind, schema_json) = {
        let conn = conn.lock().unwrap();
        let row: Option<(String, String, Option<f64>, Option<f64>)> = conn
            .query_row(
                "SELECT type, default_json, min, max FROM schema WHERE plugin = ?1 AND key = ?2",
                params![plugin, key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
        let (kind, default_json, min, max) = row.ok_or_else(|| {
            CallError::new(
                "core-settings.undefined",
                format!("未定义的设置项：{plugin}.{key}"),
            )
        })?;
        let mut schema = serde_json::from_str::<Value>(&default_json)
            .map_err(|e| CallError::new("core-settings.corrupt", e.to_string()))?;
        if !schema.is_object() {
            schema = json!({});
        }
        if let Some(min) = min {
            schema["min"] = json!(min);
        }
        if let Some(max) = max {
            schema["max"] = json!(max);
        }
        (kind, schema)
    };
    validate_value(value, &kind, &schema_json)?;
    let conn = conn.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO kv (plugin, key, value_json) VALUES (?1, ?2, ?3)",
        params![plugin, key, value.to_string()],
    )
    .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
    Ok(())
}

fn list_all(conn: &Mutex<Connection>, plugin: Option<&str>) -> Result<Value, CallError> {
    let conn = conn.lock().unwrap();
    let mut items = Vec::new();
    let mut collect = |rows: Box<dyn Iterator<Item = (String, String, String, String)>>| {
        for (plugin, key, kind, default) in rows {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT value_json FROM kv WHERE plugin = ?1 AND key = ?2",
                    params![plugin, key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
            let value: Value = stored
                .and_then(|t| serde_json::from_str(&t).ok())
                .or_else(|| serde_json::from_str(&default).ok())
                .unwrap_or(Value::Null);
            items.push(json!({"plugin": plugin, "key": key, "type": kind, "value": value}));
        }
        Ok::<(), CallError>(())
    };
    match plugin {
        Some(p) => {
            let mut stmt = conn
                .prepare("SELECT plugin, key, type, default_json FROM schema WHERE plugin = ?1 ORDER BY key")
                .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
            let rows = stmt
                .query_map(params![p], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
            collect(Box::new(rows.flatten()))?;
        }
        None => {
            let mut stmt = conn
                .prepare("SELECT plugin, key, type, default_json FROM schema ORDER BY plugin, key")
                .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map_err(|e| CallError::new("core-settings.io", e.to_string()))?;
            collect(Box::new(rows.flatten()))?;
        }
    }
    Ok(json!({"settings": items}))
}

fn validate_value(value: &Value, kind: &str, schema: &Value) -> Result<(), CallError> {
    let bad = |msg: String| CallError::new("core-settings.schema", msg);
    match kind {
        "string" => {
            value.as_str().ok_or_else(|| bad("期望 string".into()))?;
        }
        "number" => {
            let n = value.as_f64().ok_or_else(|| bad("期望 number".into()))?;
            if let Some(min) = schema["min"].as_f64() {
                if n < min {
                    return Err(bad(format!("{n} 小于下限 {min}")));
                }
            }
            if let Some(max) = schema["max"].as_f64() {
                if n > max {
                    return Err(bad(format!("{n} 大于上限 {max}")));
                }
            }
        }
        "boolean" => {
            value.as_bool().ok_or_else(|| bad("期望 boolean".into()))?;
        }
        other => return Err(bad(format!("未知类型：{other}"))),
    }
    Ok(())
}

fn register_define(
    ctx: &Context,
    conn: &Arc<Mutex<Connection>>,
) -> Result<(), ling_sdk::ActivateError> {
    let conn = Arc::clone(conn);
    ctx.register_method(
        "core-settings.define",
        Box::new(move |args| {
            let plugin = str_arg(&args, "plugin")?;
            let key = str_arg(&args, "key")?;
            let schema = args
                .get("schema")
                .cloned()
                .ok_or_else(|| CallError::new("core-settings.bad_args", "缺少 schema"))?;
            define(&conn, &plugin, &key, &schema).map(|_| json!("ok"))
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_get(
    ctx: &Context,
    conn: &Arc<Mutex<Connection>>,
) -> Result<(), ling_sdk::ActivateError> {
    let conn = Arc::clone(conn);
    ctx.register_method(
        "core-settings.get",
        Box::new(move |args| {
            let plugin = str_arg(&args, "plugin")?;
            let key = str_arg(&args, "key")?;
            get(&conn, &plugin, &key)
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_set(
    ctx: &Context,
    conn: &Arc<Mutex<Connection>>,
) -> Result<(), ling_sdk::ActivateError> {
    let conn = Arc::clone(conn);
    ctx.register_method(
        "core-settings.set",
        Box::new(move |args| {
            let plugin = str_arg(&args, "plugin")?;
            let key = str_arg(&args, "key")?;
            let value = args
                .get("value")
                .cloned()
                .ok_or_else(|| CallError::new("core-settings.bad_args", "缺少 value"))?;
            set(&conn, &plugin, &key, &value).map(|_| json!("ok"))
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_list(
    ctx: &Context,
    conn: &Arc<Mutex<Connection>>,
) -> Result<(), ling_sdk::ActivateError> {
    let conn = Arc::clone(conn);
    ctx.register_method(
        "core-settings.list",
        Box::new(move |args| list_all(&conn, args["plugin"].as_str())),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn contribute_settings_page(ctx: &Context) {
    let asset_dir = ctx.data_dir().join("ui");
    let _ = std::fs::create_dir_all(&asset_dir);
    let _ = std::fs::write(asset_dir.join("index.html"), SETTINGS_HTML);
    let dir = asset_dir.display().to_string();
    let _ = ctx.call(
        "core-ui",
        "core-ui.ui.contribute",
        json!({
            "plugin": "core-settings",
            "type": "page",
            "path": "/settings",
            "title": "设置",
            "icon": "⚙",
            "order": 200,
            "asset_dir": dir,
        }),
    );
    let _ = ctx.call(
        "core-ui",
        "core-ui.ui.contribute",
        json!({
            "plugin": "core-settings",
            "type": "menu",
            "path": "/settings",
            "title": "设置",
            "icon": "⚙",
            "order": 200,
        }),
    );
}

const SETTINGS_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>设置</title>
<style>body{font:14px/1.5 system-ui,sans-serif;padding:16px}table{border-collapse:collapse;width:100%}
th,td{border:1px solid #8884;padding:6px 10px;text-align:left}input,select{padding:4px}</style></head>
<body><h2>设置</h2><div id="app">加载中…</div>
<script>
function token(){const q=new URLSearchParams(location.search).get('token');if(q)return q;return localStorage.getItem('ling_token')||''}
async function rpc(plugin,method,params){const res=await fetch('/rpc',{method:'POST',headers:{'Content-Type':'application/json','X-Ling-Token':token()},body:JSON.stringify({id:'1',plugin,method,params:params||{}})});const d=await res.json();if(!d.ok)throw new Error(d.error?d.error.code+': '+d.error.message:'失败');return d.result}
(async()=>{try{const {settings}=await rpc('core-settings','core-settings.list');
const app=document.getElementById('app');
if(!settings.length){app.textContent='暂无已声明的设置项';return}
let html='<table><tr><th>插件</th><th>键</th><th>类型</th><th>值</th><th></th></tr>';
for(const s of settings){html+=`<tr><td>${s.plugin}</td><td>${s.key}</td><td>${s.type}</td><td><input data-p="${s.plugin}" data-k="${s.key}" value=${JSON.stringify(String(s.value))}></td><td><button data-p="${s.plugin}" data-k="${s.key}">保存</button></td></tr>`}
html+='</table>';app.innerHTML=html;
app.querySelectorAll('button').forEach(b=>b.onclick=async()=>{const input=app.querySelector(`input[data-p="${b.dataset.p}"][data-k="${b.dataset.k}"]`);let v=input.value;const s=settings.find(x=>x.plugin===b.dataset.p&&x.key===b.dataset.k);if(s.type==='number')v=Number(v);if(s.type==='boolean')v=v==='true';
try{await rpc('core-settings','core-settings.set',{plugin:b.dataset.p,key:b.dataset.k,value:v});alert('已保存')}catch(e){alert(e.message)}});
}catch(e){document.getElementById('app').textContent='加载失败：'+e.message}})();
</script></body></html>
"#;

fn str_arg(args: &Value, key: &str) -> Result<String, CallError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CallError::new("core-settings.bad_args", format!("缺少字符串参数 {key}")))
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(CoreSettings { conn: None }))
}
