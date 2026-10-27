//! `player` 插件（`docs/01` 五区模型）：P0 本地最小播放 → P1 图片/PDF 查看 →
//! P2 输出设备/UI 统一 → P3 串流输出 v1（模式 A + lite 接收器 + 局域网发现）→
//! P4 串流 v2（重编码/自动路由）→ P5 沙箱运行 → P6 会话持久化（player.db）→
//! P7 远程执行 + 跨设备接力（控制平面 7917 + §5.9 会话同步，`control.rs`）。
//!
//! 管道：`Source`（rm `stream.open` 流 URL）→ `Capability`（libmpv / 图片渲染 /
//! PDF 渲染 / 沙箱）→ `Sink`（屏幕帧通道 `player.frame` / mpv 直出 / 模式 A 代理）。
//! 附控制面与查看器网页、P 族错误。
//!
//! 方法（§5.12）：play / pause / resume / seek / set_volume / stop / status +
//! P1：goto_page / frame / set_subtitle + P2：outputs.list / outputs.set +
//! P3/P4：cast / cast.stop / receiver.list + P5：run / run.list / run.stop +
//! P6 会话/选择态持久化 + P7：remote.caps/run/status/stop/input/log /
//! control.status；事件：player.playback.*、player.cast.*。

pub mod asset;
mod aspect;
mod backend;
mod cast;
mod control;
mod discovery;
mod error;
pub mod image_cap;
mod httpmini;
pub mod lite;
pub mod outputs;
pub mod pdf_cap;
mod proxy;
mod router;
mod run;
pub mod sandbox;
mod session;
mod shcmd;
mod store;
pub mod tracks;
pub mod tv;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ling_sdk::{ActivateError, CallError, Context, Plugin, Value};

use asset::Route;
use backend::{BackendEvent, MpvBackend};
use cast::CastState;
use error::{from_rm_err, PlayerError};
use image_cap::ImageFrame;
use outputs::Selected;
use session::{SessionManager, ViewerState};

/// 播放器插件状态（activate 时建，deactivate 时收束）。
struct PlayerState {
    /// P7：方法/钩子/控制平面发起 rm 调用与对端 HTTP 所需上下文（activate 存入）。
    ctx: Context,
    sessions: SessionManager,
    /// 当前选中的输出设备（P2 `player.outputs.set`；P6 起随设置持久化，附录 B #39）。
    selected_output: Mutex<Option<Selected>>,
    /// 进行中的投屏（P3 `player.cast`；会话 id → 投屏态，附录 B #45）。
    casts: Mutex<HashMap<String, CastState>>,
    /// 运行表（P5 `player.run`；监控线程与停用即杀共享，附录 B 登记）。
    runs: Arc<run::RunRegistry>,
    /// player.db（P6 §5.11）；打开失败 → `None` = 降级纯内存（附录 B 登记）。
    store: Option<store::Store>,
    /// P7 控制平面句柄（activate 起；`None` = 端口全忙降级关闭，附录 B 登记）。
    control: Mutex<Option<control::ControlHandle>>,
    /// P7 同步节流 mark（content_id → (position_sec, Unix秒)；推送成功判定后盖戳）。
    sync_marks: Mutex<HashMap<String, (f64, u64)>>,
    /// P7 接收侧计数（`player.control.status` 观测：合入 / 丢弃[自报+旧报]）。
    sync_applied: AtomicU64,
    sync_ignored: AtomicU64,
}

impl PlayerState {
    fn new(ctx: Context, store: Option<store::Store>) -> Self {
        Self {
            ctx,
            sessions: SessionManager::new(),
            selected_output: Mutex::new(None),
            casts: Mutex::new(HashMap::new()),
            runs: Arc::new(run::RunRegistry::default()),
            store,
            control: Mutex::new(None),
            sync_marks: Mutex::new(HashMap::new()),
            sync_applied: AtomicU64::new(0),
            sync_ignored: AtomicU64::new(0),
        }
    }

    fn selected(&self) -> Option<Selected> {
        self.selected_output.lock().unwrap().clone()
    }

    fn selected_id(&self) -> Option<String> {
        self.selected().map(|s| s.id)
    }

    /// 读已存会话快照（P6 续播；无库/无行 → None）。
    fn saved_session(&self, content_id: &str) -> Option<store::SavedSession> {
        self.store.as_ref().and_then(|s| s.load_session(content_id))
    }

    /// 会话状态写穿（P6）：进度/音量/字幕 upsert 到 player.db。
    /// - 查看器会话 / 无 content_id / 无库 → 跳过（schema 仅承载播放语义）。
    /// - `position_override`：调用方显式覆写（暂未用；语义位保留）；
    ///   缺省下 Finished 相自动归零（播完下次从头看），Playing/Paused 读当前。
    /// - Failed 不落（见事件钩子：避免瞬时错误把上一份进度抹成 0）。
    /// - 写失败 best-effort 忽略——持久化是增强面，绝不打断播放主链。
    /// - P7：写库与同步报文共用 `store.stamp` 同一批盖戳值（本地行与
    ///   推出去的报文 `updated_at` 一致），随后按 `sync` 模式推 §5.9 报文。
    fn persist_session(
        &self,
        session: &session::Session,
        position_override: Option<f64>,
        sync: control::SyncMode,
    ) {
        let (Some(store), Some(cid)) = (self.store.as_ref(), session.content_id.as_deref())
        else {
            return;
        };
        if session.is_viewer() {
            return;
        }
        let pos = match position_override {
            Some(p) => p,
            None => match session.phase() {
                session::Phase::Finished => 0.0,
                _ => session.position(),
            },
        };
        let raw = store::SavedSession {
            position_sec: pos,
            volume: *session.volume.lock().unwrap(),
            subtitle_track: session.subtitle_track(),
            // aid/倍速控制面未落方法（§9 验收面=进度/音量/字幕）——列按
            // schema 预置默认值，附录 B 登记。
            audio_track: 0,
            playback_rate: 1.0,
            updated_at: 0,
            device_id: String::new(),
        };
        let stamped = store.stamp(&raw);
        let _ = store.upsert_session(cid, &stamped);
        self.maybe_sync(cid, &stamped, sync, session.duration());
    }

    /// P7 §5.9 同步推送（写穿成功后调）：节流判定 → 解析目标 → 后台线程
    /// best-effort（2s 超时，失败静默——同步是增强面，绝不打断播放主链）。
    fn maybe_sync(
        &self,
        content_id: &str,
        stamped: &store::SavedSession,
        mode: control::SyncMode,
        duration: f64,
    ) {
        let now = store::now_secs().max(0) as u64;
        let pos = stamped.position_sec;
        let should = {
            let mut marks = self.sync_marks.lock().unwrap();
            let should =
                control::should_sync(mode, marks.get(content_id).copied(), pos, duration, now);
            if should {
                marks.insert(content_id.to_string(), (pos, now));
            }
            should
        };
        if !should {
            return;
        }
        let targets = control::sync_targets(&self.ctx);
        if targets.is_empty() {
            return;
        }
        let payload = serde_json::json!({
            "type": "session.sync",
            "content_id": content_id,
            "position_sec": stamped.position_sec,
            "volume": stamped.volume,
            "subtitle_track": stamped.subtitle_track,
            "updated_at": stamped.updated_at,
            "device_id": stamped.device_id,
        });
        std::thread::spawn(move || control::push_sync(&targets, &payload));
    }

    /// 停用 flush：全部会话落库（必须在 `sessions.clear()` 拆后端之前调）。
    /// P7：flush 即停机同步触发点（Immediate，附录 B 登记）。
    fn flush_sessions(&self) {
        for s in self.sessions.list() {
            self.persist_session(&s, None, control::SyncMode::Immediate);
        }
    }

    /// 停机收束（`deactivate()` 与 `on_deactivate` 双入口幂等调）：
    /// 关控制平面（释端口）→ flush 会话（含停机同步）→ 拆会话 → 杀运行。
    fn teardown(&self) {
        if let Some(mut h) = self.control.lock().unwrap().take() {
            h.shutdown();
        }
        self.flush_sessions();
        self.sessions.clear();
        run::kill_non_background(&self.runs);
    }
}

/// 数据根（§5.11）：`PLAYER_DATA_DIR` 覆盖优先，否则内核约定的插件数据目录。
/// player.db 与 P5 沙箱根统一从这里落（附录 B #19）。
pub(crate) fn data_root(ctx: &Context) -> PathBuf {
    match std::env::var_os("PLAYER_DATA_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => ctx.data_dir(),
    }
}

pub struct Player {
    state: Option<Arc<PlayerState>>,
}

impl Default for Player {
    fn default() -> Self {
        Self { state: None }
    }
}

impl Plugin for Player {
    fn id(&self) -> &str {
        "player"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError> {
        // P6：player.db（§5.11；路径 PLAYER_DATA_DIR 可覆盖，附录 B #19）。
        // 打开失败 → 降级纯内存（持久化是增强面，不阻塞装载）。
        let store = store::Store::open(&data_root(&ctx).join("player.db")).ok();
        let state = Arc::new(PlayerState::new(ctx.clone(), store));

        // #39：输出选择态随设置持久化——重开后恢复；设备已不在枚举
        // （validate 失败）→ 不恢复选择，但设置值保留（下次插回即恢复）。
        if let Some(sel_id) = state.store.as_ref().and_then(|s| s.get_setting("outputs.selected"))
        {
            if let Ok(row) = outputs::validate(&sel_id) {
                *state.selected_output.lock().unwrap() = Some(Selected {
                    id: row.id,
                    kind: row.kind,
                });
            }
        }

        register_methods(&ctx, &state)?;

        // 页面贡献给 core-ui（未装载则忽略，仍可经 player.ui.page 直取）。
        contribute_ui_pages(&ctx, &ctx.data_dir());

        // P3：接收器发现监听（UDP 7916，进程级单例；绑失败降级空表，不阻塞装载）。
        discovery::ensure_listener();

        // P7：控制平面（7917 忙退 7918–7920；全忙 → 关闭降级，不阻塞装载）。
        *state.control.lock().unwrap() = control::start(Arc::clone(&state));

        // 停机收束（双入口幂等）：关控制平面（释端口）→ flush 会话进度（P6/P7
        // 停机同步）→ 收束全部会话（含 mpv 事件线程）→ 杀未授权后台运行（P5 §5.8）。
        let de_state = Arc::clone(&state);
        ctx.on_deactivate(Box::new(move || {
            de_state.teardown();
        }));

        self.state = Some(state);
        Ok(())
    }

    fn deactivate(&mut self) {
        if let Some(state) = self.state.take() {
            state.teardown();
        }
    }
}

/// 注册 §5.12 方法（P0 + P1）。
fn register_methods(ctx: &Context, state: &Arc<PlayerState>) -> Result<(), ActivateError> {
    let reg = |name: &str,
               handler: Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>| {
        ctx.register_method(name, handler, Box::new(|| {}), None)
            .map_err(|e| ActivateError::new(e.to_string()))
    };

    // ===== player.play =====
    {
        let ctx2 = ctx.clone();
        let st = Arc::clone(state);
        reg(
            "player.play",
            Box::new(move |args| play(&ctx2, &st, args).map_err(CallError::from)),
        )?;
    }
    // ===== player.pause =====
    {
        let st = Arc::clone(state);
        reg(
            "player.pause",
            Box::new(move |args| {
                let session = st.sessions.get(&str_field(&args, "session_id")?)?;
                session.pause().map_err(CallError::from)?;
                st.persist_session(&session, None, control::SyncMode::Immediate);
                Ok(serde_json::json!({}))
            }),
        )?;
    }
    // ===== player.resume =====
    {
        let st = Arc::clone(state);
        reg(
            "player.resume",
            Box::new(move |args| {
                let session = st.sessions.get(&str_field(&args, "session_id")?)?;
                session.resume().map_err(CallError::from)?;
                st.persist_session(&session, None, control::SyncMode::Immediate);
                Ok(serde_json::json!({}))
            }),
        )?;
    }
    // ===== player.seek =====
    {
        let st = Arc::clone(state);
        reg(
            "player.seek",
            Box::new(move |args| {
                let session = st.sessions.get(&str_field(&args, "session_id")?)?;
                let pos = args
                    .get("position_sec")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| {
                        CallError::from(PlayerError::bad_param("缺少 position_sec"))
                    })?;
                session.seek(pos).map_err(CallError::from)?;
                // seek = 进度变化 → 节流（1% 规则对跳转即刻命中，§5.9 触发表）。
                st.persist_session(&session, None, control::SyncMode::Throttled);
                Ok(serde_json::json!({}))
            }),
        )?;
    }
    // ===== player.set_volume =====
    {
        let st = Arc::clone(state);
        reg(
            "player.set_volume",
            Box::new(move |args| {
                let session = st.sessions.get(&str_field(&args, "session_id")?)?;
                let v = args.get("volume").and_then(Value::as_f64).ok_or_else(|| {
                    CallError::from(PlayerError::bad_param("缺少 volume"))
                })?;
                session.set_volume(v).map_err(CallError::from)?;
                st.persist_session(&session, None, control::SyncMode::Immediate);
                Ok(serde_json::json!({}))
            }),
        )?;
    }
    // ===== player.stop =====
    {
        let ctx2 = ctx.clone();
        let st = Arc::clone(state);
        reg(
            "player.stop",
            Box::new(move |args| {
                let sid = str_field(&args, "session_id")?;
                let session = st.sessions.get(&sid)?;
                // P6：先落库（stop_playback 后 time-pos 归零，抓不到已播位置）。
                // P7：停止 → 立即同步（§5.9「暂停/停止」行）。
                st.persist_session(&session, None, control::SyncMode::Immediate);
                session.stop();
                // P3：会话停止 → 结束投屏（摘路由/通知对端/cast.ended，附录 B #45）。
                cast::end_cast(&ctx2, &st, &sid, "stopped");
                // P0：停掉即摘（会话记录在 player.db 保留，供重开续播，附录 B #63）。
                st.sessions.remove(&sid);
                Ok(serde_json::json!({}))
            }),
        )?;
    }
    // ===== player.status =====
    {
        let st = Arc::clone(state);
        reg(
            "player.status",
            Box::new(move |args| {
                let out = match args.get("session_id").and_then(Value::as_str) {
                    Some(id) => vec![st.sessions.get(id)?.status()],
                    None => st.sessions.list().iter().map(|s| s.status()).collect(),
                };
                Ok(serde_json::json!({ "sessions": out, "playing": out
                    .iter()
                    .any(|s| s["playing"] == true) }))
            }),
        )?;
    }
    // ===== player.goto_page（P1：PDF 翻页，1-based） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.goto_page",
            Box::new(move |args| {
                let session = st.sessions.get(&str_field(&args, "session_id")?)?;
                let n = args.get("page").and_then(Value::as_i64).ok_or_else(|| {
                    CallError::from(PlayerError::bad_param("缺少 page"))
                })?;
                if !(1..=u32::MAX as i64).contains(&n) {
                    return Err(CallError::from(PlayerError::bad_param("page 从 1 开始")));
                }
                session.goto_page(n as u32).map_err(CallError::from)?;
                let st_now = session.status();
                Ok(serde_json::json!({ "page": st_now["page"], "pages": st_now["pages"] }))
            }),
        )?;
    }
    // ===== player.frame（P1：查看器帧 → PNG base64） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.frame",
            Box::new(move |args| {
                let session = st.sessions.get(&str_field(&args, "session_id")?)?;
                let frame = session.frame().map_err(CallError::from)?;
                let (data, width, height) = frame.png_base64().map_err(CallError::from)?;
                Ok(serde_json::json!({
                    "mime": "image/png",
                    "data": data,
                    "width": width,
                    "height": height,
                }))
            }),
        )?;
    }
    // ===== player.set_subtitle（P1：字幕轨选择/关闭） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.set_subtitle",
            Box::new(move |args| {
                let session = st.sessions.get(&str_field(&args, "session_id")?)?;
                let n = args.get("track").and_then(Value::as_i64).ok_or_else(|| {
                    CallError::from(PlayerError::bad_param("缺少 track"))
                })?;
                if !(i32::MIN as i64..=i32::MAX as i64).contains(&n) {
                    return Err(CallError::from(PlayerError::bad_param("track 超出范围")));
                }
                session.set_subtitle(n as i32).map_err(CallError::from)?;
                st.persist_session(&session, None, control::SyncMode::Immediate);
                Ok(serde_json::json!({ "subtitle_track": session.subtitle_track() }))
            }),
        )?;
    }
    // ===== player.outputs.list（P2：本机输出设备枚举） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.outputs.list",
            Box::new(move |_args| {
                let sel = st.selected_id();
                Ok(outputs::list_value(sel.as_deref()))
            }),
        )?;
    }
    // ===== player.outputs.set（P2：切换输出设备，§9 验收「能切换」；原表未列 → 附录 B #39） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.outputs.set",
            Box::new(move |args| {
                let output_id = str_field(&args, "output_id")?;
                let row = outputs::validate(&output_id)?;
                *st.selected_output.lock().unwrap() = Some(Selected {
                    id: row.id.clone(),
                    kind: row.kind.clone(),
                });
                // P6/#39：选择态随设置持久化（重开 activate 恢复；写失败忽略）。
                if let Some(store) = st.store.as_ref() {
                    let _ = store.set_setting("outputs.selected", &row.id);
                }
                // audio：即时应用到全部 mpv 会话（单会话失败不回滚——设备已过枚举校验，
                // 新会话仍会继承选择）；display：headless 无窗口可迁，仅记录选择。
                let mut applied = 0u32;
                if row.kind == "audio" {
                    for s in st.sessions.list() {
                        if !s.is_viewer() && s.apply_audio_device(&row.id).is_ok() {
                            applied += 1;
                        }
                    }
                }
                Ok(serde_json::json!({
                    "output_id": row.id,
                    "kind": row.kind,
                    "applied": applied,
                }))
            }),
        )?;
    }
    // ===== player.receiver.list（P3：局域网接收器发现面；原表未列 → 附录 B #42） =====
    reg(
        "player.receiver.list",
        Box::new(|_args| {
            // discovery_port：7916 忙时监听方退到的临时端口（lite --hb 指向它；
            // 附录 B #44）。0 = 发现面未起（降级空表）。
            Ok(serde_json::json!({
                "rows": discovery::list_rows(),
                "discovery_port": discovery::listen_port(),
            }))
        }),
    )?;
    // ===== player.cast（P3 §5.12：模式 A 投屏） =====
    {
        let ctx2 = ctx.clone();
        let st = Arc::clone(state);
        reg(
            "player.cast",
            Box::new(move |args| cast::cast(&ctx2, &st, &args).map_err(CallError::from)),
        )?;
    }
    // ===== player.cast.stop（P3：结束投屏；原表未列 → 附录 B #42） =====
    {
        let ctx2 = ctx.clone();
        let st = Arc::clone(state);
        reg(
            "player.cast.stop",
            Box::new(move |args| cast::stop(&ctx2, &st, &args).map_err(CallError::from)),
        )?;
    }
    // ===== player.run（P5 §5.8：沙箱运行；placement_id 兼容同 #23 口径） =====
    {
        let ctx2 = ctx.clone();
        let st = Arc::clone(state);
        reg(
            "player.run",
            Box::new(move |args| run::run(&ctx2, &st.runs, args).map_err(CallError::from)),
        )?;
    }
    // ===== player.run.list（P5：运行列表；原表未列 → 附录 B 登记） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.run.list",
            Box::new(move |_args| Ok(run::list(&st.runs))),
        )?;
    }
    // ===== player.run.stop（P5：显式停运行（含 background 授权的）；原表未列 → 附录 B 登记） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.run.stop",
            Box::new(move |args| run::stop(&st.runs, &args).map_err(CallError::from)),
        )?;
    }
    // ===== player.remote.*（P7 §5.6/§5.15：远程执行发起面；原表未列 → 附录 B 登记） =====
    {
        let ctx2 = ctx.clone();
        reg(
            "player.remote.caps",
            Box::new(move |args| control::remote_caps(&ctx2, &args)),
        )?;
    }
    {
        let ctx2 = ctx.clone();
        reg(
            "player.remote.run",
            Box::new(move |args| control::remote_run(&ctx2, &args)),
        )?;
    }
    {
        let ctx2 = ctx.clone();
        reg(
            "player.remote.status",
            Box::new(move |args| control::remote_status(&ctx2, &args)),
        )?;
    }
    {
        let ctx2 = ctx.clone();
        reg(
            "player.remote.stop",
            Box::new(move |args| control::remote_stop(&ctx2, &args)),
        )?;
    }
    {
        let ctx2 = ctx.clone();
        reg(
            "player.remote.input",
            Box::new(move |args| control::remote_input(&ctx2, &args)),
        )?;
    }
    {
        let ctx2 = ctx.clone();
        reg(
            "player.remote.log",
            Box::new(move |args| control::remote_log(&ctx2, &args)),
        )?;
    }
    // ===== player.control.status（P7：控制平面/同步观测面；e2e 轮询用；附录 B 登记） =====
    {
        let st = Arc::clone(state);
        reg(
            "player.control.status",
            Box::new(move |_args| {
                let port = st.control.lock().unwrap().as_ref().map(|h| h.port);
                Ok(control::control_status_value(&st, port))
            }),
        )?;
    }
    // ===== player.ui.page =====
    reg(
        "player.ui.page",
        Box::new(|args| {
            let path = args["path"].as_str().unwrap_or("/player/control");
            let html = ui::page_html(path)
                .ok_or_else(|| PlayerError::bad_param(format!("未知页面：{path}")))?;
            Ok(serde_json::json!({ "path": path, "html": html }))
        }),
    )?;
    Ok(())
}

/// browse 行解析出的播放目标（P1：带资产类型用于分流）。
struct Target {
    placement_id: i64,
    content_id: Option<String>,
    asset_type: String,
    rel_path: String,
}

/// `player.play`：placement_id 或 content_id → 在线放置 → 按资产类型分流。
fn play(
    ctx: &Context,
    state: &Arc<PlayerState>,
    args: Value,
) -> Result<Value, PlayerError> {
    let target = resolve_target(ctx, &args)?;
    // §4.2 分流；不支持类型先拒（P103）——不开流、不留副作用。
    let route = asset::route(&target.asset_type, &target.rel_path)?;

    // Source：经 ling 调 rm 流交付（03 §2.6）。
    let opened = ctx
        .call(
            "resources-manager",
            "resources-manager.stream.open",
            serde_json::json!({ "placement_id": target.placement_id, "purpose": "play" }),
        )
        .map_err(|e| from_rm_err(&e))?;
    let url = opened["url"]
        .as_str()
        .ok_or_else(|| PlayerError::content_unavailable("stream.open 未返回 url"))?
        .to_string();
    let content_len = opened["content_len"].as_u64().unwrap_or(0);
    let stream_id = opened["stream_id"].as_str().unwrap_or("").to_string();

    let value = match route {
        Route::Player => play_mpv(ctx, state, &target, url)?,
        Route::ViewerImage => play_image(state, &target, url)?,
        Route::ViewerPdf => play_pdf(state, &target, url)?,
    };

    let _ = ctx.emit(
        "player.playback.started",
        serde_json::json!({
            "session_id": value["session_id"],
            "content_id": target.content_id,
            "placement_id": target.placement_id,
            "stream_id": stream_id,
            "asset_type": target.asset_type,
        }),
    );

    let mut out = value;
    out["content_len"] = serde_json::json!(content_len);
    out["stream_id"] = serde_json::json!(stream_id);
    Ok(out)
}

/// video/audio：mpv 后端（P0 路径）+ P1 轨探测（ffprobe，失败降级空表）。
fn play_mpv(
    ctx: &Context,
    state: &Arc<PlayerState>,
    target: &Target,
    url: String,
) -> Result<Value, PlayerError> {
    // Capability：起后端（事件回调 → 回写 phase + 广播）。
    // 会话 id 在 insert 后才有——回调持「待挂」id，insert 后回填；事件到达
    // 时经会话表查回 session 做 phase 迁移。
    let pending_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_id2 = Arc::clone(&pending_id);
    let emit_ctx = ctx.clone();
    let st_for_events = Arc::clone(state);
    let backend = MpvBackend::start(move |ev| {
        let Some(sid) = pending_id2.lock().unwrap().clone() else { return };
        let session = st_for_events.sessions.get(&sid).ok();
        if let Some(s) = session.as_ref() {
            let _ = s.on_backend_event(&ev);
        }
        match ev {
            BackendEvent::Progress { position_sec } => {
                let _ = emit_ctx.emit(
                    "player.playback.progress",
                    serde_json::json!({"session_id": sid, "position_sec": position_sec}),
                );
                // P6 写穿（约 1s 心跳；失败忽略）；P7 节流同步（5s/1% 先到者）。
                if let Some(s) = session.as_ref() {
                    st_for_events.persist_session(s, None, control::SyncMode::Throttled);
                }
            }
            BackendEvent::Paused { position_sec } => {
                let _ = emit_ctx.emit(
                    "player.playback.paused",
                    serde_json::json!({"session_id": sid, "position_sec": position_sec}),
                );
                if let Some(s) = session.as_ref() {
                    // P7：暂停 → 立即同步（§5.9 触发表）。
                    st_for_events.persist_session(s, None, control::SyncMode::Immediate);
                }
            }
            BackendEvent::Finished => {
                let _ = emit_ctx.emit(
                    "player.playback.finished",
                    serde_json::json!({"session_id": sid}),
                );
                // phase 已迁 Finished → persist 自动归零（播完从头看）；
                // P7：播完归零报文立即同步（接力侧也从头看）。
                if let Some(s) = session.as_ref() {
                    st_for_events.persist_session(s, None, control::SyncMode::Immediate);
                }
            }
            BackendEvent::Failed { message } => {
                let _ = emit_ctx.emit(
                    "player.playback.finished",
                    serde_json::json!({"session_id": sid, "error": message}),
                );
                // Failed 不落库：瞬时错误（token 过期/网络抖动）会把 time-pos
                // 打成 0 或读空，写下去反而抹掉上一份有效进度。
            }
        }
    })?;

    // Sink 绑定 + P6 续播（§5.11）：有已存快照 → load_at（等装载就绪再绝对
    // 定位，且给下面的音量/字幕回放一个已装载的文件）；无快照走原 load。
    let saved = target
        .content_id
        .as_deref()
        .and_then(|cid| state.saved_session(cid));
    let resume_pos = saved.as_ref().map(|s| s.position_sec).unwrap_or(0.0);
    if saved.is_some() {
        backend.load_at(&url, resume_pos)?;
    } else {
        backend.load(&url)?;
    }

    let session = state.sessions.insert_player(
        target.placement_id,
        target.content_id.clone(),
        url.clone(),
        target.asset_type.clone(),
        backend,
    );
    *pending_id.lock().unwrap() = Some(session.id.clone());

    // P1 §5.10：ffprobe 枚举轨（缺工具/失败 → 空表，set_subtitle 仍可裸用）。
    session.set_tracks(tracks::probe(&url));

    // P2：继承已选音频输出设备（best effort，设备名已过枚举校验）。
    if let Some(sel) = state.selected() {
        if sel.kind == "audio" {
            let _ = session.apply_audio_device(&sel.id);
        }
    }

    // P6：回放音量/字幕（尽力而为——内容变更后轨位对不上不拖死播放）。
    if let Some(s) = saved.as_ref() {
        let _ = session.set_volume(s.volume);
        let _ = session.set_subtitle(s.subtitle_track);
    }

    Ok(serde_json::json!({
        "session_id": session.id,
        "asset_type": target.asset_type,
        // P6：续播回显（§5.12 play 响应增补，附录 B 登记）。
        "resumed": resume_pos > 0.0,
        "resume_position_sec": resume_pos,
    }))
}

/// image：解码为帧常驻会话（frame 接口输出）。
fn play_image(
    state: &Arc<PlayerState>,
    target: &Target,
    url: String,
) -> Result<Value, PlayerError> {
    let bytes = fetch_bytes(&url)?;
    let frame = ImageFrame::from_bytes(&bytes)?;
    let (width, height) = (frame.width, frame.height);
    let session = state.sessions.insert_viewer(
        target.placement_id,
        target.content_id.clone(),
        url,
        target.asset_type.clone(),
        ViewerState::Image { frame },
    );
    Ok(serde_json::json!({
        "session_id": session.id,
        "asset_type": target.asset_type,
        "width": width,
        "height": height,
    }))
}

/// doc（PDF）：计页 + 渲染第 1 页常驻。
fn play_pdf(
    state: &Arc<PlayerState>,
    target: &Target,
    url: String,
) -> Result<Value, PlayerError> {
    let bytes = fetch_bytes(&url)?;
    let pages = pdf_cap::count_pages(&bytes)?;
    let frame = pdf_cap::render_page(&bytes, 1)?;
    let (width, height) = (frame.width, frame.height);
    let session = state.sessions.insert_viewer(
        target.placement_id,
        target.content_id.clone(),
        url,
        target.asset_type.clone(),
        ViewerState::Pdf {
            bytes: Arc::new(bytes),
            page: 1,
            pages,
            frame,
        },
    );
    Ok(serde_json::json!({
        "session_id": session.id,
        "asset_type": target.asset_type,
        "page": 1,
        "pages": pages,
        "width": width,
        "height": height,
    }))
}

/// 拉取流字节（图片/PDF 源）。403 → P102（token 失效），其余传输/HTTP 错 → P101。
fn fetch_bytes(url: &str) -> Result<Vec<u8>, PlayerError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| PlayerError::content_unavailable(format!("HTTP 客户端失败：{e}")))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| PlayerError::content_unavailable(format!("读取流失败：{e}")))?;
    if resp.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(PlayerError::new(
            "P102",
            format!("流 token 失效：HTTP 403 {url}"),
        ));
    }
    let resp = resp
        .error_for_status()
        .map_err(|e| PlayerError::content_unavailable(format!("读取流失败：{e}")))?;
    resp.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| PlayerError::content_unavailable(format!("读取流失败：{e}")))
}

/// 从入参解析本机可播目标（带资产类型；P1 起扫描 browse 行）：
/// - `placement_id` 直给 → browse 找行取 asset_type/rel_path/content_id
/// - `content_id` → browse 分页检索在线放置（§5.12 主接口）
fn resolve_target(ctx: &Context, args: &Value) -> Result<Target, PlayerError> {
    if let Some(pid) = args.get("placement_id") {
        let placement_id = match pid {
            Value::Number(n) => n
                .as_i64()
                .ok_or_else(|| PlayerError::content_unavailable("placement_id 不是整数"))?,
            Value::String(s) => s
                .parse::<i64>()
                .map_err(|_| PlayerError::content_unavailable("placement_id 不是整数"))?,
            _ => return Err(PlayerError::content_unavailable("placement_id 缺失")),
        };
        let row = scan_browse(ctx, |r| r["placement_id"].as_i64() == Some(placement_id))?
            .ok_or_else(|| {
                PlayerError::content_unavailable(format!("本机没有该放置：{placement_id}"))
            })?;
        return target_from_row(row, args);
    }
    let content_id = args
        .get("content_id")
        .and_then(Value::as_str)
        .ok_or_else(|| PlayerError::bad_param("缺少 placement_id 或 content_id"))?
        .to_string();
    let row = scan_browse(ctx, |r| {
        r["content_id"].as_str() == Some(content_id.as_str())
            && r["state"].as_str() == Some("online")
    })?
    .ok_or_else(|| {
        PlayerError::content_unavailable(format!(
            "本机没有该内容的在线放置：{content_id}"
        ))
    })?;
    target_from_row(row, args)
}

/// browse 分页扫描（limit 500；offset 上限 10 万），返回首个匹配行。
fn scan_browse(
    ctx: &Context,
    pred: impl Fn(&Value) -> bool,
) -> Result<Option<Value>, PlayerError> {
    let mut offset = 0u32;
    loop {
        let page = ctx
            .call(
                "resources-manager",
                "resources-manager.browse.query",
                serde_json::json!({ "page": { "limit": 500, "offset": offset } }),
            )
            .map_err(|e| from_rm_err(&e))?;
        let rows = page["rows"].as_array().cloned().unwrap_or_default();
        let total = page["total"].as_u64().unwrap_or(0);
        if rows.is_empty() {
            break;
        }
        for row in rows {
            if pred(&row) {
                return Ok(Some(row));
            }
        }
        offset += 500;
        if (offset as u64) >= total || offset > 100_000 {
            break;
        }
    }
    Ok(None)
}

fn target_from_row(row: Value, args: &Value) -> Result<Target, PlayerError> {
    let placement_id = row["placement_id"].as_i64().ok_or_else(|| {
        PlayerError::content_unavailable("browse 行缺 placement_id")
    })?;
    let asset_type = row["asset_type"].as_str().unwrap_or("other").to_string();
    let rel_path = row["rel_path"].as_str().unwrap_or("").to_string();
    let content_id = args
        .get("content_id")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| row["content_id"].as_str().map(String::from));
    Ok(Target { placement_id, content_id, asset_type, rel_path })
}

/// 取字符串入参（session_id 等）。
fn str_field(args: &Value, key: &str) -> Result<String, PlayerError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| PlayerError::bad_param(format!("缺少 {key}")))
}

/// 页面贡献（rm 同款：asset_dir + core-ui.ui.contribute）。
fn contribute_ui_pages(ctx: &Context, data_dir: &std::path::Path) {
    let asset_dir = data_dir.join("ui");
    let _ = std::fs::create_dir_all(&asset_dir);
    for page in ui::pages() {
        let page_assets = asset_dir.join(page.path.trim_start_matches('/'));
        let _ = std::fs::create_dir_all(&page_assets);
        let _ = std::fs::write(page_assets.join("index.html"), page.html());
        let args = serde_json::json!({
            "plugin": "player",
            "type": "page",
            "path": page.path,
            "title": page.title,
            "icon": page.icon,
            "order": page.order,
            "asset_dir": page_assets.display().to_string(),
        });
        let _ = ctx.call("core-ui", "core-ui.ui.contribute", args);
    }
}

/// ling 装载约定的入口符号（`ling/docs/04` §2）。
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn ling_sdk::Plugin {
    Box::into_raw(Box::new(Player::default()))
}
