//! 压缩（`docs/01` §7.7、`docs/03` §2.3 `compress.*`）。
//!
//! 判定阈值：先压前 1 MB，压缩后 **< 原体积 95%** 才算值得。

use std::io::{Read, Write};
use std::path::Path;

use rm_proto::BizError;

use crate::codes;

/// 试压前缀长度（1 MB）。
pub const SAMPLE_LEN: u64 = 1_048_576;

/// zstd 压缩级别（默认 3，速度/比折中）。
pub const ZSTD_LEVEL: i32 = 3;

/// 试压报告。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SampleRow {
    pub blake3: String,
    pub sample_in: u64,
    pub sample_out: u64,
    /// 压缩后 < 原体积 95%。
    pub worth: bool,
    pub ratio: f64,
}

/// 对文件前 `SAMPLE_LEN` 字节试压。文件不存在/不可读返回 F302。
pub fn sample_file(path: &Path) -> Result<SampleRow, BizError> {
    let mut f = std::fs::File::open(path).map_err(|e| {
        BizError::new(codes::F302, format!("试压打开失败：{}：{e}", path.display()))
    })?;
    let mut buf = Vec::with_capacity(SAMPLE_LEN as usize);
    std::io::Read::by_ref(&mut f)
        .take(SAMPLE_LEN)
        .read_to_end(&mut buf)
        .map_err(|e| {
            BizError::new(codes::F302, format!("试压读取失败：{}：{e}", path.display()))
        })?;
    let sample_in = buf.len() as u64;
    if sample_in == 0 {
        return Ok(SampleRow {
            blake3: String::new(),
            sample_in: 0,
            sample_out: 0,
            worth: false,
            ratio: 1.0,
        });
    }
    let compressed = zstd::encode_all(&buf[..], ZSTD_LEVEL).map_err(|e| {
        BizError::new(codes::F302, format!("试压压缩失败：{}：{e}", path.display()))
    })?;
    let sample_out = compressed.len() as u64;
    let worth = (sample_out as f64) < (sample_in as f64) * 0.95;
    let ratio = if sample_in > 0 {
        sample_out as f64 / sample_in as f64
    } else {
        1.0
    };
    Ok(SampleRow {
        blake3: String::new(),
        sample_in,
        sample_out,
        worth,
        ratio,
    })
}

/// 整文件 zstd 压缩到 `out`。返回压缩后字节数。
pub fn compress_file(src: &Path, out: &Path) -> Result<i64, BizError> {
    let mut input = std::fs::File::open(src).map_err(|e| {
        BizError::new(codes::F302, format!("压缩打开失败：{}：{e}", src.display()))
    })?;
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut output = std::fs::File::create(out).map_err(|e| {
        BizError::new(
            codes::F302,
            format!("压缩创建失败：{}：{e}", out.display()),
        )
    })?;
    zstd::stream::copy_encode(&mut input, &mut output, ZSTD_LEVEL).map_err(|e| {
        BizError::new(codes::F302, format!("压缩失败：{}：{e}", src.display()))
    })?;
    let n = output.metadata().map(|m| m.len() as i64).unwrap_or(0);
    Ok(n)
}

/// 整文件解压到 `out`（自然结束，无需原始长度）。返回解压后字节数。
pub fn decompress_file(src: &Path, out: &Path) -> Result<i64, BizError> {
    let mut input = std::fs::File::open(src).map_err(|e| {
        BizError::new(
            codes::F302,
            format!("解压打开失败：{}：{e}", src.display()),
        )
    })?;
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut output = std::fs::File::create(out).map_err(|e| {
        BizError::new(
            codes::F302,
            format!("解压创建失败：{}：{e}", out.display()),
        )
    })?;
    let mut decoder = zstd::Decoder::new(&mut input)
        .map_err(|e| BizError::new(codes::F302, format!("解压器失败：{e}")))?;
    let n = std::io::copy(&mut decoder, &mut output)
        .map_err(|e| BizError::new(codes::F302, format!("解压失败：{e}")))?;
    Ok(n as i64)
}

/// 压缩字节切片。
pub fn compress_bytes(data: &[u8]) -> Result<Vec<u8>, BizError> {
    zstd::encode_all(data, ZSTD_LEVEL)
        .map_err(|e| BizError::new(codes::F302, format!("压缩失败：{e}")))
}

/// 解压字节切片到自然结束。
pub fn decompress_bytes(data: &[u8]) -> Result<Vec<u8>, BizError> {
    zstd::decode_all(data)
        .map_err(|e| BizError::new(codes::F302, format!("解压失败：{e}")))
}

/// 用临时文件原子替换：先写 `path.part`，fsync 后 rename。
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), BizError> {
    let part = path.with_extension("part");
    {
        let mut f = std::fs::File::create(&part).map_err(|e| {
            BizError::new(codes::F302, format!("写入失败：{}：{e}", part.display()))
        })?;
        f.write_all(data).map_err(|e| {
            BizError::new(codes::F302, format!("写入失败：{}：{e}", part.display()))
        })?;
        f.sync_all().ok();
    }
    std::fs::rename(&part, path).map_err(|e| {
        BizError::new(
            codes::F302,
            format!("替换失败：{}：{e}", path.display()),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sample_and_ratio() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.bin");
        // 高度可压：1MB 零字节。
        let data = vec![0u8; 2 * SAMPLE_LEN as usize];
        std::fs::write(&p, &data).unwrap();
        let r = sample_file(&p).unwrap();
        assert!(r.worth, "zeros should compress: ratio={}", r.ratio);
        assert!(r.sample_out < r.sample_in * 95 / 100);
    }

    #[test]
    fn roundtrip_bytes() {
        let src = b"hello world hello world hello world".repeat(100);
        let c = compress_bytes(&src).unwrap();
        let d = decompress_bytes(&c).unwrap();
        assert_eq!(src, d.as_slice());
    }

    #[test]
    fn roundtrip_file() {
        let d = tempdir().unwrap();
        let src = d.path().join("in.bin");
        let mid = d.path().join("mid.zst");
        let out = d.path().join("out.bin");
        let data: Vec<u8> = (0..500_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        compress_file(&src, &mid).unwrap();
        decompress_file(&mid, &out).unwrap();
        assert_eq!(data, std::fs::read(&out).unwrap());
    }
}
