//! libmpv 后端（`docs/01` §5.5/§5.14）：播放控制与事件源。
//!
//! - 解码/渲染/音画同步/字幕交给 libmpv（`keepaspect` 默认开 → 比例规则同 §5.5）
//! - 无显示环境（CI/无 DISPLAY）→ `vo=null ao=null` 仅解码（P0 验收走此路径）
//! - 事件走独立 client 句柄：`time-pos`（progress 节流在上层）、`pause`、`eof`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libmpv2::events::{Event, PropertyData};
use libmpv2::{Format, Mpv};

use crate::error::PlayerError;

/// 输出配置：headless 用 null 渲染（测试/无显示器），否则 mpv 自开窗口。
///
/// - `PLAYER_VO` 显式设置 → 强制 headless 路径（vo 取其值，默认 `null`）
/// - 否则按 `DISPLAY` / `WAYLAND_DISPLAY` 缺失判定
fn detect_headless() -> bool {
    if std::env::var("PLAYER_VO").is_ok() {
        return true;
    }
    let no_display = std::env::var("DISPLAY").map(|d| d.is_empty()).unwrap_or(true);
    let no_wayland = std::env::var("WAYLAND_DISPLAY").map(|d| d.is_empty()).unwrap_or(true);
    no_display && no_wayland
}

/// `MPV_END_FILE_REASON_EOF`（libmpv2 未 re-export 常量，按绑定值引用）。
const MPV_EOF: u32 = 0;
/// `MPV_END_FILE_REASON_ERROR`。
const MPV_ERROR: u32 = 4;

/// P0 播放后端：一个会话一个 mpv 实例（事件句柄由事件泵线程持有）。
pub struct MpvBackend {
    mpv: Mpv,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

/// 事件回调载荷（后端线程 → 会话层）。
#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// 进度心跳（≥1 秒节流，后端内完成）。
    Progress { position_sec: f64 },
    /// 暂停态变化。
    Paused { position_sec: f64 },
    /// 正常播完（eof）。
    Finished,
    /// 解码/加载失败。
    Failed { message: String },
}

impl MpvBackend {
    /// 建实例并开始事件泵。`on_event` 在后端线程调用。
    pub fn start<F>(on_event: F) -> Result<Self, PlayerError>
    where
        F: Fn(BackendEvent) + Send + 'static,
    {
        let headless = detect_headless();
        let mpv = Mpv::with_initializer(|init| {
            if headless {
                init.set_property("vo", "null")?;
                init.set_property("ao", "null")?;
            }
            // §5.5：等比缩放、不裁切（mpv 默认即此，显式声明意图）。
            init.set_option("keepaspect", true)?;
            Ok(())
        })
        .map_err(|e| PlayerError::no_decoder(format!("libmpv 初始化失败：{e}")))?;

        // 独立事件队列句柄。注意：libmpv2 6.0.0 的 `create_client(Some(name))`
        // 有悬垂 bug（CString 在 if-let 块尾提前释放 → 传入野指针 → mpv 返回
        // NULL → NonNull UB abort）；name 只是装饰，用 None 绕开。
        let events = mpv
            .create_client(None)
            .map_err(|e| PlayerError::no_decoder(format!("创建事件句柄失败：{e}")))?;
        events
            .observe_property("time-pos", Format::Double, 1)
            .map_err(|e| PlayerError::no_decoder(format!("observe time-pos 失败：{e}")))?;
        events
            .observe_property("pause", Format::Flag, 2)
            .map_err(|e| PlayerError::no_decoder(format!("observe pause 失败：{e}")))?;
        events
            .observe_property("eof-reached", Format::Flag, 4)
            .map_err(|e| PlayerError::no_decoder(format!("observe eof 失败：{e}")))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("player-mpv-events".into())
            .spawn(move || event_pump(events, on_event, stop2))
            .map_err(|e| PlayerError::no_decoder(format!("事件线程失败：{e}")))?;

        Ok(Self { mpv, stop, join: Some(join) })
    }

    /// 载入流 URL 并播放（`replace` 清单当前项）。
    pub fn load(&self, url: &str) -> Result<(), PlayerError> {
        self.mpv
            .command("loadfile", &[url, "replace"])
            .map_err(|e| PlayerError::no_decoder(format!("加载失败：{e}")))
    }

    /// 载入并续播（P6 §5.11）：mpv `loadfile` 异步装载，seek/`sid` 回放早于
    /// 装载都会落空 → 先轮询 `duration`（>0 = 解复用就绪，≤3s），就绪后按需
    /// 绝对定位；迟迟不就绪则跳过定位继续播——恢复是增强面，不拖死播放链。
    pub fn load_at(&self, url: &str, start_sec: f64) -> Result<(), PlayerError> {
        self.load(url)?;
        let mut loaded = false;
        for _ in 0..60 {
            if self
                .mpv
                .get_property::<f64>("duration")
                .map(|d| d > 0.0)
                .unwrap_or(false)
            {
                loaded = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if start_sec > 0.0 && loaded {
            return self.seek(start_sec);
        }
        Ok(())
    }

    pub fn set_pause(&self, paused: bool) -> Result<(), PlayerError> {
        self.mpv
            .set_property("pause", paused)
            .map_err(|e| PlayerError::no_decoder(format!("暂停控制失败：{e}")))
    }

    /// 当前是否暂停（P2 输出抽象/状态轮询备用）。
    #[allow(dead_code)]
    pub fn is_paused(&self) -> bool {
        self.mpv.get_property::<bool>("pause").unwrap_or(false)
    }

    /// 绝对定位（秒）。
    pub fn seek(&self, position_sec: f64) -> Result<(), PlayerError> {
        let target = position_sec.max(0.0).to_string();
        self.mpv
            .command("seek", &[&target, "absolute"])
            .map_err(|e| PlayerError::no_decoder(format!("seek 失败：{e}")))
    }

    pub fn position(&self) -> f64 {
        self.mpv.get_property::<f64>("time-pos").unwrap_or(0.0)
    }

    pub fn duration(&self) -> f64 {
        self.mpv.get_property::<f64>("duration").unwrap_or(0.0)
    }

    /// 音量 0.0–1.0（对外）→ mpv 0–100。
    pub fn set_volume(&self, volume: f64) -> Result<(), PlayerError> {
        let v = (volume.clamp(0.0, 1.0) * 100.0).round() as i64;
        self.mpv
            .set_property("volume", v)
            .map_err(|e| PlayerError::no_decoder(format!("音量设置失败：{e}")))
    }

    pub fn stop_playback(&self) {
        let _ = self.mpv.command("stop", &[]);
    }

    /// 切换音频输出设备（P2；mpv `audio-device` 可运行时 set，回读校验）。
    /// 设备名已由 `outputs::validate` 对照枚举核过；失败 → P100。
    pub fn set_audio_device(&self, device: &str) -> Result<(), PlayerError> {
        self.mpv
            .set_property("audio-device", device)
            .map_err(|_| PlayerError::new("P100", format!("音频设备设置失败：{device}")))?;
        match self.mpv.get_property::<String>("audio-device") {
            Ok(cur) if cur == device => Ok(()),
            _ => Err(PlayerError::new("P100", format!("音频设备回读不匹配：{device}"))),
        }
    }

    /// 选择字幕轨（P1；`track`：1-based sid，-1=关闭）。
    ///
    /// mpv `sid` 实证编号：按类型 1-based（video/audio/sub 各自从 1）。
    /// `set`/`set_property` 对无效值可能静默失败 → 一律回读校验，
    /// 不匹配 → **P100**。关闭路径：`sid=no` 后 `sid` 属性不可读即成功。
    pub fn set_subtitle_track(&self, track: i32) -> Result<(), PlayerError> {
        if track < 0 {
            let _ = self.mpv.command("set", &["sid", "no"]);
            if self.mpv.get_property::<i64>("sid").is_ok() {
                return Err(PlayerError::new("P100", "关闭字幕失败"));
            }
            return Ok(());
        }
        self.mpv
            .set_property("sid", track as i64)
            .map_err(|_| PlayerError::new("P100", format!("无此字幕轨：sid={track}")))?;
        match self.mpv.get_property::<i64>("sid") {
            Ok(cur) if cur == track as i64 => Ok(()),
            _ => Err(PlayerError::new("P100", format!("无此字幕轨：sid={track}"))),
        }
    }

    /// 当前字幕轨（无/关 → -1）。
    pub fn subtitle_track(&self) -> i32 {
        self.mpv
            .get_property::<i64>("sid")
            .map(|v| v as i32)
            .unwrap_or(-1)
    }

    /// 测试专用：headless 实例 + 空事件回调（unit 门禁路径用）。
    #[cfg(test)]
    pub fn new_headless_for_test() -> Self {
        Self::start(|_| {}).expect("test mpv init")
    }

    /// 收束事件线程（deactivate/stop 调）。
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // 唤醒 wait_event（超时兜底即可，事件泵 0.5s 心跳）。
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for MpvBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 事件泵：wait_event 循环；progress ≥1s 节流；eof → Finished。
fn event_pump<F>(events: Mpv, on_event: F, stop: Arc<AtomicBool>)
where
    F: Fn(BackendEvent),
{
    let mut last_progress = Instant::now() - Duration::from_secs(2);
    let mut last_pos = 0.0;
    while !stop.load(Ordering::SeqCst) {
        // 0.5s 心跳：兼顾 stop 反应与 progress 节流采样。
        let ev = match events.wait_event(0.5) {
            Some(Ok(e)) => e,
            Some(Err(_)) => {
                // 句柄失效 → 收束。
                break;
            }
            None => {
                // 超时：若在播放，补发节流进度。
                continue;
            }
        };
        match ev {
            Event::PropertyChange { name: "time-pos", change: PropertyData::Double(t), .. } => {
                last_pos = t;
                if last_progress.elapsed() >= Duration::from_secs(1) {
                    last_progress = Instant::now();
                    on_event(BackendEvent::Progress { position_sec: t });
                }
            }
            Event::PropertyChange { name: "pause", change: PropertyData::Flag(p), .. } => {
                if p {
                    on_event(BackendEvent::Paused { position_sec: last_pos });
                }
            }
            Event::PropertyChange { name: "eof-reached", change: PropertyData::Flag(true), .. } => {
                on_event(BackendEvent::Finished);
                break;
            }
            Event::EndFile(reason) => {
                // mpv_end_file_reason：0=EOF，4=ERROR（其余为 Stop/Quit/Redirect，上层 stop 路径）。
                if reason == MPV_EOF {
                    on_event(BackendEvent::Finished);
                    break;
                } else if reason == MPV_ERROR {
                    on_event(BackendEvent::Failed {
                        message: "播放中止（mpv error）".into(),
                    });
                    break;
                }
            }
            Event::Shutdown => break,
            _ => {}
        }
    }
}
