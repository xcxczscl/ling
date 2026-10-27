//! 确定性提取（`docs/03` §5.4.3 第 1、4 步；纯代码，不花钱、可复现）。
//!
//! 第 2/3 步（ffprobe/exiftool）属宿主工具调用，M5 环境未装，留给后续；
//! 第 5 步目录规则由 `rm-classify` 的规则表承担；第 6 步才是 Provider。

/// 从文件名提取的确定性事实。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Extracted {
    /// 无点小写扩展名（如 `mkv`）。
    pub ext: String,
    /// 资产类型（`rm_proto::asset_type_for_ext`）。
    pub asset_type: String,
    /// 去掉扩展名后的文件名主体。
    pub stem: String,
    /// 年份（四位 19xx/20xx，取最靠前的一个）。
    pub year: Option<i32>,
    /// 剧集编号 SxxEyy。
    pub episode: Option<String>,
    /// 分辨率关键词：4k / 1080p / 720p / 2160p。
    pub resolution_tag: Option<String>,
    /// HDR 标记。
    pub hdr: bool,
    /// 音轨语言猜测（中字 / 中英 / JP / EN …）。
    pub language: Option<String>,
    /// 版本标记：PROPER / REPACK / REMUX / WEB-DL / BluRay。
    pub version_marker: Option<String>,
    /// 归一化标题（去噪点后的文件名主体，供刮削查询）。
    pub title_guess: String,
}

/// 从路径（含文件名）提取确定性事实。
pub fn extract(rel_path: &str) -> Extracted {
    let name = std::path::Path::new(rel_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.to_string());
    let ext = std::path::Path::new(&name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let asset_type = rm_proto::asset_type_for_ext(&ext).to_string();
    let stem = std::path::Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let year = find_year(&stem);
    let episode = find_episode(&stem);
    let resolution_tag = find_resolution(&stem);
    let hdr = stem.to_ascii_lowercase().contains("hdr")
        || stem.to_ascii_lowercase().contains("dv");
    let language = find_language(&stem);
    let version_marker = find_version_marker(&stem);
    let title_guess = clean_title(&stem);

    Extracted {
        ext,
        asset_type,
        stem,
        year,
        episode,
        resolution_tag,
        hdr,
        language,
        version_marker,
        title_guess,
    }
}

fn find_year(s: &str) -> Option<i32> {
    // 19xx / 20xx，前后是非数字。
    let bytes = s.as_bytes();
    for i in 0..s.len().saturating_sub(3) {
        if !bytes[i].is_ascii_digit() {
            continue;
        }
        if i > 0 && bytes[i - 1].is_ascii_digit() {
            continue;
        }
        let chunk = &s[i..i + 4];
        if chunk.len() == 4 && chunk.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(y) = chunk.parse::<i32>() {
                if (1900..=2099).contains(&y) {
                    return Some(y);
                }
            }
        }
    }
    None
}

fn find_episode(s: &str) -> Option<String> {
    let up = s.to_ascii_uppercase();
    // SxxEyy / SxxEyy-Zzz
    let bytes = up.as_bytes();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == b'S'
            && i + 5 <= bytes.len()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3] == b'E'
            && bytes[i + 4].is_ascii_digit()
        {
            // E 后面可以是一位或两位
            let end = if i + 6 <= bytes.len() && bytes[i + 5].is_ascii_digit() {
                i + 6
            } else {
                i + 5
            };
            return Some(up[i..end].to_string());
        }
    }
    None
}

fn find_resolution(s: &str) -> Option<String> {
    let low = s.to_ascii_lowercase();
    for pat in ["2160p", "4k", "1080p", "720p", "480p"] {
        if low.contains(pat) {
            return Some(pat.to_string());
        }
    }
    None
}

fn find_language(s: &str) -> Option<String> {
    let low = s.to_ascii_lowercase();
    // 常见中文字幕/双语标记
    for (pat, lang) in [
        ("中英", "zh-en"),
        ("中字", "zh"),
        ("双语", "dual"),
        ("国语", "mandarin"),
        ("粤语", "cantonese"),
        (".jp.", "ja"),
        (".jpn.", "ja"),
        (".en.", "en"),
        (".eng.", "en"),
    ] {
        if low.contains(pat) {
            return Some(lang.to_string());
        }
    }
    None
}

fn find_version_marker(s: &str) -> Option<String> {
    let up = s.to_ascii_uppercase();
    for pat in ["PROPER", "REPACK", "REMUX", "WEB-DL", "WEBDL", "BLURAY", "BLU-RAY", "HDTV"] {
        if up.contains(pat) {
            return Some(pat.to_string());
        }
    }
    None
}

/// 归一化标题：去掉分辨率/音轨/版本噪点，保留可读主体。
fn clean_title(stem: &str) -> String {
    let mut s = stem.to_string();
    // 去掉中括号/圆括号内容（常见噪点）
    while let Some(a) = s.find('[') {
        if let Some(b) = s.find(']') {
            if b > a {
                s.replace_range(a..=b, "");
                continue;
            }
        }
        break;
    }
    while let Some(a) = s.find('(') {
        if let Some(b) = s.find(')') {
            if b > a {
                s.replace_range(a..=b, "");
                continue;
            }
        }
        break;
    }
    // 去掉已知关键词（大小写不敏感）
    let low = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let tokens: Vec<&str> = low.split_whitespace().collect();
    const NOISE: &[&str] = &[
        "2160p", "1080p", "720p", "480p", "4k", "hdr", "dv", "hevc", "x264", "x265", "h264",
        "h265", "aac", "ac3", "dts", "truehd", "atmos", "blu-ray", "bluray", "web-dl", "webdl",
        "hdtv", "proper", "repack", "remux", "ddp5", "dd5", "dual", "中字", "中英", "国语",
        "粤语", "chs", "cht", "eng", "jpn",
    ];
    for t in tokens {
        let bare = t.trim_matches(|c: char| c == '.' || c == '-' || c == '_');
        if NOISE.contains(&bare) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
    }
    if out.trim().is_empty() {
        stem.to_ascii_lowercase()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_movie_filename() {
        let e = extract("电影/深空旅人 (2024) 2160p HDR.mkv");
        assert_eq!(e.ext, "mkv");
        assert_eq!(e.asset_type, "video");
        assert_eq!(e.year, Some(2024));
        assert_eq!(e.resolution_tag.as_deref(), Some("2160p"));
        assert!(e.hdr);
        assert!(e.title_guess.contains("深空旅人"));
    }

    #[test]
    fn extracts_episode() {
        let e = extract("剧/深空旅人.S01E03.1080p.中字.mkv");
        assert_eq!(e.episode.as_deref(), Some("S01E03"));
        assert_eq!(e.resolution_tag.as_deref(), Some("1080p"));
        assert_eq!(e.language.as_deref(), Some("zh"));
    }

    #[test]
    fn extracts_book() {
        let e = extract("books/旧港疑云.epub");
        assert_eq!(e.asset_type, "doc");
        assert!(e.year.is_none());
    }
}
