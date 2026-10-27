//! 字幕/音轨枚举（`docs/01` §5.10 运行时探测——FFmpeg 读容器）。
//!
//! libmpv2 无 node 访问能力（GetData 仅标量），`track-list` 读不到 →
//! 子进程 `ffprobe -print_format json -show_streams <流URL>` 枚举；
//! 降级：ffprobe 缺失/超时 → 空列表（set_subtitle 仍可按裸 sid 使用）。
//! sid 约定（附录 B 登记，已实证）：mpv 按类型 1-based 编号，
//! 即「同类型第 n 条流的 sid = n」；-1 表示关闭。

use std::time::Duration;

use crate::shcmd;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// 单条轨（id = mpv sid/aid，1-based）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub id: i32,
    pub lang: Option<String>,
    pub title: Option<String>,
}

/// 容器轨清单（ffprobe 探测结果）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tracks {
    pub audio: Vec<TrackInfo>,
    pub subtitle: Vec<TrackInfo>,
}

impl Tracks {
    pub fn status_field(&self) -> serde_json::Value {
        fn arr(t: &[TrackInfo]) -> serde_json::Value {
            serde_json::Value::Array(
                t.iter()
                    .map(|x| {
                        serde_json::json!({
                            "id": x.id,
                            "lang": x.lang,
                            "title": x.title,
                        })
                    })
                    .collect(),
            )
        }
        serde_json::json!({
            "audio_tracks": arr(&self.audio),
            "subtitle_tracks": arr(&self.subtitle),
        })
    }
}

/// 探测流 URL；失败返回 `None`（降级：状态里轨列表为空数组）。
pub fn probe(url: &str) -> Option<Tracks> {
    let out = shcmd::run(
        &[
            "ffprobe",
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            url,
        ],
        PROBE_TIMEOUT,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    parse(&v)
}

/// 解析 ffprobe JSON（pub 供测试注入样本）。
pub fn parse(v: &serde_json::Value) -> Option<Tracks> {
    let streams = v.get("streams")?.as_array()?;
    // 按 index 升序保证与容器序一致
    let mut indexed: Vec<&serde_json::Value> = streams.iter().collect();
    indexed.sort_by_key(|s| s.get("index").and_then(|i| i.as_i64()).unwrap_or(0));

    let mut t = Tracks::default();
    let mut audio_n = 0i32;
    let mut sub_n = 0i32;
    for s in indexed {
        let lang = s
            .get("tags")
            .and_then(|g| g.get("language"))
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let title = s
            .get("tags")
            .and_then(|g| g.get("title"))
            .and_then(|x| x.as_str())
            .map(str::to_string);
        match s.get("codec_type").and_then(|x| x.as_str()) {
            Some("audio") => {
                audio_n += 1;
                t.audio.push(TrackInfo { id: audio_n, lang, title });
            }
            Some("subtitle") => {
                sub_n += 1;
                t.subtitle.push(TrackInfo { id: sub_n, lang, title });
            }
            _ => {}
        }
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_parse_ordinal_ids() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"streams":[
                {"index":0,"codec_type":"video"},
                {"index":1,"codec_type":"audio","tags":{"language":"eng"}},
                {"index":2,"codec_type":"subtitle","tags":{"language":"eng","title":"English"}},
                {"index":3,"codec_type":"subtitle","tags":{"language":"jpn","title":"日本語"}}
            ]}"#,
        )
        .unwrap();
        let t = parse(&v).unwrap();
        assert_eq!(t.audio.len(), 1);
        assert_eq!(t.audio[0].id, 1);
        assert_eq!(t.subtitle.len(), 2);
        assert_eq!(t.subtitle[0].id, 1);
        assert_eq!(t.subtitle[1].id, 2);
        assert_eq!(t.subtitle[1].lang.as_deref(), Some("jpn"));
        let st = t.status_field();
        assert_eq!(st["subtitle_tracks"][1]["title"], "日本語");
        assert_eq!(st["subtitle_tracks"][0]["id"], 1);
    }

    #[test]
    fn t2_parse_bad_json() {
        assert!(parse(&serde_json::json!({})).is_none());
        assert!(parse(&serde_json::json!({"streams":"x"})).is_none());
    }

    #[test]
    fn t3_probe_missing_stream_degrades() {
        // 不存在的 URL → ffprobe 失败 → None（不 panic）
        assert!(probe("http://127.0.0.1:1/nope").is_none());
    }
}
