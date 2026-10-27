//! 资产类型解析与 P1 分流（`docs/01` §4.2 能力路由表）。
//!
//! 浏览行（rm `browse.query`）自带 `asset_type`，本模块只做：
//! 类型 → 会话形态 的归类，以及首版不支持类型的拒绝（**P103**）。

use crate::error::PlayerError;

/// 会话形态（P1：mpv 播放 / 查看器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// video/audio → libmpv（backend.rs）。
    Player,
    /// image → 查看器（渲染为帧）。
    ViewerImage,
    /// doc（首版仅 .pdf）→ 查看器（PDF 翻页）。
    ViewerPdf,
}

/// 按 `asset_type` + 相对路径归类；不支持 → **P103**。
/// `asset_type` 先小写化（rm 出参为小写，防御式兼容）。
pub fn route(asset_type: &str, rel_path: &str) -> Result<Route, PlayerError> {
    match asset_type.to_ascii_lowercase().as_str() {
        "video" | "audio" => Ok(Route::Player),
        "image" => Ok(Route::ViewerImage),
        "doc" => {
            if rel_path.to_ascii_lowercase().ends_with(".pdf") {
                Ok(Route::ViewerPdf)
            } else {
                Err(PlayerError::new(
                    "P103",
                    "doc 首版仅支持 PDF，其他文档格式待后续里程碑",
                ))
            }
        }
        "app" | "game" => Err(PlayerError::new(
            "P103",
            "应用/游戏运行自 P5（沙箱）起支持",
        )),
        other => Err(PlayerError::new(
            "P103",
            format!("不支持的资产类型：{other}"),
        )),
    }
}

/// PDF 魔数校验（前 1024 字节含 `%PDF-`）。
pub fn is_pdf(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    head.windows(5).any(|w| w == b"%PDF-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_route_by_type() {
        assert_eq!(route("video", "a.mkv").unwrap(), Route::Player);
        assert_eq!(route("audio", "a.mp3").unwrap(), Route::Player);
        assert_eq!(route("image", "a.png").unwrap(), Route::ViewerImage);
        assert_eq!(route("doc", "a.pdf").unwrap(), Route::ViewerPdf);
        assert_eq!(route("DOC", "A.PDF").unwrap(), Route::ViewerPdf);
    }

    #[test]
    fn t2_unsupported_p103() {
        for (t, p) in [
            ("doc", "note.txt"),
            ("app", "setup.exe"),
            ("game", "rom.iso"),
            ("other", "weird.xyz"),
            ("archive", "a.zip"),
        ] {
            let err = route(t, p).unwrap_err();
            assert_eq!(err.code, "P103", "{t} {} {}", err.code, err.message);
        }
    }

    #[test]
    fn t3_is_pdf_magic() {
        assert!(is_pdf(b"%PDF-1.7\n..."));
        assert!(!is_pdf(b"PK\x03\x04zip"));
        assert!(!is_pdf(b""));
        // 1024 窗口外不算
        let mut far = vec![0u8; 2048];
        far[1500..1505].copy_from_slice(b"%PDF-");
        assert!(!is_pdf(&far));
    }
}
