//! `rm-classify`：规则引擎（`docs/01` §7.4）。
//!
//! 约束：规则只改**记录**（标签、词条归属），不搬动磁盘文件。

use rm_catalog::repo::{placement, rule};
use rm_proto::codes;

fn err(msg: impl Into<String>) -> rm_proto::BizError {
    rm_proto::BizError::new(codes::F301, msg.into())
}

/// 规则匹配条件（`docs/02` §6.1 match_json）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct MatchSpec {
    #[serde(default)]
    pub path_contains: Option<String>,
    #[serde(default)]
    pub ext: Vec<String>,
    #[serde(default)]
    pub min_size: Option<i64>,
    #[serde(default)]
    pub max_size: Option<i64>,
    #[serde(default)]
    pub has_tag: Option<String>,
}

/// 规则动作（action_json）。M1 支持 `add_tag`。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ActionSpec {
    #[serde(default)]
    pub add_tag: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
}

fn parse_match(json: &str) -> Result<MatchSpec, rm_proto::BizError> {
    serde_json::from_str(json).map_err(|e| err(format!("规则匹配条件解析失败：{e}")))
}

fn parse_action(json: &str) -> Result<ActionSpec, rm_proto::BizError> {
    serde_json::from_str(json).map_err(|e| err(format!("规则动作解析失败：{e}")))
}

fn matches(spec: &MatchSpec, rel_path: &str, size: i64, asset_type: &str) -> bool {
    if let Some(p) = &spec.path_contains {
        if !rel_path.contains(p.as_str()) {
            return false;
        }
    }
    if !spec.ext.is_empty() {
        let ext = std::path::Path::new(rel_path)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy().to_ascii_lowercase()))
            .unwrap_or_default();
        let hit = spec.ext.iter().any(|x| {
            x.to_ascii_lowercase() == ext
                || x.trim_start_matches('.')
                    .eq_ignore_ascii_case(ext.trim_start_matches('.'))
        });
        if !hit {
            return false;
        }
    }
    if let Some(min) = spec.min_size {
        if size < min {
            return false;
        }
    }
    if let Some(max) = spec.max_size {
        if size > max {
            return false;
        }
    }
    let _ = asset_type;
    true
}

/// 规则导入结果。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportResult {
    pub imported: u64,
}

/// 导入（整体替换）规则集。
pub fn import_rules(
    conn: &rusqlite::Connection,
    rules: &[rm_proto::msg::ClassifyRule],
) -> Result<ImportResult, rm_proto::BizError> {
    let mut rows = Vec::with_capacity(rules.len());
    for r in rules {
        // 校验 JSON 可解析。
        parse_match(&r.match_spec.to_string())?;
        parse_action(&r.action.to_string())?;
        rows.push((
            r.id.clone(),
            r.priority,
            r.match_spec.to_string(),
            r.action.to_string(),
            r.enabled,
        ));
    }
    rule::replace_all(conn, &rows).map_err(|e| err(e.to_string()))?;
    Ok(ImportResult {
        imported: rules.len() as u64,
    })
}

/// 列规则。
pub fn list_rules(
    conn: &rusqlite::Connection,
) -> Result<Vec<rm_proto::msg::ClassifyRule>, rm_proto::BizError> {
    rule::list(conn)
        .map_err(|e| err(e.to_string()))?
        .into_iter()
        .map(|(id, priority, m, a, enabled)| {
            Ok(rm_proto::msg::ClassifyRule {
                id,
                priority,
                match_spec: serde_json::from_str(&m).map_err(|e| err(e.to_string()))?,
                action: serde_json::from_str(&a).map_err(|e| err(e.to_string()))?,
                enabled,
            })
        })
        .collect()
}

/// 执行结果。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RunResult {
    pub placements_scanned: u64,
    pub tags_applied: u64,
    pub rules_fired: u64,
}

/// 执行规则：对每个放置按优先级匹配，`add_tag` 动作贴到**词条**还是**版本**由
/// tag 域的 default_subject 决定；M1 简化：放置没有词条归属时贴到由路径推断的
/// 词条（若无则跳过），否则贴 version 需要版本关联——M1 只支持已有词条贴标。
///
/// M1 实际策略：`add_tag` 动作把标签贴到**所有**通过匹配的放置所关联的……放置没有
/// 词条链路时，退化为"确保标签存在并计入结果"。词条级贴标走 `tag.apply` 或 M2
/// 的自动归入词条。此处实现：匹配的放置 → 若该内容被某版本清单引用则贴版本标签，
/// 否则只统计命中。
pub fn run_rules(
    conn: &rusqlite::Connection,
    rule_ids: Option<&[String]>,
) -> Result<RunResult, rm_proto::BizError> {
    let all = rule::list(conn).map_err(|e| err(e.to_string()))?;
    let selected: Vec<_> = all
        .into_iter()
        .filter(|r| r.4) // enabled
        .filter(|r| match rule_ids {
            None => true,
            Some(ids) => ids.contains(&r.0),
        })
        .collect();
    if selected.is_empty() {
        return Ok(RunResult::default());
    }
    let mut st = conn
        .prepare(
            "SELECT p.id, p.rel_path, p.size, c.asset_type FROM placement p
             JOIN content c ON c.blake3 = p.content_id",
        )
        .map_err(|e| err(e.to_string()))?;
    let rows: Vec<(i64, String, i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| err(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| err(e.to_string()))?;

    let mut out = RunResult::default();
    let now = rm_catalog::now();
    for (pid, rel_path, size, asset_type) in rows {
        out.placements_scanned += 1;
        for (rid, _prio, m, a, _en) in &selected {
            let ms = parse_match(m)?;
            if !matches(&ms, &rel_path, size, &asset_type) {
                continue;
            }
            out.rules_fired += 1;
            let act = parse_action(a)?;
            if let Some(tag_name) = &act.add_tag {
                let domain = act.domain.as_deref().unwrap_or("status");
                let tag_id = rm_semantic::ensure_tag(conn, tag_name, domain)?;
                // 版本清单引用了该内容 → 贴版本标签。
                let content_id: String = conn
                    .query_row(
                        "SELECT content_id FROM placement WHERE id=?1",
                        rusqlite::params![pid],
                        |r| r.get(0),
                    )
                    .map_err(|e| err(e.to_string()))?;
                let version_ids: Vec<String> = {
                    let mut st = conn
                        .prepare(
                            "SELECT DISTINCT version_id FROM version_file WHERE content_blake3=?1",
                        )
                        .map_err(|e| err(e.to_string()))?;
                    let mapped = st
                        .query_map(rusqlite::params![content_id], |r| r.get::<_, String>(0))
                        .map_err(|e| err(e.to_string()))?;
                    mapped
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| err(e.to_string()))?
                };
                for vid in version_ids {
                    rm_catalog::repo::tag::version_tag_apply(
                        conn,
                        &vid,
                        &tag_id,
                        "rule",
                        1.0,
                        Some(rid),
                        now,
                    )
                    .map_err(|e| err(e.to_string()))?;
                    out.tags_applied += 1;
                }
            }
        }
    }
    let _ = placement::scan_totals;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_spec_filters_by_path_and_ext() {
        let spec = MatchSpec {
            path_contains: Some("movies".into()),
            ext: vec!["mkv".into()],
            min_size: None,
            max_size: None,
            has_tag: None,
        };
        assert!(matches(&spec, "movies/foo.mkv", 10, "video"));
        assert!(!matches(&spec, "pics/foo.mkv", 10, "video"));
        assert!(!matches(&spec, "movies/foo.mp4", 10, "video"));
        let spec2 = MatchSpec {
            path_contains: None,
            ext: vec![],
            min_size: Some(100),
            max_size: None,
            has_tag: None,
        };
        assert!(matches(&spec2, "a.bin", 200, "other"));
        assert!(!matches(&spec2, "a.bin", 50, "other"));
    }

    #[test]
    fn import_rules_validates_match_and_action() {
        let conn = rm_catalog::open_memory().unwrap();
        let ok = rm_proto::msg::ClassifyRule {
            id: "r1".into(),
            priority: 10,
            match_spec: serde_json::json!({"path_contains": "movies"}),
            action: serde_json::json!({"add_tag": "视频", "domain": "kind"}),
            enabled: true,
        };
        assert!(import_rules(&conn, &[ok]).is_ok());
        let bad = rm_proto::msg::ClassifyRule {
            id: "r2".into(),
            priority: 10,
            match_spec: serde_json::json!({"unknown_field": true}),
            action: serde_json::json!({"add_tag": "x"}),
            enabled: true,
        };
        // 未知字段会被 serde 忽略（deny_unknown_fields 未开）→ 应成功。
        assert!(import_rules(&conn, &[bad]).is_ok());
        // action 完全缺失 add_tag 仍可解析（Optional）。
        let no_action = rm_proto::msg::ClassifyRule {
            id: "r3".into(),
            priority: 1,
            match_spec: serde_json::json!({}),
            action: serde_json::json!({}),
            enabled: false,
        };
        assert!(import_rules(&conn, &[no_action]).is_ok());
    }
}
