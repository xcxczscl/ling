//! P7 远程执行与跨设备接力（`docs/01` §5.6 / §5.9 / §5.15，方案甲 §7.3）：
//!
//! - **控制平面**（接收侧，HTTP/1.1，`httpmini` 解析）：activate 绑
//!   `0.0.0.0:7917`（忙退 7918–7920，全忙 → 平面关闭降级，不阻塞装载）。
//!   端点：`GET /remote/caps`、`POST /sync`（§5.9 LWW 接收，仅落库）、
//!   `POST /remote/run`、`GET /remote/run/{id}`、`POST /remote/run/{id}/stop`、
//!   `POST /remote/run/{id}/input`、`GET /remote/run/{id}/log`、
//!   `GET /control/status`。错误一律 `{error:{code,message}}` + HTTP 400，
//!   **P 码原样透传**回发起侧（P103/P100 跨机回传）。
//! - **发起侧**：`player.remote.*` + `player.control.status`；对端 =
//!   `peer` 直给 `host:port` / device_id 查 rm peer 表（host+7917）/
//!   缺省单配对推导；0 配对 → **P303**，多配对未指定 → **P100**。
//! - **同步发送**（§5.9）：`persist_session` 钩子——音量/字幕/暂停/停止/
//!   播完/停机 flush **立即**，Progress **节流（5s 或 1% duration 先到者）**；
//!   目标 = rm peer 表全量 `host:7917`（一设备一控制口口径），后台线程
//!   best-effort 2s，失败静默（同步是增强面）。
//! - **输入**（§5.15）：仅 `/remote/run/{id}/input` 命名空间（视频会话 id
//!   不在运行表 → P100，普通投屏不接受输入）；事件 JSONL 写沙箱 stdin。
//!
//! 首版口径（附录 B 登记）：控制平面**无请求级鉴权**（bind 0.0.0.0 局域网
//! 信任，同 lite `/cast`）；目标选择受 rm peer 表约束在发起侧；`/remote/run`
//! 只能运行**本机目录内** app/game 且全链走 P5 沙箱（与本地 `player.run`
//! 同等约束）；画面回传 headless 降级为 stdout/stderr 日志尾观测面。

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ling_sdk::{CallError, Context, Value};

use crate::error::{from_rm_err, PlayerError};
use crate::httpmini::{read_body, read_request, write_response};
use crate::run;
use crate::store::{SyncOutcome, SyncPayload};
use crate::PlayerState;

/// 控制平面首选端口（docs-common 未占，P7 登记；一设备一控制口口径）。
pub const CONTROL_PORT: u16 = 7917;
/// 端口候选：7917 忙则顺延 7918–7920；全忙 → 平面关闭（降级不阻塞装载）。
const PORT_CANDIDATES: [u16; 4] = [7917, 7918, 7919, 7920];

// ===== 同步发送节流（§5.9 触发表） =====

/// 同步推送时机（写穿钩子传入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// 立即（暂停/停止/音量/字幕/播完/停机 flush）。
    Immediate,
    /// 节流：5 秒或 1% duration 先到者（播放进度）。
    Throttled,
}

/// §5.9「每 5 秒或每 1% 进度（取先到者）」判定；`mark` = 上次推送
/// `(position_sec, Unix秒)`。判定为真时调用方盖新 mark。
pub fn should_sync(
    mode: SyncMode,
    mark: Option<(f64, u64)>,
    position_sec: f64,
    duration: f64,
    now: u64,
) -> bool {
    match mode {
        SyncMode::Immediate => true,
        SyncMode::Throttled => match mark {
            None => true,
            Some((last_pos, last_at)) => {
                now.saturating_sub(last_at) >= 5
                    || (duration > 0.0 && (position_sec - last_pos).abs() >= duration * 0.01)
            }
        },
    }
}

/// rm peer 表 → 同步目标（addr 取 host + 控制口；空 host 跳过）。
pub fn derive_targets(peers: &[(String, String)]) -> Vec<String> {
    peers
        .iter()
        .filter_map(|(_, addr)| {
            let host = host_of(addr);
            (!host.is_empty()).then(|| format!("{host}:{CONTROL_PORT}"))
        })
        .collect()
}

/// 同步目标（读 `resources-manager.sync.status` 只读 peer 表；rm 不可用 → 空表）。
pub fn sync_targets(ctx: &Context) -> Vec<String> {
    match rm_peers(ctx) {
        Ok(peers) => derive_targets(&peers),
        Err(_) => Vec::new(),
    }
}

/// 后台推送一条 §5.9 报文到全部目标（best-effort，2s 超时，失败静默）。
pub fn push_sync(targets: &[String], payload: &Value) {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return;
    };
    for t in targets {
        let _ = client
            .post(format!("http://{t}/sync"))
            .header("content-type", "application/json")
            .body(payload.to_string())
            .send();
    }
}

// ===== 对端解析与发起侧 HTTP =====

/// rm peer 表（只读）→ `(device_id, addr)` 列表。
fn rm_peers(ctx: &Context) -> Result<Vec<(String, String)>, PlayerError> {
    let out = ctx
        .call(
            "resources-manager",
            "resources-manager.sync.status",
            serde_json::json!({}),
        )
        .map_err(|e| from_rm_err(&e))?;
    let rows = out["peers"].as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some((
                r["device_id"].as_str()?.to_string(),
                r["addr"].as_str()?.to_string(),
            ))
        })
        .collect())
}

/// `peer` 入参归一为 `host:port`：
/// - 含 `:` → 直给（剥 `http(s)://` 前缀与尾 `/`）；
/// - 不含 `:` → device_id 查 rm peer 表，取 addr 的 host + **7917**
///   （rm LAN 口只给 host，控制口按一设备一控制口口径推导）；
/// - 缺省 → 恰 1 台配对用之；**0 台 → P303**（未配对）；**>1 台 → P100**
///   （需指定 peer）。
pub fn resolve_peer(ctx: &Context, args: &Value) -> Result<String, PlayerError> {
    let peers = rm_peers(ctx)?;
    if let Some(p) = args
        .get("peer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if p.contains(':') {
            return Ok(normalize_addr(p));
        }
        let hit = peers
            .iter()
            .find(|(id, _)| id == p)
            .ok_or_else(|| PlayerError::remote_unavailable(format!("未配对或未知对端：{p}")))?;
        return Ok(format!("{}:{}", host_of(&hit.1), CONTROL_PORT));
    }
    match peers.len() {
        0 => Err(PlayerError::remote_unavailable("无配对对端（rm peer 表为空）")),
        1 => Ok(format!("{}:{}", host_of(&peers[0].1), CONTROL_PORT)),
        _ => Err(PlayerError::bad_param(format!(
            "配对对端 {} 台，需以 peer 指定（device_id 或 host:port）",
            peers.len()
        ))),
    }
}

/// 剥协议前缀与尾斜杠（`http://a:7917/` → `a:7917`）。
fn normalize_addr(addr: &str) -> String {
    let s = addr.trim().trim_end_matches('/');
    let s = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .unwrap_or(s);
    s.to_string()
}

/// addr 的 host 部分（`192.168.1.5:7912` → `192.168.1.5`；无端口原样）。
/// IPv6 不支持（LAN 口为 ipv4 口径，附录 B 登记）。
pub fn host_of(addr: &str) -> String {
    let a = normalize_addr(addr);
    let a = a.split('/').next().unwrap_or(&a).to_string();
    match a.rsplit_once(':') {
        Some((h, port))
            if !h.is_empty() && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            h.to_string()
        }
        _ => a,
    }
}

/// run_id 字符集（URL 段安全；`run-<hex>` 照此生成）。
fn valid_run_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 发起侧 → 对端控制平面一次请求。
/// 连接失败/响应畸形 → **P303**（对端未运行）；错误体带 P 码 → **原码透传**
/// （P103/P100 等跨机回传，方法面按 CallError 冒泡）。
pub fn remote_request(
    peer: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, CallError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| {
            CallError::from(PlayerError::remote_unavailable(format!("HTTP 客户端失败：{e}")))
        })?;
    let url = format!("http://{peer}{path}");
    let resp = match (method, body) {
        ("POST", Some(v)) => client
            .post(&url)
            .header("content-type", "application/json")
            .body(v.to_string())
            .send(),
        ("GET", _) => client.get(&url).send(),
        _ => unreachable!("remote_request 仅 GET/POST"),
    };
    let resp = resp.map_err(|e| {
        CallError::from(PlayerError::remote_unavailable(format!(
            "对端控制平面不可达（{peer}）：{e}"
        )))
    })?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str(&text).map_err(|_| {
            CallError::from(PlayerError::remote_unavailable(format!(
                "对端响应非 JSON：{text}"
            )))
        });
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let code = v["error"]["code"].as_str().unwrap_or("");
    if code.is_empty() {
        return Err(CallError::from(PlayerError::remote_unavailable(format!(
            "对端 HTTP {status}"
        ))));
    }
    let message = v["error"]["message"].as_str().unwrap_or("").to_string();
    Err(CallError::new(code, message))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, CallError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CallError::from(PlayerError::bad_param(format!("缺少 {key}"))))
}

/// `player.remote.caps`：对端能力面。
pub fn remote_caps(ctx: &Context, args: &Value) -> Result<Value, CallError> {
    let peer = resolve_peer(ctx, args).map_err(CallError::from)?;
    remote_request(&peer, "GET", "/remote/caps", None)
}

/// `player.remote.run`：远程执行（发起侧先过 content_id 关，接收侧再校验一次）。
/// `sandbox_opts` 原样透传（接收侧沙箱全链与本地 `player.run` 一致）。
pub fn remote_run(ctx: &Context, args: &Value) -> Result<Value, CallError> {
    ensure_content_id(args)?;
    let peer = resolve_peer(ctx, args).map_err(CallError::from)?;
    let mut body = args.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.remove("peer");
    }
    remote_request(&peer, "POST", "/remote/run", Some(&body))
}

/// `player.remote.status`：运行行。
pub fn remote_status(ctx: &Context, args: &Value) -> Result<Value, CallError> {
    let run_id = arg_str(args, "run_id")?;
    if !valid_run_id(run_id) {
        return Err(CallError::from(PlayerError::bad_param(format!(
            "run_id 非法：{run_id}"
        ))));
    }
    let peer = resolve_peer(ctx, args).map_err(CallError::from)?;
    remote_request(&peer, "GET", &format!("/remote/run/{run_id}"), None)
}

/// `player.remote.stop`：停远端运行。
pub fn remote_stop(ctx: &Context, args: &Value) -> Result<Value, CallError> {
    let run_id = arg_str(args, "run_id")?;
    if !valid_run_id(run_id) {
        return Err(CallError::from(PlayerError::bad_param(format!(
            "run_id 非法：{run_id}"
        ))));
    }
    let peer = resolve_peer(ctx, args).map_err(CallError::from)?;
    remote_request(
        &peer,
        "POST",
        &format!("/remote/run/{run_id}/stop"),
        Some(&serde_json::json!({})),
    )
}

/// `player.remote.input`：§5.15 输入事件（发起侧先校验，失败不发包）。
pub fn remote_input(ctx: &Context, args: &Value) -> Result<Value, CallError> {
    let run_id = arg_str(args, "run_id")?;
    if !valid_run_id(run_id) {
        return Err(CallError::from(PlayerError::bad_param(format!(
            "run_id 非法：{run_id}"
        ))));
    }
    let event = args.get("event").cloned().unwrap_or(Value::Null);
    validate_input(&event).map_err(|m| CallError::from(PlayerError::bad_param(m)))?;
    let peer = resolve_peer(ctx, args).map_err(CallError::from)?;
    remote_request(
        &peer,
        "POST",
        &format!("/remote/run/{run_id}/input"),
        Some(&serde_json::json!({ "event": event })),
    )
}

/// `player.remote.log`：远端沙箱日志尾（`stream` 缺省 stdout）。
pub fn remote_log(ctx: &Context, args: &Value) -> Result<Value, CallError> {
    let run_id = arg_str(args, "run_id")?;
    if !valid_run_id(run_id) {
        return Err(CallError::from(PlayerError::bad_param(format!(
            "run_id 非法：{run_id}"
        ))));
    }
    let stream = args
        .get("stream")
        .and_then(Value::as_str)
        .unwrap_or("stdout");
    if stream != "stdout" && stream != "stderr" {
        return Err(CallError::from(PlayerError::bad_param(format!(
            "stream 须为 stdout/stderr，收到：{stream}"
        ))));
    }
    let peer = resolve_peer(ctx, args).map_err(CallError::from)?;
    remote_request(
        &peer,
        "GET",
        &format!("/remote/run/{run_id}/log?stream={stream}"),
        None,
    )
}

/// 远程执行入参关（发起侧与接收侧各过一次）：须 `content_id`——
/// `placement_id` 是本机放置号，跨设备不可移植。
pub fn ensure_content_id(args: &Value) -> Result<(), CallError> {
    let ok = args
        .get("content_id")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(CallError::from(PlayerError::bad_param(
            "远程执行需 content_id（placement_id 为本机放置号，跨设备不可移植）",
        )))
    }
}

/// §5.15 输入事件校验（四类照表；未知 type/缺字段/越界 → Err(原因) → P100）。
pub fn validate_input(event: &Value) -> Result<(), String> {
    let ty = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or("event.type 缺失")?;
    match ty {
        "input.touch" => {
            let x = event.get("x").and_then(Value::as_f64).ok_or("input.touch 缺 x")?;
            let y = event.get("y").and_then(Value::as_f64).ok_or("input.touch 缺 y")?;
            if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
                return Err("input.touch x/y 须在 0..=1".into());
            }
            action_in(event, &["down", "move", "up"])
        }
        "input.key" => {
            let key = event.get("key").and_then(Value::as_str).unwrap_or("");
            if key.is_empty() {
                return Err("input.key 缺 key".into());
            }
            action_in(event, &["down", "up"])
        }
        "input.text" => {
            let text = event
                .get("text")
                .and_then(Value::as_str)
                .ok_or("input.text 缺 text")?;
            if text.len() > 4096 {
                return Err("input.text 超长（>4096 字节）".into());
            }
            Ok(())
        }
        "input.back" => action_in(event, &["press"]),
        other => Err(format!("未知事件类型：{other}")),
    }
}

fn action_in(event: &Value, allow: &[&str]) -> Result<(), String> {
    let a = event
        .get("action")
        .and_then(Value::as_str)
        .ok_or("缺 action")?;
    if allow.contains(&a) {
        Ok(())
    } else {
        Err(format!("action 非法：{a}（须为 {allow:?} 之一）"))
    }
}

// ===== 本机控制状态（`player.control.status` 与端点共用） =====

/// 控制平面/同步观测面（e2e 确定性轮询：端口、接收计数、同步目标）。
pub fn control_status_value(state: &PlayerState, port: Option<u16>) -> Value {
    serde_json::json!({
        "enabled": port.is_some(),
        "port": port,
        "device_id": state.store.as_ref().map(|s| s.device_id().to_string()),
        "sync_applied": state.sync_applied.load(Ordering::SeqCst),
        "sync_ignored": state.sync_ignored.load(Ordering::SeqCst),
        "sync_targets": sync_targets(&state.ctx),
    })
}

// ===== 控制平面（接收侧 HTTP 服务） =====

/// 控制平面句柄：停用时 `shutdown()` 关监听（端口释放，供同进程下一次
/// 装载/下一 e2e 用例复用 7917）。
pub struct ControlHandle {
    pub port: u16,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ControlHandle {
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.join.take() {
            let _ = h.join(); // 非阻塞 accept 轮询 ≤100ms 自醒
        }
    }
}

impl Drop for ControlHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 按候选表绑第一个可用端口（`port=0` → OS 随机，测试用）。
fn bind_first(candidates: &[u16]) -> std::io::Result<(TcpListener, u16)> {
    for &p in candidates {
        if let Ok(l) = TcpListener::bind(("0.0.0.0", p)) {
            let port = l.local_addr()?.port();
            return Ok((l, port));
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        "控制平面候选端口全忙",
    ))
}

/// activate 时启动控制平面；端口全忙 → `None`（平面关闭降级，不阻塞装载）。
pub fn start(state: Arc<PlayerState>) -> Option<ControlHandle> {
    let (listener, port) = bind_first(&PORT_CANDIDATES).ok()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let st = Arc::clone(&state);
    let join = std::thread::spawn(move || serve(listener, st, port, stop2));
    Some(ControlHandle { port, stop, join: Some(join) })
}

/// accept 循环：非阻塞 + 50ms 轮询（停机标志自醒）；每连接独立线程
/// （读写 5s 超时，单慢连接不拖住整个平面）。
fn serve(listener: TcpListener, state: Arc<PlayerState>, port: u16, stop: Arc<AtomicBool>) {
    let _ = listener.set_nonblocking(true);
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let st = Arc::clone(&state);
                std::thread::spawn(move || {
                    let _ = handle_conn(&stream, &st, port);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

fn handle_conn(stream: &std::net::TcpStream, state: &Arc<PlayerState>, port: u16) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut rd = std::io::BufReader::new(stream.try_clone()?);
    let Some(req) = read_request(&mut rd)? else {
        return Ok(());
    };
    let body = if req.content_length() > 0 {
        read_body(&mut rd, req.content_length()).unwrap_or_default()
    } else {
        Vec::new()
    };
    let (status, value) = route(state, port, &req, &body);
    let bytes = value.to_string().into_bytes();
    let mut w = stream;
    write_response(&mut w, status, "application/json", &bytes)?;
    Ok(())
}

/// 路由（段匹配；错误 → HTTP 400 + `{error:{code,message}}`，P 码原码透传）。
fn route(
    state: &Arc<PlayerState>,
    port: u16,
    req: &crate::httpmini::Request,
    body: &[u8],
) -> (u16, Value) {
    let (path, query) = match req.path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (req.path.as_str(), ""),
    };
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let method = req.method.as_str();
    let outcome = match (method, segs.as_slice()) {
        ("GET", ["control", "status"]) => Ok(control_status_value(state, Some(port))),
        ("GET", ["remote", "caps"]) => Ok(serde_json::json!({
            "caps": { "run": true, "sync": true, "input": true },
            "control_port": port,
        })),
        ("POST", ["sync"]) => handle_sync(state, body),
        ("POST", ["remote", "run"]) => handle_remote_run(state, body),
        ("GET", ["remote", "run", id]) => run::get_value(&state.runs, id).map_err(CallError::from),
        ("POST", ["remote", "run", id, "stop"]) => run::stop(
            &state.runs,
            &serde_json::json!({ "run_id": id }),
        )
        .map_err(CallError::from),
        ("POST", ["remote", "run", id, "input"]) => handle_input(state, id, body),
        ("GET", ["remote", "run", id, "log"]) => handle_log(state, id, query),
        _ => Err(CallError::from(PlayerError::bad_param(format!(
            "未知路径：{method} {path}"
        )))),
    };
    match outcome {
        Ok(v) => (200, v),
        Err(e) => (
            400,
            serde_json::json!({ "error": { "code": e.code(), "message": e.message() } }),
        ),
    }
}

/// `POST /sync`（§5.9 LWW 接收）：仅落库，不扰在播会话。
/// 协议结果 200：`{applied:true}` / `{applied:false, reason:"stale"|"self"|"degraded"}`；
/// 报文非法 → 400 P100。
fn handle_sync(state: &Arc<PlayerState>, body: &[u8]) -> Result<Value, CallError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| CallError::from(PlayerError::bad_param("body 非 JSON")))?;
    let payload = SyncPayload::parse(&v)
        .map_err(|m| CallError::from(PlayerError::bad_param(format!("同步报文非法：{m}"))))?;
    let Some(store) = state.store.as_ref() else {
        return Ok(serde_json::json!({ "applied": false, "reason": "degraded" }));
    };
    let outcome = store
        .apply_sync(&payload)
        .map_err(|m| CallError::from(PlayerError::bad_param(m)))?;
    let out = match outcome {
        SyncOutcome::Applied => {
            state.sync_applied.fetch_add(1, Ordering::SeqCst);
            serde_json::json!({ "applied": true })
        }
        SyncOutcome::Stale => {
            state.sync_ignored.fetch_add(1, Ordering::SeqCst);
            serde_json::json!({ "applied": false, "reason": "stale" })
        }
        SyncOutcome::SelfDevice => {
            state.sync_ignored.fetch_add(1, Ordering::SeqCst);
            serde_json::json!({ "applied": false, "reason": "self" })
        }
    };
    Ok(out)
}

/// `POST /remote/run`（§5.6）：content_id 关（placement 跨设备不可移植）+
/// 复用 `run::run` 沙箱全链（与本地 `player.run` 同等约束）。
fn handle_remote_run(state: &Arc<PlayerState>, body: &[u8]) -> Result<Value, CallError> {
    let mut args: Value = serde_json::from_slice(body)
        .map_err(|_| CallError::from(PlayerError::bad_param("body 非 JSON")))?;
    ensure_content_id(&args)?;
    if let Some(obj) = args.as_object_mut() {
        obj.remove("peer");
    }
    run::run(&state.ctx, &state.runs, args).map_err(CallError::from)
}

/// `POST /remote/run/{id}/input`（§5.15）：事件校验 + 命名空间门禁
/// （run 表之外的 id 一律 P100 → 视频会话拿不到输入）。
fn handle_input(state: &Arc<PlayerState>, run_id: &str, body: &[u8]) -> Result<Value, CallError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| CallError::from(PlayerError::bad_param("body 非 JSON")))?;
    let event = v
        .get("event")
        .cloned()
        .ok_or_else(|| CallError::from(PlayerError::bad_param("缺 event")))?;
    validate_input(&event).map_err(|m| CallError::from(PlayerError::bad_param(m)))?;
    run::write_input(&state.runs, run_id, &event).map_err(CallError::from)?;
    Ok(serde_json::json!({ "accepted": true, "run_id": run_id }))
}

/// `GET /remote/run/{id}/log?stream=`：stdout/stderr 尾（headless 观测面）。
fn handle_log(state: &Arc<PlayerState>, run_id: &str, query: &str) -> Result<Value, CallError> {
    let stream = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("stream="))
        .unwrap_or("stdout");
    run::log_tail(&state.runs, run_id, stream).map_err(CallError::from)
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(x: f64, y: f64, action: &str) -> Value {
        serde_json::json!({"type": "input.touch", "x": x, "y": y, "action": action})
    }

    /// t1：§5.15 四类事件合法样例通过；缺 type/未知 type/越界/缺 action 拒。
    #[test]
    fn t1_validate_input_shapes() {
        assert!(validate_input(&touch(0.5, 0.3, "down")).is_ok());
        assert!(validate_input(&serde_json::json!({"type":"input.key","key":"Enter","action":"down"})).is_ok());
        assert!(validate_input(&serde_json::json!({"type":"input.text","text":"hello"})).is_ok());
        assert!(validate_input(&serde_json::json!({"type":"input.back","action":"press"})).is_ok());

        assert!(validate_input(&Value::Null).is_err(), "缺 type");
        assert!(validate_input(&serde_json::json!({"type":"input.paste"})).is_err(), "未知 type");
        assert!(validate_input(&touch(1.5, 0.3, "down")).is_err(), "x 越界");
        assert!(validate_input(&touch(0.5, 0.3, "click")).is_err(), "action 越界");
        assert!(validate_input(&serde_json::json!({"type":"input.key","action":"down"})).is_err(), "缺 key");
        assert!(validate_input(&serde_json::json!({"type":"input.key","key":"A"})).is_err(), "缺 action");
        let long: String = "a".repeat(4097);
        assert!(
            validate_input(&serde_json::json!({"type":"input.text","text": long})).is_err(),
            "超长 text"
        );
    }

    /// t2：节流判定——Immediate 恒真；Throttled 首推/5s/1% 先到者。
    #[test]
    fn t2_should_sync_throttle_rules() {
        use SyncMode::*;
        assert!(should_sync(Immediate, Some((1.0, 100)), 1.0, 10.0, 100));
        // 首推（无 mark）
        assert!(should_sync(Throttled, None, 0.5, 10.0, 100));
        // 同秒、位移不足 1%（0.05 < 0.1）→ 不推
        assert!(!should_sync(Throttled, Some((1.0, 100)), 1.05, 10.0, 100));
        // 位移 ≥1%（0.2 ≥ 0.1）→ 立即推（1% 先到）
        assert!(should_sync(Throttled, Some((1.0, 100)), 1.2, 10.0, 101));
        // ≥5s → 推（5s 先到）
        assert!(should_sync(Throttled, Some((1.0, 100)), 1.05, 10.0, 105));
        // 无 duration（查看器不会走到这里，防御）→ 只剩 5s 规则
        assert!(!should_sync(Throttled, Some((1.0, 100)), 999.0, 0.0, 101));
        assert!(should_sync(Throttled, Some((1.0, 100)), 999.0, 0.0, 105));
    }

    /// t3：host_of——带端口取 host、无端口原样、剥 http 前缀与路径。
    #[test]
    fn t3_host_of() {
        assert_eq!(host_of("192.168.1.5:7912"), "192.168.1.5");
        assert_eq!(host_of("127.0.0.1:7932"), "127.0.0.1");
        assert_eq!(host_of("noport"), "noport");
        assert_eq!(host_of("http://a.example:7917/"), "a.example");
        assert_eq!(host_of("https://h/extra/path"), "h");
        assert_eq!(host_of("host:notaport"), "host:notaport", "非数字端口不切");
    }

    /// t4：derive_targets——host+7917；空 addr 跳过；去重语义不保证（表内一台一 host）。
    #[test]
    fn t4_derive_targets() {
        let peers = vec![
            ("dv_a".to_string(), "192.168.1.5:7912".to_string()),
            ("dv_b".to_string(), "127.0.0.1:7951".to_string()),
            ("dv_c".to_string(), String::new()),
        ];
        assert_eq!(
            derive_targets(&peers),
            vec!["192.168.1.5:7917", "127.0.0.1:7917"],
            "空 host 跳过"
        );
    }

    /// t5：run_id 字符集门禁（URL 段安全）。
    #[test]
    fn t5_valid_run_id() {
        assert!(valid_run_id("run-0011aabbccddeeff"));
        assert!(valid_run_id("run_ab-12"));
        assert!(!valid_run_id(""));
        assert!(!valid_run_id("run/../x"));
        assert!(!valid_run_id("run a"));
        assert!(!valid_run_id("run?x=1"));
    }

    /// t6：bind_first——候选 0 → OS 随机端口（可绑可释放）；空表 → 错。
    #[test]
    fn t6_bind_first() {
        let (l, port) = bind_first(&[0]).expect("port 0 应可绑");
        assert!(port > 0);
        drop(l);
        assert!(bind_first(&[]).is_err(), "空候选表 → AddrInUse");
    }

    /// t7：同步报文构造面（与 store::SyncPayload::parse 往返）——content_id 保真。
    #[test]
    fn t7_payload_roundtrip() {
        let p = crate::store::SyncPayload {
            content_id: "blake3:xyz".into(),
            position_sec: 3.5,
            volume: 1.0,
            subtitle_track: -1,
            updated_at: 1729180800,
            device_id: "dv_me".into(),
        };
        let v = serde_json::json!({
            "type": "session.sync",
            "content_id": p.content_id,
            "position_sec": p.position_sec,
            "volume": p.volume,
            "subtitle_track": p.subtitle_track,
            "updated_at": p.updated_at,
            "device_id": p.device_id,
        });
        let back = crate::store::SyncPayload::parse(&v).unwrap();
        assert_eq!(back, p);
    }
}
