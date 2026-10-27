//! `rm-proto`：所有对外消息结构的唯一定义处（`docs/04` §2）。
//!
//! 领域包与插件壳、命令行共享这里的类型；前端类型由这些结构生成。

use serde::{Deserialize, Serialize};

pub mod event;
pub mod msg;

/// 业务错误码 F 族（`docs/03` §2.7）。
pub mod codes {
    pub const F101: &str = "F101";
    pub const F301: &str = "F301";
    pub const F302: &str = "F302";
    pub const F303: &str = "F303";
    pub const F401: &str = "F401";
    pub const F402: &str = "F402";
    pub const F403: &str = "F403";
    pub const F404: &str = "F404";
    pub const F405: &str = "F405";
    pub const F501: &str = "F501";
    pub const F502: &str = "F502";
    pub const F503: &str = "F503";
    pub const F504: &str = "F504";
    pub const F601: &str = "F601";
    pub const F602: &str = "F602";
    pub const F603: &str = "F603";
    pub const F604: &str = "F604";
    pub const F605: &str = "F605";
    pub const F606: &str = "F606";
    pub const F701: &str = "F701";
    pub const F702: &str = "F702";
    pub const F703: &str = "F703";
    pub const F704: &str = "F704";
    pub const F705: &str = "F705";
    pub const F706: &str = "F706";
    pub const F801: &str = "F801";
    pub const F802: &str = "F802";
    pub const F803: &str = "F803";
    pub const F804: &str = "F804";
}

/// 业务错误：带 F 族错误码的结构化错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BizError {
    pub code: String,
    pub message: String,
}

impl BizError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// 安全删除被拦截（`docs/03` §2.5）：CLI 据此退出码 3。
    pub fn blocked(reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(codes::F303, format!("{}：{}", reason.into(), detail.into()))
    }
}

impl std::fmt::Display for BizError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BizError {}

pub type BizResult<T> = Result<T, BizError>;

/// 通用分页请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageReq {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

impl PageReq {
    /// 缺省每页 500 行（`docs/02` §8.8）；跳页偏移硬上限 10 万行（§8.5）。
    pub fn limit(&self) -> u32 {
        self.limit.unwrap_or(500).clamp(1, 5000)
    }

    pub fn offset(&self) -> u32 {
        self.offset.unwrap_or(0).min(100_000)
    }
}

/// 位置状态（`docs/02` §7.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationState {
    Online,
    Offline,
    Cold,
    Missing,
}

impl LocationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Cold => "cold",
            Self::Missing => "missing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "online" => Some(Self::Online),
            "offline" => Some(Self::Offline),
            "cold" => Some(Self::Cold),
            "missing" => Some(Self::Missing),
            _ => None,
        }
    }

    /// 离线/冷/丢失的副本永不作为删除候选（`docs/01` §7.3 检查 2）。
    pub fn is_available(self) -> bool {
        matches!(self, Self::Online)
    }
}

/// 放置模式（`docs/02` §2.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementMode {
    Scattered,
    Managed,
}

impl PlacementMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scattered => "scattered",
            Self::Managed => "managed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "scattered" => Some(Self::Scattered),
            "managed" => Some(Self::Managed),
            _ => None,
        }
    }
}

/// 资产类型（`docs/02` §2.3）。
pub const ASSET_TYPES: [&str; 8] = [
    "video", "audio", "image", "app", "game", "doc", "archive", "other",
];

/// 按扩展名推断资产类型（确定性提取，M1 规则表；`docs/01` §7.8 第一步）。
pub fn asset_type_for_ext(ext: &str) -> &'static str {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    match e.as_str() {
        "mkv" | "mp4" | "avi" | "mov" | "wmv" | "flv" | "webm" | "ts" | "m2ts" => "video",
        "mp3" | "flac" | "aac" | "ogg" | "wav" | "m4a" | "opus" | "wma" => "audio",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" | "avif" | "tif" | "tiff" => {
            "image"
        }
        "exe" | "msi" | "apk" | "deb" | "rpm" | "appimage" | "dmg" | "pkg" => "app",
        "iso" | "cue" | "gdi" | "nes" | "smc" | "gba" | "nds" | "pk3" | "wad" => "game",
        "pdf" | "epub" | "mobi" | "azw3" | "txt" | "md" | "doc" | "docx" | "odt" | "rtf" => "doc",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" => "archive",
        _ => "other",
    }
}
