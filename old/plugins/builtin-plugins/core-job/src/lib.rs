//! core-job：作业调度（`docs/06` §4.4）。内核不管作业；本插件把"长任务登记"
//! 做成公共服务：登记/进度/取消/查询，进度与完成广播事件，作业数据落 data/。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ling_sdk::{json, CallError, Context, Plugin, Value};

const MAX_JOBS: usize = 200;
const RETENTION_SECS: u64 = 30 * 24 * 3600;

type Jobs = Arc<Mutex<BTreeMap<u64, Value>>>;

pub struct CoreJob {
    store: Option<Jobs>,
}

impl Plugin for CoreJob {
    fn id(&self) -> &str {
        "core-job"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ling_sdk::ActivateError> {
        let path = ctx.data_dir().join("jobs.json");
        let mut jobs: BTreeMap<u64, Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        prune(&mut jobs);
        persist(&path, &jobs);
        let store: Jobs = Arc::new(Mutex::new(jobs));

        register_register(&ctx, &store, &path)?;
        register_progress(&ctx, &store, &path)?;
        register_cancel(&ctx, &store, &path)?;
        register_list(&ctx, &store)?;
        register_get(&ctx, &store)?;
        contribute_job_page(&ctx);

        self.store = Some(store);
        Ok(())
    }

    fn deactivate(&mut self) {
        self.store = None;
    }
}

fn prune(jobs: &mut BTreeMap<u64, Value>) {
    let now = now_secs();
    jobs.retain(|_, job| {
        let started = job["started_at"].as_u64().unwrap_or(now);
        now.saturating_sub(started) <= RETENTION_SECS
    });
    while jobs.len() > MAX_JOBS {
        let oldest = *jobs.keys().next().expect("len > 0");
        jobs.remove(&oldest);
    }
}

fn persist(path: &Path, jobs: &BTreeMap<u64, Value>) {
    let text = serde_json::to_string(jobs).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::write(&tmp, text);
    let _ = std::fs::rename(&tmp, path);
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn register_register(
    ctx: &Context,
    store: &Jobs,
    path: &Path,
) -> Result<(), ling_sdk::ActivateError> {
    let store = Arc::clone(store);
    let path = path.to_path_buf();
    let emit_ctx = ctx.clone();
    ctx.register_method(
        "core-job.register",
        Box::new(move |args| {
            let title = args["title"].as_str().unwrap_or("未命名作业").to_string();
            let owner = args["owner"]
                .as_str()
                .unwrap_or_else(|| emit_ctx.plugin_id())
                .to_string();
            let mut jobs = store.lock().unwrap();
            let job_id = jobs.keys().max().copied().unwrap_or(0) + 1;
            let job = json!({
                "job_id": job_id,
                "owner": owner,
                "title": title,
                "percent": 0,
                "state": "running",
                "started_at": now_secs(),
            });
            jobs.insert(job_id, job.clone());
            persist(&path, &jobs);
            Ok(job)
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_progress(
    ctx: &Context,
    store: &Jobs,
    path: &Path,
) -> Result<(), ling_sdk::ActivateError> {
    let store = Arc::clone(store);
    let path = path.to_path_buf();
    let emit_ctx = ctx.clone();
    ctx.register_method(
        "core-job.progress",
        Box::new(move |args| {
            let job_id = args["job_id"]
                .as_u64()
                .ok_or_else(|| CallError::new("core-job.bad_args", "缺少 job_id"))?;
            let percent = args["percent"].as_f64().unwrap_or(0.0).clamp(0.0, 100.0);
            let done = args["done"].as_bool().unwrap_or(percent >= 100.0);
            let job = {
                let mut jobs = store.lock().unwrap();
                let job = jobs.get_mut(&job_id).ok_or_else(|| {
                    CallError::new("core-job.missing", format!("作业不存在：{job_id}"))
                })?;
                job["percent"] = json!(percent);
                if done {
                    job["state"] = json!("done");
                }
                job.clone()
            };
            persist(&path, &store.lock().unwrap());
            let topic = if done {
                "core-job.done"
            } else {
                "core-job.progress"
            };
            emit_ctx
                .emit(topic, job.clone())
                .map_err(|e| CallError::new(e.code(), e.message()))?;
            Ok(job)
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_cancel(
    ctx: &Context,
    store: &Jobs,
    path: &Path,
) -> Result<(), ling_sdk::ActivateError> {
    let store = Arc::clone(store);
    let path = path.to_path_buf();
    ctx.register_method(
        "core-job.cancel",
        Box::new(move |args| {
            let job_id = args["job_id"]
                .as_u64()
                .ok_or_else(|| CallError::new("core-job.bad_args", "缺少 job_id"))?;
            let mut jobs = store.lock().unwrap();
            let job = jobs.get_mut(&job_id).ok_or_else(|| {
                CallError::new("core-job.missing", format!("作业不存在：{job_id}"))
            })?;
            job["state"] = json!("cancelled");
            let job = job.clone();
            persist(&path, &jobs);
            Ok(job)
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_list(ctx: &Context, store: &Jobs) -> Result<(), ling_sdk::ActivateError> {
    let store = Arc::clone(store);
    ctx.register_method(
        "core-job.list",
        Box::new(move |args| {
            let jobs = store.lock().unwrap();
            let owner = args["owner"].as_str();
            let items: Vec<Value> = jobs
                .values()
                .filter(|j| owner.is_none_or(&|o| j["owner"] == json!(o)))
                .cloned()
                .collect();
            Ok(json!({"jobs": items}))
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn register_get(ctx: &Context, store: &Jobs) -> Result<(), ling_sdk::ActivateError> {
    let store = Arc::clone(store);
    ctx.register_method(
        "core-job.get",
        Box::new(move |args| {
            let job_id = args["job_id"]
                .as_u64()
                .ok_or_else(|| CallError::new("core-job.bad_args", "缺少 job_id"))?;
            let jobs = store.lock().unwrap();
            jobs.get(&job_id)
                .cloned()
                .ok_or_else(|| CallError::new("core-job.missing", format!("作业不存在：{job_id}")))
        }),
        Box::new(|| {}),
        None,
    )
    .map_err(|e| ling_sdk::ActivateError::new(e.to_string()))
}

fn contribute_job_page(ctx: &Context) {
    let asset_dir = ctx.data_dir().join("ui");
    let _ = std::fs::create_dir_all(&asset_dir);
    let _ = std::fs::write(asset_dir.join("index.html"), JOB_HTML);
    let dir = asset_dir.display().to_string();
    let _ = ctx.call(
        "core-ui",
        "core-ui.ui.contribute",
        json!({
            "plugin": "core-job",
            "type": "page",
            "path": "/jobs",
            "title": "作业",
            "icon": "⏳",
            "order": 300,
            "asset_dir": dir,
        }),
    );
    let _ = ctx.call(
        "core-ui",
        "core-ui.ui.contribute",
        json!({
            "plugin": "core-job",
            "type": "menu",
            "path": "/jobs",
            "title": "作业",
            "icon": "⏳",
            "order": 300,
        }),
    );
}

const JOB_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>作业</title>
<style>body{font:14px/1.5 system-ui,sans-serif;padding:16px}table{border-collapse:collapse;width:100%}
th,td{border:1px solid #8884;padding:6px 10px;text-align:left}progress{width:120px}</style></head>
<body><h2>作业</h2><div id="app">加载中…</div>
<script>
function token(){const q=new URLSearchParams(location.search).get('token');if(q)return q;return localStorage.getItem('ling_token')||''}
async function rpc(plugin,method,params){const res=await fetch('/rpc',{method:'POST',headers:{'Content-Type':'application/json','X-Ling-Token':token()},body:JSON.stringify({id:'1',plugin,method,params:params||{}})});const d=await res.json();if(!d.ok)throw new Error(d.error?d.error.code+': '+d.error.message:'失败');return d.result}
async function load(){try{const {jobs}=await rpc('core-job','core-job.list',{});const app=document.getElementById('app');
if(!jobs.length){app.textContent='暂无作业';return}
let html='<table><tr><th>#</th><th>所属</th><th>标题</th><th>进度</th><th>状态</th></tr>';
for(const j of jobs){html+=`<tr><td>${j.job_id}</td><td>${j.owner}</td><td>${j.title}</td><td><progress max="100" value="${j.percent}"></progress> ${j.percent}%</td><td>${j.state}</td></tr>`}
html+='</table>';app.innerHTML=html}catch(e){document.getElementById('app').textContent='加载失败：'+e.message}}
load();setInterval(load,3000);
</script></body></html>
"#;

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(CoreJob { store: None }))
}
