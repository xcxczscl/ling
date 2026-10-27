use ling_sdk::{
    json, ActivateError, CallError, Capability, Context, MethodHandler, Plugin, UndoFn, Value,
};

pub struct DemoA;

impl Plugin for DemoA {
    fn id(&self) -> &str {
        "demo-a"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError> {
        ctx.register_method(
            "demo-a.ping",
            Box::new(|_| Ok(json!("pong"))),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        let forward_ctx = ctx.clone();
        ctx.register_method(
            "demo-a.forward",
            Box::new(move |args| {
                let plugin = str_arg(&args, "plugin")?;
                let method = str_arg(&args, "method")?;
                let inner = args.get("args").cloned().unwrap_or(Value::Null);
                forward_ctx.call(&plugin, &method, inner)
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        let emit_ctx = ctx.clone();
        ctx.register_method(
            "demo-a.emit",
            Box::new(move |args| {
                let topic = str_arg(&args, "topic")?;
                let payload = args.get("payload").cloned().unwrap_or(Value::Null);
                emit_ctx
                    .emit(&topic, payload)
                    .map(|_| json!("ok"))
                    .map_err(|e| CallError::new(e.code(), e.message()))
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        let probe_ctx = ctx.clone();
        ctx.register_method(
            "demo-a.probe",
            Box::new(move |args| {
                let kind = str_arg(&args, "kind")?;
                match kind.as_str() {
                    "register" => {
                        let name = str_arg(&args, "name")?;
                        let requires = match args.get("requires").and_then(Value::as_str) {
                            Some("fs") => Some(Capability::Fs),
                            Some("network") => Some(Capability::Network),
                            Some("ui") => Some(Capability::Ui),
                            Some("shell") => Some(Capability::Shell),
                            Some(other) => {
                                return Err(CallError::new(
                                    "demo-a.bad_probe",
                                    format!("未知能力：{other}"),
                                ))
                            }
                            None => None,
                        };
                        let handler: MethodHandler = Box::new(|_| Ok(json!({"probe": "handler"})));
                        let undo: UndoFn = Box::new(|| {});
                        probe_ctx
                            .register_method(&name, handler, undo, requires)
                            .map(|_| json!("ok"))
                            .map_err(|e| CallError::new(e.code(), e.message()))
                    }
                    "emit" => {
                        let topic = str_arg(&args, "topic")?;
                        probe_ctx
                            .emit(&topic, Value::Null)
                            .map(|_| json!("ok"))
                            .map_err(|e| CallError::new(e.code(), e.message()))
                    }
                    other => Err(CallError::new(
                        "demo-a.bad_probe",
                        format!("未知探针：{other}"),
                    )),
                }
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        // 验收 07 §2 L5-5：demo-a 贡献一个测试页（core-ui 未装时静默跳过——
        // L1 验收不依赖界面壳）。资源目录写到自己的 data/ui/。
        let asset_dir = ctx.data_dir().join("ui");
        let _ = std::fs::create_dir_all(&asset_dir);
        let _ = std::fs::write(asset_dir.join("index.html"), DEMO_PAGE_HTML);
        let _ = ctx.call(
            "core-ui",
            "core-ui.ui.contribute",
            json!({
                "plugin": "demo-a",
                "type": "page",
                "path": "/demo-a",
                "title": "示例 A",
                "icon": "🧪",
                "order": 900,
                "asset_dir": asset_dir.display().to_string(),
            }),
        );

        Ok(())
    }

    fn deactivate(&mut self) {}
}

fn str_arg(args: &Value, key: &str) -> Result<String, CallError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CallError::new("demo-a.bad_args", format!("缺少字符串参数 {key}")))
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(DemoA))
}

const DEMO_PAGE_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>示例 A</title>
<style>body{font:14px/1.5 system-ui,sans-serif;padding:16px}button{padding:6px 12px}</style></head>
<body><h2>示例 A 测试页</h2>
<button id="ping">调用 demo-a.ping</button>
<pre id="out">—</pre>
<script>
function token(){const q=new URLSearchParams(location.search).get('token');if(q)return q;return localStorage.getItem('ling_token')||''}
document.getElementById('ping').onclick=async()=>{
  const res=await fetch('/rpc',{method:'POST',headers:{'Content-Type':'application/json','X-Ling-Token':token()},
    body:JSON.stringify({id:'1',plugin:'demo-a',method:'demo-a.ping',params:{}})});
  const d=await res.json();
  document.getElementById('out').textContent=JSON.stringify(d);
};
</script></body></html>
"##;
