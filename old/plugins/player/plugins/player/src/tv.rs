//! P4 电视协议 Sink 接口占位（`docs/01` §5.4；附录 B #51）：
//! `TvProtocolSink` trait 照设计落签名，首版只给 Null 实现——`discover` 返回空列表，
//! `cast` 报 **P201**（无电视协议目标可达）。Chromecast/AirPlay/Miracast 留后续。
//!
//! 注：§6.3 Sink trait 的帧写入面（`write_video_frame`/`write_audio_sample`）本里程碑
//! 不落——A=代理转发、B/C=ffmpeg 管道，均无逐帧消费者，落类型面即死代码（#40 立场），
//! 延至 P7 画面回传（附录 B #52 登记）。

use crate::error::PlayerError;

/// 系统级投屏设备（§5.4）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TvDevice {
    pub id: String,
    pub name: String,
    /// 协议族（chromecast/airplay/miracast；首版无设备 → 不出现）。
    pub protocol: String,
}

/// 电视协议 Sink（§5.4 签名照表）。
pub trait TvProtocolSink {
    /// 发现系统级投屏设备（首版恒空）。
    fn discover(&self) -> Vec<TvDevice>;
    /// 推流给系统接收器。
    fn cast(&self, url: &str) -> Result<(), PlayerError>;
}

/// 首版占位实现：空列表 + 拒绝（§5.4「首版返回空列表」）。
pub struct NullTvSink;

impl TvProtocolSink for NullTvSink {
    fn discover(&self) -> Vec<TvDevice> {
        Vec::new()
    }

    fn cast(&self, _url: &str) -> Result<(), PlayerError> {
        Err(PlayerError::target_unreachable(
            "电视协议 Sink 未实现（P4 仅接口占位，§5.4）",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_null_sink_discover_empty_cast_p201() {
        let sink = NullTvSink;
        assert!(sink.discover().is_empty(), "首版返回空列表（§5.4）");
        let e = sink.cast("rtsp://tv/stream").unwrap_err();
        assert_eq!(e.code, "P201");
        assert!(e.message.contains("占位"), "{}", e.message);
    }
}
