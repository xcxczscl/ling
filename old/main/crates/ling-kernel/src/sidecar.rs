//! 进程外侧车（`docs/04` §6）：jsonl 控制面，端点 `tcp:127.0.0.1:<随机>`，
//! 经环境变量 `LING_SOCKET` 传入。数据面不走此协议——侧车自己干重活。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ling_sdk::{json, CallError, Capability, Value};

use crate::context::KernelCore;
use crate::error::KernelError;
use crate::manifest::Manifest;
use crate::state::{PluginState, Shared};
use crate::{broker, bus, loader};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INVOKE_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_GRACE: Duration = Duration::from_secs(5);

type Pending = Mutex<HashMap<u64, SyncSender<Result<Value, CallError>>>>;

pub struct SidecarHandle {
    pub endpoint: String,
    pub restart: String,
    pub stopping: AtomicBool,
    pub deactivated_ack: AtomicBool,
    child: Mutex<Option<Child>>,
    writer: Mutex<Option<TcpStream>>,
    next_req: AtomicU64,
    pending: Pending,
}

impl SidecarHandle {
    fn next_req_id(&self) -> u64 {
        self.next_req.fetch_add(1, Ordering::SeqCst)
    }

    fn send(&self, message: &Value) -> Result<(), CallError> {
        let mut writer = self.writer.lock().unwrap();
        let Some(stream) = writer.as_mut() else {
            return Err(CallError::new("E_SIDE_CRASHED", "侧车连接已断开"));
        };
        writeln!(stream, "{message}").map_err(|e| CallError::new("E_SIDE_CRASHED", e.to_string()))
    }

    fn invoke(&self, method: &str, args: Value) -> Result<Value, CallError> {
        let req_id = self.next_req_id();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.pending.lock().unwrap().insert(req_id, tx);
        self.send(&json!({"op": "invoke", "id": req_id, "method": method, "args": args}))?;
        match rx.recv_timeout(INVOKE_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.pending.lock().unwrap().remove(&req_id);
                Err(CallError::new(
                    "E_TIMEOUT",
                    format!("侧车方法 {method} 应答超时"),
                ))
            }
        }
    }
}

/// 启动侧车进程并完成握手（`loader::load` 的侧车分支）。
pub fn start(
    shared: &Arc<Shared>,
    core: &Arc<KernelCore>,
    id: &str,
    dir: &Path,
    manifest: Manifest,
    manifest_text: String,
) -> Result<(), KernelError> {
    let meta = manifest
        .sidecar
        .clone()
        .ok_or_else(|| KernelError::load("清单缺少 sidecar 段"))?;
    let exe = dir.join(&meta.executable);
    if !exe.is_file() {
        return Err(KernelError::load(format!(
            "侧车可执行文件不存在：{}",
            exe.display()
        )));
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| KernelError::io("bind sidecar listener", e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| KernelError::io("sidecar local_addr", e.to_string()))?
        .port();
    let endpoint = format!("tcp:127.0.0.1:{port}");

    let mut child = Command::new(&exe)
        .current_dir(dir)
        .env("LING_SOCKET", &endpoint)
        .env("LING_PLUGIN", id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| KernelError::load(format!("启动侧车失败：{e}")))?;

    listener
        .set_nonblocking(true)
        .map_err(|e| KernelError::io("sidecar listener mode", e.to_string()))?;
    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if let Some(status) = child.try_wait().ok().flatten() {
                    return Err(KernelError::load(format!(
                        "侧车进程未连上控制面就退出了（{status}）"
                    )));
                }
                if Instant::now() > connect_deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(KernelError::load("侧车未在期限内连上控制面"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(KernelError::io("sidecar accept", e.to_string())),
        }
    };
    drop(listener);
    let _ = stream.set_nodelay(true);

    let handle = Arc::new(SidecarHandle {
        endpoint,
        restart: meta.restart.clone(),
        stopping: AtomicBool::new(false),
        deactivated_ack: AtomicBool::new(false),
        child: Mutex::new(Some(child)),
        writer: Mutex::new(Some(
            stream
                .try_clone()
                .map_err(|e| KernelError::io("clone sidecar stream", e.to_string()))?,
        )),
        next_req: AtomicU64::new(1),
        pending: Pending::default(),
    });

    let declared: std::collections::BTreeSet<Capability> = manifest
        .declared_capabilities()
        .map_err(|e| KernelError::load(e.to_string()))?
        .into_iter()
        .collect();
    std::fs::create_dir_all(dir.join("data"))
        .map_err(|e| KernelError::io("create data dir", e.to_string()))?;
    crate::caps::set_declared(shared, id, declared);

    shared.plugins.write().unwrap().insert(
        id.to_string(),
        crate::state::PluginRecord {
            manifest: manifest.clone(),
            manifest_text,
            state: PluginState::Activating,
            library: None,
            library_file: None,
            instance: None,
            ledger: Vec::new(),
            next_seq: 1,
            deactivate_cbs: Vec::new(),
        },
    );
    shared
        .sidecars
        .lock()
        .unwrap()
        .insert(id.to_string(), Arc::clone(&handle));

    handle
        .send(&json!({"op": "activate"}))
        .map_err(|e| KernelError::load(format!("发送 activate 失败：{e}")))?;

    {
        let mut reader = BufReader::new(stream);
        let shared = Arc::clone(shared);
        let core = Arc::clone(core);
        let handle = Arc::clone(&handle);
        let id = id.to_string();
        std::thread::spawn(move || monitor(&shared, &core, &id, &handle, &mut reader));
    }

    if let Some(record) = shared.plugins.write().unwrap().get_mut(id) {
        record.state = PluginState::Activated;
    }
    loader::state_event(shared, id, "activated");
    Ok(())
}

/// 优雅停用：deactivate → 等 deactivated → shutdown → 等退出（5s）→ 强杀。
pub fn graceful_stop(handle: &SidecarHandle) {
    let _ = handle.send(&json!({"op": "deactivate"}));
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if handle.deactivated_ack.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = handle.send(&json!({"op": "shutdown"}));
    reap_child(handle, STOP_GRACE);
    let mut writer = handle.writer.lock().unwrap();
    *writer = None;
}

fn reap_child(handle: &SidecarHandle, grace: Duration) {
    let mut child_slot = handle.child.lock().unwrap();
    let Some(child) = child_slot.as_mut() else {
        return;
    };
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn monitor(
    shared: &Arc<Shared>,
    core: &Arc<KernelCore>,
    id: &str,
    handle: &Arc<SidecarHandle>,
    reader: &mut BufReader<TcpStream>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        handle_sidecar_message(shared, core, id, handle, &message);
    }

    // 连接断开：正常停用路径不做崩溃善后（stopping 已置位）。
    if handle.stopping.load(Ordering::SeqCst) {
        reap_child(handle, Duration::from_secs(1));
        return;
    }
    crash_cleanup(shared, core, id, handle);
}

fn handle_sidecar_message(
    shared: &Arc<Shared>,
    core: &Arc<KernelCore>,
    id: &str,
    handle: &Arc<SidecarHandle>,
    message: &Value,
) {
    let op = message["op"].as_str().unwrap_or("");
    match op {
        "register_method" => {
            let Some(name) = message["name"].as_str() else {
                return;
            };
            let method_handle = Arc::clone(handle);
            let method_name = name.to_string();
            let handler: crate::state::Handler =
                Arc::new(move |args: Value| method_handle.invoke(&method_name, args));
            let undo: ling_sdk::UndoFn = Box::new(|| {});
            let _ = broker::register(shared, id, name, handler, undo, None);
        }
        "subscribe" => {
            let Some(pattern) = message["pattern"].as_str() else {
                return;
            };
            let event_handle = Arc::clone(handle);
            let cb: crate::state::Callback = Arc::new(move |topic: &str, payload: Value| {
                let _ = event_handle.send(&json!({
                    "op": "event",
                    "topic": topic,
                    "payload": payload,
                }));
            });
            let _ = bus::subscribe(shared, id, pattern, cb, Box::new(|| {}));
        }
        "call" => {
            let req_id = message["id"].as_u64();
            let plugin = message["plugin"].as_str().unwrap_or("").to_string();
            let method = message["method"].as_str().unwrap_or("").to_string();
            let args = message["args"].clone();
            let shared = Arc::clone(shared);
            let handle = Arc::clone(handle);
            std::thread::spawn(move || {
                let response = match broker::call(&shared, &plugin, &method, args) {
                    Ok(result) => json!({"op": "result", "id": req_id, "result": result}),
                    Err(e) => json!({
                        "op": "error",
                        "id": req_id,
                        "error": {"code": e.code(), "message": e.message()}
                    }),
                };
                let _ = handle.send(&response);
            });
        }
        "emit" => {
            let Some(topic) = message["topic"].as_str() else {
                return;
            };
            let _ = bus::emit(shared, id, topic, message["payload"].clone());
        }
        "result" => {
            if let Some(req_id) = message["id"].as_u64() {
                if let Some(tx) = handle.pending.lock().unwrap().remove(&req_id) {
                    let _ = tx.send(Ok(message["result"].clone()));
                }
            }
        }
        "error" => {
            if let Some(req_id) = message["id"].as_u64() {
                let code = message["error"]["code"].as_str().unwrap_or("E_INTERNAL");
                let msg = message["error"]["message"]
                    .as_str()
                    .unwrap_or("侧车调用失败");
                if let Some(tx) = handle.pending.lock().unwrap().remove(&req_id) {
                    let _ = tx.send(Err(CallError::new(code, msg)));
                }
            }
        }
        "deactivated" => {
            handle.deactivated_ack.store(true, Ordering::SeqCst);
        }
        _ => {}
    }
    let _ = core;
}

fn crash_cleanup(
    shared: &Arc<Shared>,
    core: &Arc<KernelCore>,
    id: &str,
    handle: &Arc<SidecarHandle>,
) {
    shared.sidecars.lock().unwrap().remove(id);
    reap_child(handle, Duration::from_secs(1));
    // 账本倒序撤销 + 注销方法/订阅 + 移除装载记录。
    let _ = crate::kernel::stop_plugin(shared, id);
    loader::state_event(shared, id, "crashed");
    if handle.restart == "on-crash" {
        std::thread::sleep(Duration::from_millis(200));
        if !shared.plugins.read().unwrap().contains_key(id)
            && shared.plugin_dir(id).join("plugin.toml").is_file()
        {
            let _ = loader::load(shared, core, id);
        }
    }
}
