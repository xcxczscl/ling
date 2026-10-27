//! PDF 能力：页数统计与渲染（`docs/01` §4.2 doc → PDF 渲染通道）。
//!
//! 选型（附录 B 登记）：子进程调 poppler-utils（`pdfinfo`/`pdftoppm`），
//! 不链接 mupdf（AGPL）/poppler（GPL）库——进程隔离 + 系统工具调用许可干净；
//! 相应 plugin.toml 声明 `shell` 能力。运行环境需已安装 poppler-utils。
//! pdftoppm 无 stdin 输入 → 源字节落临时文件（写完即删）。

use std::io::Write as _;
use std::time::Duration;

use crate::error::PlayerError;
use crate::image_cap::ImageFrame;
use crate::shcmd;

/// 默认渲染分辨率（dpi）：≈1.33×，A4 ≈ 793×1122 px。
pub const PDF_DPI: u32 = 96;

const INFO_TIMEOUT: Duration = Duration::from_secs(5);
const RENDER_TIMEOUT: Duration = Duration::from_secs(20);

fn ensure_pdf(bytes: &[u8]) -> Result<(), PlayerError> {
    if crate::asset::is_pdf(bytes) {
        Ok(())
    } else {
        Err(PlayerError::new(
            "P103",
            "内容不是 PDF（doc 首版仅支持 PDF）",
        ))
    }
}

/// 缺工具 → P104，附安装提示。
fn tool_err(tool: &str, detail: String) -> PlayerError {
    if detail.contains("spawn") {
        PlayerError::new(
            "P104",
            format!("缺少 {tool}（请安装 poppler-utils）：{detail}"),
        )
    } else {
        PlayerError::new("P104", format!("{tool} 失败：{detail}"))
    }
}

/// 页数（pdfinfo）。
pub fn count_pages(pdf: &[u8]) -> Result<u32, PlayerError> {
    ensure_pdf(pdf)?;
    let tmp = tempfile::NamedTempFile::new()
        .map_err(|e| PlayerError::new("P104", format!("临时文件失败：{e}")))?;
    tmp.as_file()
        .write_all(pdf)
        .map_err(|e| PlayerError::new("P104", format!("临时文件写入失败：{e}")))?;
    let path = tmp.path().to_str().ok_or_else(|| {
        PlayerError::new("P104", "临时文件路径非 UTF-8".to_string())
    })?;

    let out = shcmd::run_checked(
        &["pdfinfo", path],
        INFO_TIMEOUT,
    )
    .map_err(|e| tool_err("pdfinfo", e))?;
    out.lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| PlayerError::new("P104", "pdfinfo 未给出有效页数".to_string()))
}

/// 渲染第 `page` 页（1-based）为 RGBA 帧。
pub fn render_page(pdf: &[u8], page: u32) -> Result<ImageFrame, PlayerError> {
    ensure_pdf(pdf)?;
    if page == 0 {
        return Err(PlayerError::new("P100", "page 从 1 开始"));
    }
    let dir = tempfile::tempdir()
        .map_err(|e| PlayerError::new("P104", format!("临时目录失败：{e}")))?;
    let src = dir.path().join("in.pdf");
    std::fs::write(&src, pdf)
        .map_err(|e| PlayerError::new("P104", format!("临时文件写入失败：{e}")))?;
    let prefix = dir.path().join("out");
    let prefix_s = prefix.to_str().ok_or_else(|| {
        PlayerError::new("P104", "临时路径非 UTF-8".to_string())
    })?;

    shcmd::run_checked(
        &[
            "pdftoppm",
            "-png",
            "-singlefile",
            "-r",
            &PDF_DPI.to_string(),
            "-f",
            &page.to_string(),
            src.to_str().unwrap(),
            prefix_s,
        ],
        RENDER_TIMEOUT,
    )
    .map_err(|e| tool_err("pdftoppm", e))?;

    let png_path = dir.path().join("out.png");
    let png = std::fs::read(&png_path)
        .map_err(|e| PlayerError::new("P104", format!("渲染输出缺失（页码越界？）：{e}")))?;
    ImageFrame::from_bytes(&png)
}

/// 测试/夹具 PDF：`pages` 每项为一页可见文本；页间背景色不同。
/// 手写 xref（各 obj 字节偏移自算），unit 与 e2e 共用（rlib 链接）。
pub fn fixture_pdf(pages: &[&str]) -> Vec<u8> {
    let defaults = ["PAGE1"];
    let pages: Vec<&str> = if pages.is_empty() {
        defaults.to_vec()
    } else {
        pages.to_vec()
    };
    let n = pages.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + i * 2)).collect();

    let mut objs: Vec<Vec<u8>> = Vec::new();
    objs.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objs.push(
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes(),
    );
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for (i, text) in pages.iter().enumerate() {
        // 页背景色随页码变化 → 渲染像素必不同
        let colors = ["1 0 0", "0 1 0", "0 0 1", "1 1 0"];
        let c = colors[i % colors.len()];
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 3 0 R >> >> \
                 /Contents {} 0 R >>",
                4 + i * 2 + 1
            )
            .into_bytes(),
        );
        let esc = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let body = format!(
            "{c} rg 40 40 532 712 re f BT /F1 32 Tf 72 400 Td ({esc}) Tj ET"
        );
        objs.push(
            format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len())
                .into_bytes(),
        );
    }

    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1)
            .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境前置：poppler-utils（pdfinfo/pdftoppm）。缺则测试失败——
    /// 这是 P1 的运行依赖（README 环境段落已注明）。
    #[test]
    fn t1_fixture_count_and_render() {
        let pdf = fixture_pdf(&["HELLO-ONE", "HELLO-TWO"]);
        assert!(crate::asset::is_pdf(&pdf));
        let pages = count_pages(&pdf).unwrap();
        assert_eq!(pages, 2);

        let p1 = render_page(&pdf, 1).unwrap();
        let p2 = render_page(&pdf, 2).unwrap();
        assert_eq!((p1.width, p1.height), (p2.width, p2.height));
        assert!(p1.width > 600, "≈96dpi 下 letter 宽度应 >600：{}", p1.width);
        assert_ne!(p1.checksum(), p2.checksum(), "两页背景色不同 → 像素不同");
        let (b64, w, h) = p1.png_base64().unwrap();
        assert!(!b64.is_empty() && w == p1.width && h == p1.height);
    }

    #[test]
    fn t2_non_pdf_p103_and_bad_pdf_p104() {
        let err = count_pages(b"just text").unwrap_err();
        assert_eq!(err.code, "P103");
        // 含魔数但结构损坏 → pdfinfo 报错 → P104
        let err = render_page(b"%PDF-1.4\ngarbage garbage", 1).unwrap_err();
        assert_eq!(err.code, "P104");
    }

    #[test]
    fn t3_page_zero_p100() {
        let err = render_page(&fixture_pdf(&["A"]), 0).unwrap_err();
        assert_eq!(err.code, "P100");
    }
}
