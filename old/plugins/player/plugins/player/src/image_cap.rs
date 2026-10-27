//! 图片解码与帧封装（`docs/01` §4.2 image → 图片渲染通道）。
//!
//! 设计未钦定图片库 → 选 `image` crate（纯 Rust，MPL-2.0，无链接传染）；
//! 显示通道走 `player.frame` 方法输出 PNG base64（首版统一帧通道，附录 B 登记）。
//! 传输前长边 > `MAX_FRAME_EDGE` 等比缩小，防大图 base64 撑爆 UI。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use image::imageops::FilterType;
use image::{ImageEncoder, RgbaImage};

use crate::error::PlayerError;

/// `player.frame` 单帧长边上限（附录 B）。
pub const MAX_FRAME_EDGE: u32 = 2048;

/// 解码后的一帧 RGBA。
#[derive(Clone)]
pub struct ImageFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 只打印尺寸（rgba 可达数 MB，不入 Debug 输出）。
impl std::fmt::Debug for ImageFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_len", &self.rgba.len())
            .finish()
    }
}

impl ImageFrame {
    /// 从原始图片字节解码（format 探测自动）；失败 → **P104**。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PlayerError> {
        let img = image::load_from_memory(bytes)
            .map_err(|e| PlayerError::new("P104", format!("图片解码失败：{e}")))?;
        Ok(Self::from_rgba(img.to_rgba8()))
    }

    pub fn from_rgba(img: RgbaImage) -> Self {
        Self {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
        }
    }

    /// 长边超过 `max_edge` 时等比缩小（否则原样）。
    pub fn scaled(&self, max_edge: u32) -> Self {
        let long = self.width.max(self.height);
        if long <= max_edge || long == 0 {
            return self.clone();
        }
        let ratio = max_edge as f32 / long as f32;
        let w = ((self.width as f32 * ratio).round() as u32).max(1);
        let h = ((self.height as f32 * ratio).round() as u32).max(1);
        let src = RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .expect("rgba 长度与尺寸一致（构造方不变量）");
        let dst = image::imageops::resize(&src, w, h, FilterType::Triangle);
        Self::from_rgba(dst)
    }

    /// 编码 PNG → base64（按 `MAX_FRAME_EDGE` 缩边）。
    pub fn png_base64(&self) -> Result<(String, u32, u32), PlayerError> {
        let f = self.scaled(MAX_FRAME_EDGE);
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&f.rgba, f.width, f.height, image::ExtendedColorType::Rgba8)
            .map_err(|e| PlayerError::new("P104", format!("PNG 编码失败：{e}")))?;
        Ok((B64.encode(&png), f.width, f.height))
    }

    /// 内容校验和（翻页/渲染差异断言用；非密码学）。
    pub fn checksum(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in self.rgba.iter().chain(self.width.to_le_bytes().iter()).chain(self.height.to_le_bytes().iter()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(&img.into_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        buf
    }

    #[test]
    fn t1_decode_roundtrip() {
        let png = solid_png(320, 240, [255, 0, 0, 255]);
        let f = ImageFrame::from_bytes(&png).unwrap();
        assert_eq!((f.width, f.height), (320, 240));
        assert_eq!(&f.rgba[0..4], &[255, 0, 0, 255]);
        let (b64, w, h) = f.png_base64().unwrap();
        assert_eq!((w, h), (320, 240));
        let bytes = B64.decode(&b64).unwrap();
        let back = ImageFrame::from_bytes(&bytes).unwrap();
        assert_eq!(back.rgba, f.rgba);
    }

    #[test]
    fn t2_scale_down_big() {
        let f = ImageFrame {
            width: 4096,
            height: 1024,
            rgba: vec![7u8; 4096 * 1024 * 4],
        };
        let s = f.scaled(MAX_FRAME_EDGE);
        assert_eq!(s.width, MAX_FRAME_EDGE);
        assert_eq!(s.height, 1024 * MAX_FRAME_EDGE / 4096);
        // 不超限不缩
        let small = f.scaled(4096);
        assert_eq!((small.width, small.height), (4096, 1024));
    }

    #[test]
    fn t3_checksum_sensitive() {
        let a = ImageFrame { width: 4, height: 4, rgba: vec![1u8; 64] };
        let b = ImageFrame { width: 4, height: 4, rgba: { let mut v = vec![1u8; 64]; v[0] = 2; v } };
        assert_ne!(a.checksum(), b.checksum());
        assert_eq!(a.checksum(), a.clone().checksum());
    }

    #[test]
    fn t4_bad_bytes_p104() {
        let err = ImageFrame::from_bytes(b"not an image").unwrap_err();
        assert_eq!(err.code, "P104");
    }
}
