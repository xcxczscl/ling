//! 模型提供方抽象（`docs/01` §7.8、`docs/03` §5.4.2）。
//!
//! M5 提供 **MockProvider**（规则关键词，确定性、零成本）与
//! **OllamaProvider / HttpCloudProvider 占位**（真实接入留待环境具备后）。
//!
//! 隐私三档：`local` 只许本地 Provider；`cloud` 可出网但只传文件名/参数；
//! `hybrid`（默认）分类/关系走本地、资料对齐走云端。

use serde_json::Value;

use crate::extract::Extracted;

/// 提供给 Provider 的上下文（绝不含文件内容——隐私红线）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AnalyzeContext {
    pub target_kind: String,
    pub target_id: String,
    pub rel_path: String,
    pub extracted: Extracted,
    pub existing_tags: Vec<String>,
    pub candidate_titles: Vec<String>,
}

/// 原始建议（Provider 输出；尚未入库）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RawSuggestion {
    /// tag / connection / entry_match / entry_create / version_split / metadata
    pub kind: String,
    pub payload: Value,
    pub confidence: f64,
}

/// Provider 错误（带 F7xx 码）。
#[derive(Debug, Clone)]
pub struct ProviderError {
    pub code: String,
    pub message: String,
}

impl ProviderError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn unreachable(msg: impl Into<String>) -> Self {
        Self::new(crate::codes::F704, msg)
    }

    pub fn bad_response(msg: impl Into<String>) -> Self {
        Self::new(crate::codes::F704, msg)
    }

    pub fn budget_exhausted() -> Self {
        Self::new(crate::codes::F703, "AI 预算耗尽")
    }

    pub fn privacy_blocked(msg: impl Into<String>) -> Self {
        Self::new(crate::codes::F705, msg)
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProviderError {}

/// 模型提供方接口。
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn analyze(&self, ctx: &AnalyzeContext) -> Result<Vec<RawSuggestion>, ProviderError>;
}

/// MockProvider：规则关键词 → 建议，确定性、零成本、离线可跑。
///
/// - 文件名含"科幻/赛博/太空/星际"→ subject 域 tag，置信 0.90
/// - 文件名含"悬疑/侦探/推理"→ subject 域 tag，置信 0.88
/// - 文件名含"纪录片/纪录"→ subject 域 tag，置信 0.86
/// - 有年份 → metadata payload {year}，置信 0.95（纯确定性，属低风险 metadata）
/// - 有剧集编号 → 建议 metadata payload {episode}
pub struct MockProvider;

impl Provider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn analyze(&self, ctx: &AnalyzeContext) -> Result<Vec<RawSuggestion>, ProviderError> {
        let name = format!("{} {}", ctx.rel_path, ctx.extracted.stem);
        let low = name.to_ascii_lowercase();
        let mut out = Vec::new();

        const GENRE_RULES: &[(&str, &str, f64)] = &[
            ("科幻", "科幻", 0.90),
            ("赛博", "赛博朋克", 0.88),
            ("太空", "科幻", 0.85),
            ("星际", "科幻", 0.85),
            ("悬疑", "悬疑", 0.88),
            ("侦探", "推理", 0.87),
            ("推理", "推理", 0.87),
            ("纪录", "纪录片", 0.86),
            ("documentary", "纪录片", 0.86),
            ("sci-fi", "科幻", 0.89),
            ("cyberpunk", "赛博朋克", 0.89),
        ];
        let mut hit_tags: Vec<(&str, f64)> = Vec::new();
        for (pat, tag, conf) in GENRE_RULES {
            if low.contains(pat) && !hit_tags.iter().any(|(t, _)| t == tag) {
                hit_tags.push((tag, *conf));
            }
        }
        for (tag, conf) in hit_tags {
            out.push(RawSuggestion {
                kind: "tag".into(),
                payload: serde_json::json!({"tag_name": tag, "domain": "subject"}),
                confidence: conf,
            });
        }

        if let Some(y) = ctx.extracted.year {
            out.push(RawSuggestion {
                kind: "metadata".into(),
                payload: serde_json::json!({"year": y}),
                confidence: 0.95,
            });
        }
        if let Some(ep) = &ctx.extracted.episode {
            out.push(RawSuggestion {
                kind: "metadata".into(),
                payload: serde_json::json!({"episode": ep}),
                confidence: 0.93,
            });
        }

        // 已有标签的不重复建议
        out.retain(|s| {
            if s.kind == "tag" {
                let name = s.payload["tag_name"].as_str().unwrap_or("");
                !ctx.existing_tags.iter().any(|t| t == name)
            } else {
                true
            }
        });

        Ok(out)
    }
}

/// Ollama 本地模型占位（M5 环境未装 Ollama；接口冻结，环境具备后接入）。
pub struct OllamaProvider {
    pub model: String,
    pub endpoint: String,
}

impl Provider for OllamaProvider {
    fn id(&self) -> &str {
        &self.model
    }

    fn analyze(&self, _ctx: &AnalyzeContext) -> Result<Vec<RawSuggestion>, ProviderError> {
        Err(ProviderError::unreachable(format!(
            "Ollama 未接入（endpoint={}）；M5 环境未装，接口已冻结",
            self.endpoint
        )))
    }
}

/// 云端模型占位（只传文件名与已提取参数，绝不传文件内容——`docs/01` §7.8.4）。
pub struct HttpCloudProvider {
    pub model: String,
    pub endpoint: String,
}

impl Provider for HttpCloudProvider {
    fn id(&self) -> &str {
        &self.model
    }

    fn analyze(&self, _ctx: &AnalyzeContext) -> Result<Vec<RawSuggestion>, ProviderError> {
        Err(ProviderError::unreachable(format!(
            "云端模型未接入（endpoint={}）；M5 环境无外网，接口已冻结",
            self.endpoint
        )))
    }
}

/// 按隐私档位挑选 Provider（`docs/01` §7.8.4）。
pub fn pick_provider<'a>(
    tier: &str,
    path: &str,
    mock: &'a MockProvider,
) -> Result<&'a dyn Provider, ProviderError> {
    match tier {
        "local" | "cloud" | "hybrid" => {
            let _ = path;
            Ok(mock) // M5：真实本地/云端 Provider 未接入，统一走 Mock（接口冻结）
        }
        other => Err(ProviderError::privacy_blocked(format!("未知档位：{other}"))),
    }
}
