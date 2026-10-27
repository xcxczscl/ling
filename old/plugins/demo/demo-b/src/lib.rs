use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};

use ling_sdk::{json, ActivateError, CallError, Capability, Context, Plugin, Value};

pub struct DemoB;

impl Plugin for DemoB {
    fn id(&self) -> &str {
        "demo-b"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError> {
        ctx.register_method(
            "demo-b.add",
            Box::new(|args| {
                let a = int_arg(&args, "a")?;
                let b = int_arg(&args, "b")?;
                Ok(json!(a + b))
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        let ping_ctx = ctx.clone();
        ctx.register_method(
            "demo-b.ping_a",
            Box::new(move |_| ping_ctx.call("demo-a", "demo-a.ping", Value::Null)),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        ctx.register_method(
            "demo-b.dup.delete",
            Box::new(|args| {
                let ids = args
                    .get("placement_ids")
                    .and_then(Value::as_array)
                    .map(|v| v.len())
                    .unwrap_or(0);
                Ok(json!({"deleted": ids}))
            }),
            Box::new(|| {}),
            Some(Capability::Fs),
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        let record_ctx = ctx.clone();
        ctx.register_method(
            "demo-b.record",
            Box::new(move |payload| {
                let dir = record_ctx.data_dir();
                std::fs::create_dir_all(&dir)
                    .map_err(|e| CallError::new("demo-b.io", e.to_string()))?;
                let path = dir.join("events.ndjson");
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| CallError::new("demo-b.io", e.to_string()))?;
                let line = serde_json::to_string(&payload)
                    .map_err(|e| CallError::new("demo-b.io", e.to_string()))?;
                writeln!(file, "{line}").map_err(|e| CallError::new("demo-b.io", e.to_string()))?;
                Ok(json!("ok"))
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        let read_ctx = ctx.clone();
        ctx.register_method(
            "demo-b.read_events",
            Box::new(move |_| {
                let path = read_ctx.data_dir().join("events.ndjson");
                let mut events = Vec::new();
                if path.is_file() {
                    let file = std::fs::File::open(&path)
                        .map_err(|e| CallError::new("demo-b.io", e.to_string()))?;
                    for line in BufReader::new(file).lines() {
                        let line = line.map_err(|e| CallError::new("demo-b.io", e.to_string()))?;
                        if line.trim().is_empty() {
                            continue;
                        }
                        let value: Value = serde_json::from_str(&line)
                            .map_err(|e| CallError::new("demo-b.io", e.to_string()))?;
                        events.push(value);
                    }
                }
                Ok(Value::Array(events))
            }),
            Box::new(|| {}),
            None,
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        let echo_ctx = ctx.clone();
        ctx.subscribe(
            "demo-a.*",
            Box::new(move |payload| {
                let _ = echo_ctx.call("demo-b", "demo-b.record", payload);
            }),
            Box::new(|| {}),
        )
        .map_err(|e| ActivateError::new(e.to_string()))?;

        Ok(())
    }

    fn deactivate(&mut self) {}
}

fn int_arg(args: &Value, key: &str) -> Result<i64, CallError> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| CallError::new("demo-b.bad_args", format!("缺少整数参数 {key}")))
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(DemoB))
}
