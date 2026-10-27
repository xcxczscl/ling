//! 会话状态机与会话表（`docs/01` §5.11/§5.12）。
//!
//! P0：mpv 播放会话（`playing ⇄ paused → finished / stopped`）。
//! P1：新增**查看器会话**（image/doc——无 mpv 后端，持帧与页码）；音视频
//! 控制方法对查看器一律 **P103**，`goto_page`/`frame` 仅查看器可用。
//! 对未知会话一律 **P401**。P6 落 player.db。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::backend::{BackendEvent, MpvBackend};
use crate::error::PlayerError;
use crate::image_cap::ImageFrame;
use crate::tracks::Tracks;

/// 播放阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Playing,
    Paused,
    Finished,
    Stopped,
}

/// 查看器会话状态（P1）。
pub enum ViewerState {
    /// 图片：整图帧常驻（frame 接口输出）。
    Image { frame: ImageFrame },
    /// PDF：源字节（翻页重渲染）+ 当前页帧。
    Pdf {
        bytes: Arc<Vec<u8>>,
        page: u32,
        pages: u32,
        frame: ImageFrame,
    },
}

impl ViewerState {
    pub fn kind(&self) -> &'static str {
        match self {
            ViewerState::Image { .. } => "image",
            ViewerState::Pdf { .. } => "pdf",
        }
    }
    pub fn frame(&self) -> &ImageFrame {
        match self {
            ViewerState::Image { frame } => frame,
            ViewerState::Pdf { frame, .. } => frame,
        }
    }
}

/// 单会话：后端/查看器二选一 + 阶段 + 元信息。
pub struct Session {
    pub id: String,
    pub placement_id: i64,
    pub content_id: Option<String>,
    /// 会话源 URL（P6 续播/审计用；不外发——含一次性 token）。
    #[allow(dead_code)]
    pub url: String,
    /// 资产类型（video/audio/image/doc…，随 status 外发）。
    pub asset_type: String,
    pub phase: Mutex<Phase>,
    pub volume: Mutex<f64>,
    backend: Mutex<Option<MpvBackend>>,
    /// `Some` ⇒ 查看器会话（image/doc），无 mpv。
    viewer: Mutex<Option<ViewerState>>,
    /// ffprobe 轨清单（mpv 会话；P1 首版 play 时探测，失败为空表）。
    tracks: Mutex<Option<Tracks>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let viewer = self.viewer.lock().unwrap();
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("placement_id", &self.placement_id)
            .field("content_id", &self.content_id)
            .field("asset_type", &self.asset_type)
            .field("kind", &viewer.as_ref().map(|v| v.kind()))
            .field("phase", &self.phase())
            .field("volume", &*self.volume.lock().unwrap())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// mpv 播放会话（video/audio）。
    pub fn new_player(
        id: String,
        placement_id: i64,
        content_id: Option<String>,
        url: String,
        asset_type: String,
        backend: MpvBackend,
    ) -> Self {
        Self {
            id,
            placement_id,
            content_id,
            url,
            asset_type,
            phase: Mutex::new(Phase::Playing),
            volume: Mutex::new(1.0),
            backend: Mutex::new(Some(backend)),
            viewer: Mutex::new(None),
            tracks: Mutex::new(None),
        }
    }

    /// 查看器会话（image/doc）：无 mpv 后端。
    pub fn new_viewer(
        id: String,
        placement_id: i64,
        content_id: Option<String>,
        url: String,
        asset_type: String,
        viewer: ViewerState,
    ) -> Self {
        Self {
            id,
            placement_id,
            content_id,
            url,
            asset_type,
            phase: Mutex::new(Phase::Playing),
            volume: Mutex::new(1.0),
            backend: Mutex::new(None),
            viewer: Mutex::new(Some(viewer)),
            tracks: Mutex::new(None),
        }
    }

    pub fn phase(&self) -> Phase {
        *self.phase.lock().unwrap()
    }

    pub fn is_viewer(&self) -> bool {
        self.viewer.lock().unwrap().is_some()
    }

    /// 应用音频输出设备（P2；mpv 会话实时切换，查看器无音频面——调用方把关）。
    pub fn apply_audio_device(&self, device: &str) -> Result<(), PlayerError> {
        self.with_backend(|b| b.set_audio_device(device))
    }

    pub fn position(&self) -> f64 {
        self.backend
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.position())
            .unwrap_or(0.0)
    }

    pub fn duration(&self) -> f64 {
        self.backend
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| b.duration())
            .unwrap_or(0.0)
    }

    fn with_backend<T>(&self, f: impl FnOnce(&MpvBackend) -> Result<T, PlayerError>) -> Result<T, PlayerError> {
        match self.backend.lock().unwrap().as_ref() {
            Some(b) => f(b),
            None => Err(PlayerError::new("P401", "会话后端已释放")),
        }
    }

    /// 音视频控制方法的查看器守卫：查看器会话 → **P103**。
    fn ensure_player(&self, op: &str) -> Result<(), PlayerError> {
        if self.viewer.lock().unwrap().is_some() {
            Err(PlayerError::new(
                "P103",
                format!("图片/PDF 会话不支持{op}"),
            ))
        } else {
            Ok(())
        }
    }

    /// `goto_page`/`frame` 守卫：非查看器会话 → **P103**。
    fn ensure_viewer(&self, op: &str) -> Result<(), PlayerError> {
        if self.viewer.lock().unwrap().is_none() {
            Err(PlayerError::new(
                "P103",
                format!("{op} 仅图片/PDF 查看会话可用"),
            ))
        } else {
            Ok(())
        }
    }

    pub fn pause(&self) -> Result<(), PlayerError> {
        self.ensure_player("暂停")?;
        let mut phase = self.phase.lock().unwrap();
        if !matches!(*phase, Phase::Playing | Phase::Paused) {
            return Err(PlayerError::new(
                "P401",
                format!("会话 {} 已结束，不能暂停", self.id),
            ));
        }
        self.with_backend(|b| b.set_pause(true))?;
        *phase = Phase::Paused;
        Ok(())
    }

    pub fn resume(&self) -> Result<(), PlayerError> {
        self.ensure_player("恢复播放")?;
        let mut phase = self.phase.lock().unwrap();
        if *phase != Phase::Paused {
            return Err(PlayerError::new(
                "P401",
                format!("会话 {} 不在暂停态", self.id),
            ));
        }
        self.with_backend(|b| b.set_pause(false))?;
        *phase = Phase::Playing;
        Ok(())
    }

    pub fn seek(&self, position_sec: f64) -> Result<(), PlayerError> {
        self.ensure_player("进度跳转")?;
        let phase = *self.phase.lock().unwrap();
        if matches!(phase, Phase::Stopped) {
            return Err(PlayerError::new("P401", "会话已停止"));
        }
        self.with_backend(|b| b.seek(position_sec))
    }

    pub fn set_volume(&self, volume: f64) -> Result<(), PlayerError> {
        self.ensure_player("音量调节")?;
        *self.volume.lock().unwrap() = volume.clamp(0.0, 1.0);
        self.with_backend(|b| b.set_volume(volume))
    }

    /// 选择/关闭字幕轨（`track`：1-based sid，-1=关；mpv 会话）。
    pub fn set_subtitle(&self, track: i32) -> Result<(), PlayerError> {
        self.ensure_player("字幕轨选择")?;
        self.with_backend(|b| b.set_subtitle_track(track))
    }

    /// 当前字幕轨（mpv 会话；无/关 → -1）。
    pub fn subtitle_track(&self) -> i32 {
        self.with_backend(|b| Ok(b.subtitle_track())).unwrap_or(-1)
    }

    pub fn set_tracks(&self, tracks: Option<Tracks>) {
        *self.tracks.lock().unwrap() = tracks;
    }

    /// 当前查看器帧（P1：`player.frame` 数据源；mpv 会话 → **P103**）。
    pub fn frame(&self) -> Result<ImageFrame, PlayerError> {
        self.ensure_viewer("player.frame")?;
        Ok(self.viewer.lock().unwrap().as_ref().expect("守卫已过").frame().clone())
    }

    /// PDF 翻页（1-based；图片 → **P103**，越界 → **P100**，渲染失败 → **P104**）。
    pub fn goto_page(&self, page: u32) -> Result<(), PlayerError> {
        let mut viewer = self.viewer.lock().unwrap();
        match viewer.as_mut() {
            None => Err(PlayerError::new(
                "P103",
                "goto_page 仅图片/PDF 查看会话可用",
            )),
            Some(ViewerState::Image { .. }) => Err(PlayerError::new(
                "P103",
                "图片会话无页码（goto_page 仅 PDF）",
            )),
            Some(ViewerState::Pdf {
                bytes,
                page: cur,
                pages,
                frame,
            }) => {
                if page < 1 || page > *pages {
                    return Err(PlayerError::new(
                        "P100",
                        format!("page {page} 超出范围 1..={pages}"),
                    ));
                }
                let rendered = crate::pdf_cap::render_page(bytes, page)?;
                *frame = rendered;
                *cur = page;
                Ok(())
            }
        }
    }

    /// 用户主动停止（幂等）；查看器无后端可停，仅迁相。
    pub fn stop(&self) {
        {
            let mut phase = self.phase.lock().unwrap();
            if *phase == Phase::Stopped {
                return;
            }
            *phase = Phase::Stopped;
        }
        if let Some(b) = self.backend.lock().unwrap().as_ref() {
            b.stop_playback();
        }
    }

    /// 后端事件 → phase 迁移（Finished）。返回 Some 表示已收束。
    pub fn on_backend_event(&self, ev: &BackendEvent) -> Option<Phase> {
        let mut phase = self.phase.lock().unwrap();
        match ev {
            BackendEvent::Paused { .. } => {
                if *phase == Phase::Playing {
                    *phase = Phase::Paused;
                }
                None
            }
            BackendEvent::Finished => {
                if matches!(*phase, Phase::Playing | Phase::Paused) {
                    *phase = Phase::Finished;
                    Some(Phase::Finished)
                } else {
                    None
                }
            }
            BackendEvent::Failed { .. } => {
                if matches!(*phase, Phase::Playing | Phase::Paused) {
                    *phase = Phase::Finished;
                    Some(Phase::Finished)
                } else {
                    None
                }
            }
            BackendEvent::Progress { .. } => None,
        }
    }

    pub fn status(&self) -> serde_json::Value {
        let phase = self.phase();
        let viewer = self.viewer.lock().unwrap();
        let mut v = serde_json::json!({
            "session_id": self.id,
            "placement_id": self.placement_id,
            "content_id": self.content_id,
            "asset_type": self.asset_type,
            "kind": if viewer.is_some() { "viewer" } else { "player" },
            "phase": match phase {
                Phase::Playing => "playing",
                Phase::Paused => "paused",
                Phase::Finished => "finished",
                Phase::Stopped => "stopped",
            },
            "playing": phase == Phase::Playing,
            "position_sec": self.position(),
            "duration_sec": self.duration(),
            "volume": *self.volume.lock().unwrap(),
        });
        match viewer.as_ref() {
            Some(ViewerState::Image { frame }) => {
                v["width"] = serde_json::json!(frame.width);
                v["height"] = serde_json::json!(frame.height);
            }
            Some(ViewerState::Pdf { page, pages, frame, .. }) => {
                v["page"] = serde_json::json!(page);
                v["pages"] = serde_json::json!(pages);
                v["width"] = serde_json::json!(frame.width);
                v["height"] = serde_json::json!(frame.height);
            }
            None => {
                let tracks = self.tracks.lock().unwrap();
                match tracks.as_ref() {
                    Some(t) => {
                        let st = t.status_field();
                        v["audio_tracks"] = st["audio_tracks"].clone();
                        v["subtitle_tracks"] = st["subtitle_tracks"].clone();
                    }
                    None => {
                        v["audio_tracks"] = serde_json::json!([]);
                        v["subtitle_tracks"] = serde_json::json!([]);
                    }
                }
                v["subtitle_track"] = serde_json::json!(self.subtitle_track());
            }
        }
        v
    }

    /// 卸载后端（stop/清理）。
    pub fn teardown(&self) {
        if let Some(mut b) = self.backend.lock().unwrap().take() {
            b.stop_playback();
            b.shutdown();
        }
    }
}

/// 会话表：`session_id → Session`。
#[derive(Default)]
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn fresh_id() -> String {
        let mut raw = [0u8; 16];
        let _ = getrandom::getrandom(&mut raw);
        format!(
            "ps_{}",
            raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    }

    /// mpv 播放会话（P0 语义 + asset_type）。
    pub fn insert_player(
        &self,
        placement_id: i64,
        content_id: Option<String>,
        url: String,
        asset_type: String,
        backend: MpvBackend,
    ) -> Arc<Session> {
        self.insert(Session::new_player(
            Self::fresh_id(),
            placement_id,
            content_id,
            url,
            asset_type,
            backend,
        ))
    }

    /// 查看器会话（P1）。
    pub fn insert_viewer(
        &self,
        placement_id: i64,
        content_id: Option<String>,
        url: String,
        asset_type: String,
        viewer: ViewerState,
    ) -> Arc<Session> {
        self.insert(Session::new_viewer(
            Self::fresh_id(),
            placement_id,
            content_id,
            url,
            asset_type,
            viewer,
        ))
    }

    fn insert(&self, session: Session) -> Arc<Session> {
        let session = Arc::new(session);
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), Arc::clone(&session));
        session
    }

    pub fn get(&self, id: &str) -> Result<Arc<Session>, PlayerError> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| PlayerError::session_not_found(id))
    }

    pub fn list(&self) -> Vec<Arc<Session>> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }

    /// 移除并卸载后端（P0：不持久化；P6 走 player.db）。
    pub fn remove(&self, id: &str) -> Option<Arc<Session>> {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(s) = session.as_ref() {
            s.teardown();
        }
        session
    }

    /// 全部收束（deactivate）。
    pub fn clear(&self) {
        let sessions: Vec<_> = {
            let mut map = self.sessions.lock().unwrap();
            map.drain().map(|(_, s)| s).collect()
        };
        for s in sessions {
            s.stop();
            s.teardown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_cap::ImageFrame;
    use image::ImageEncoder;

    /// 无内容播放的裸 mpv 会话（vo=null 真后端，phase 可控）。
    fn bare(phase: Phase) -> Session {
        let s = Session::new_player(
            "ps_t".into(),
            1,
            None,
            String::new(),
            "video".into(),
            MpvBackend::new_headless_for_test(),
        );
        *s.phase.lock().unwrap() = phase;
        s
    }

    #[test]
    fn t1_unknown_session_p401() {
        let mgr = SessionManager::new();
        let err = mgr.get("ps_nope").unwrap_err();
        assert_eq!(err.code, "P401");
    }

    #[test]
    fn t2_phase_transitions_guards() {
        // 无害场景：mpv 会话 phase 门禁（vo=null 后端，不播放任何内容）
        let s = bare(Phase::Finished);
        let err = s.pause().unwrap_err();
        assert_eq!(err.code, "P401");
        let err = s.resume().unwrap_err();
        assert_eq!(err.code, "P401");
        s.stop();
        let err = s.seek(1.0).unwrap_err();
        assert_eq!(err.code, "P401");
        s.teardown();
    }

    #[test]
    fn t3_finished_event_from_playing() {
        let s = bare(Phase::Playing);
        let migrated = s.on_backend_event(&BackendEvent::Finished);
        assert_eq!(migrated, Some(Phase::Finished));
        assert_eq!(s.phase(), Phase::Finished);
        let again = s.on_backend_event(&BackendEvent::Finished);
        assert_eq!(again, None);
        s.teardown();
    }

    #[test]
    fn t4_viewer_session_ops_p103() {
        let png = {
            let img = image::RgbaImage::from_pixel(64, 48, image::Rgba([10, 20, 30, 255]));
            let mut buf = Vec::new();
            image::codecs::png::PngEncoder::new(&mut buf)
                .write_image(&img.into_raw(), 64, 48, image::ExtendedColorType::Rgba8)
                .unwrap();
            buf
        };
        let frame = ImageFrame::from_bytes(&png).unwrap();
        let s = Session::new_viewer(
            "ps_v".into(),
            1,
            Some("c1".into()),
            "http://127.0.0.1:7913/s/x".into(),
            "image".into(),
            ViewerState::Image { frame },
        );
        // 音视频控制 → P103（优先于 phase 门禁）
        assert_eq!(s.pause().unwrap_err().code, "P103");
        assert_eq!(s.resume().unwrap_err().code, "P103");
        assert_eq!(s.seek(1.0).unwrap_err().code, "P103");
        assert_eq!(s.set_volume(0.5).unwrap_err().code, "P103");
        assert_eq!(s.set_subtitle(1).unwrap_err().code, "P103");
        // goto_page 在图片会话 → P103；frame 可用
        assert_eq!(s.goto_page(1).unwrap_err().code, "P103");
        let f = s.frame().unwrap();
        assert_eq!((f.width, f.height), (64, 48));
        // status 扩展
        let v = s.status();
        assert_eq!(v["asset_type"], "image");
        assert_eq!(v["kind"], "viewer");
        assert_eq!(v["width"], 64);
        assert_eq!(v["height"], 48);
        // stop 仍可用
        s.stop();
        assert_eq!(s.phase(), Phase::Stopped);
    }

    #[test]
    fn t5_frame_on_player_session_p103() {
        let s = bare(Phase::Playing);
        let err = s.frame().unwrap_err();
        assert_eq!(err.code, "P103");
        let err = s.goto_page(1).unwrap_err();
        assert_eq!(err.code, "P103");
        s.teardown();
    }

    #[test]
    fn t6_pdf_goto_page_bounds_and_render() {
        let pdf = crate::pdf_cap::fixture_pdf(&["PAGE-ONE", "PAGE-TWO"]);
        let f1 = crate::pdf_cap::render_page(&pdf, 1).unwrap();
        let s = Session::new_viewer(
            "ps_p".into(),
            1,
            None,
            String::new(),
            "doc".into(),
            ViewerState::Pdf {
                bytes: Arc::new(pdf),
                page: 1,
                pages: 2,
                frame: f1,
            },
        );
        let err = s.goto_page(0).unwrap_err();
        assert_eq!(err.code, "P100");
        let err = s.goto_page(3).unwrap_err();
        assert_eq!(err.code, "P100");
        let h1 = s.frame().unwrap().checksum();
        s.goto_page(2).unwrap();
        assert_eq!(s.status()["page"], 2);
        let h2 = s.frame().unwrap().checksum();
        assert_ne!(h1, h2, "翻页后帧内容应不同");
        s.goto_page(1).unwrap();
        assert_eq!(s.status()["page"], 1);
        assert_eq!(h1, s.frame().unwrap().checksum(), "回第 1 页复原");
    }
}
