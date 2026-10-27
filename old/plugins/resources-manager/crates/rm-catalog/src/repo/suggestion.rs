//! `ai_suggestion` / `ai_privacy`：AI 建议与隐私档位（`docs/02` §5.1、`docs/01` §7.8）。
//!
//! 建议表是本地草稿，**不同步**；只有"接受之后"的结果才进 op 流水。

use rusqlite::{params, Connection, Result};

use super::optional;

/// 建议行（`docs/02` §5.1）。
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub id: String,
    pub target_kind: String,
    pub target_id: String,
    pub kind: String,
    pub payload: String,
    pub confidence: f64,
    pub model: String,
    pub prompt_hash: String,
    pub input_hash: String,
    pub state: String,
    pub source: String,
    pub created: i64,
    pub applied_at: Option<i64>,
}

pub const SUGGEST_STATES: [&str; 4] = ["pending", "accepted", "rejected", "stale"];

pub fn state_known(s: &str) -> bool {
    SUGGEST_STATES.contains(&s)
}

/// 插入一条建议。state 固定 pending（自动采纳在上层完成状态迁移）。
#[allow(clippy::too_many_arguments)]
pub fn insert(
    conn: &Connection,
    id: &str,
    target_kind: &str,
    target_id: &str,
    kind: &str,
    payload: &str,
    confidence: f64,
    model: &str,
    prompt_hash: &str,
    input_hash: &str,
    source: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO ai_suggestion(
           id, target_kind, target_id, kind, payload, confidence,
           model, prompt_hash, input_hash, state, source, created, applied_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending',?10,?11,NULL)",
        params![id, target_kind, target_id, kind, payload, confidence,
                model, prompt_hash, input_hash, source, now],
    )?;
    Ok(())
}

fn row_to_sugg(r: &rusqlite::Row) -> Result<Suggestion> {
    Ok(Suggestion {
        id: r.get(0)?,
        target_kind: r.get(1)?,
        target_id: r.get(2)?,
        kind: r.get(3)?,
        payload: r.get(4)?,
        confidence: r.get(5)?,
        model: r.get(6)?,
        prompt_hash: r.get(7)?,
        input_hash: r.get(8)?,
        state: r.get(9)?,
        source: r.get(10)?,
        created: r.get(11)?,
        applied_at: r.get(12)?,
    })
}

const SELECT_SUGG: &str = "SELECT id, target_kind, target_id, kind, payload, confidence,
       model, prompt_hash, input_hash, state, source, created, applied_at
       FROM ai_suggestion";

pub fn get(conn: &Connection, id: &str) -> Result<Option<Suggestion>> {
    optional(conn.query_row(
        &format!("{SELECT_SUGG} WHERE id=?1"),
        params![id],
        row_to_sugg,
    ))
}

/// 过滤条件（全部可选）。
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub state: Option<String>,
    pub source: Option<String>,
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

fn where_clause(f: &ListFilter) -> (String, Vec<Box<dyn rusqlite::ToSql + '_>>) {
    let mut conds: Vec<&str> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql + '_>> = Vec::new();
    if let Some(v) = &f.state {
        conds.push("state=?");
        binds.push(Box::new(v.clone()));
    }
    if let Some(v) = &f.source {
        conds.push("source=?");
        binds.push(Box::new(v.clone()));
    }
    if let Some(v) = &f.target_kind {
        conds.push("target_kind=?");
        binds.push(Box::new(v.clone()));
    }
    if let Some(v) = &f.target_id {
        conds.push("target_id=?");
        binds.push(Box::new(v.clone()));
    }
    let sql = if conds.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conds.join(" AND "))
    };
    (sql, binds)
}

pub fn list(conn: &Connection, f: &ListFilter) -> Result<Vec<Suggestion>> {
    let (clause, mut binds) = where_clause(f);
    let sql = format!("{SELECT_SUGG}{clause} ORDER BY created DESC, id DESC LIMIT ? OFFSET ?");
    binds.push(Box::new(f.limit));
    binds.push(Box::new(f.offset));
    let params_ref: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(params_ref.as_slice(), row_to_sugg)?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn count(conn: &Connection, f: &ListFilter) -> Result<u64> {
    let (clause, binds) = where_clause(f);
    let sql = format!("SELECT COUNT(*) FROM ai_suggestion{clause}");
    let params_ref: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let mut st = conn.prepare(&sql)?;
    if params_ref.is_empty() {
        st.query_row([], |r| r.get::<_, u64>(0))
    } else {
        st.query_row(params_ref.as_slice(), |r| r.get::<_, u64>(0))
    }
}

/// 状态迁移（仅从 pending 出发）。
pub fn set_state(conn: &Connection, id: &str, state: &str, now: i64) -> Result<usize> {
    if state == "accepted" {
        conn.execute(
            "UPDATE ai_suggestion SET state=?2, applied_at=?3 WHERE id=?1 AND state='pending'",
            params![id, state, now],
        )
    } else {
        conn.execute(
            "UPDATE ai_suggestion SET state=?2 WHERE id=?1 AND state='pending'",
            params![id, state],
        )
    }
}

/// 按来源批量把 pending 标 rejected（revoke 的一半语义；另一半是回滚已采纳效果）。
pub fn reject_pending_by_source(conn: &Connection, source: &str) -> Result<usize> {
    conn.execute(
        "UPDATE ai_suggestion SET state='rejected' WHERE source=?1 AND state='pending'",
        params![source],
    )
}

/// 目标被人工改动后，把该目标的 pending 建议标 stale。
pub fn mark_stale_for_target(
    conn: &Connection,
    target_kind: &str,
    target_id: &str,
) -> Result<usize> {
    conn.execute(
        "UPDATE ai_suggestion SET state='stale'
         WHERE target_kind=?1 AND target_id=?2 AND state='pending'",
        params![target_kind, target_id],
    )
}

// ===== 隐私档位 =====

pub const PRIVACY_TIERS: [&str; 3] = ["local", "cloud", "hybrid"];

pub fn tier_known(t: &str) -> bool {
    PRIVACY_TIERS.contains(&t)
}

pub fn privacy_set(conn: &Connection, path_prefix: &str, tier: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO ai_privacy(path_prefix, tier) VALUES(?1,?2)
         ON CONFLICT(path_prefix) DO UPDATE SET tier=excluded.tier",
        params![path_prefix, tier],
    )?;
    Ok(())
}

pub fn privacy_list(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut st = conn.prepare("SELECT path_prefix, tier FROM ai_privacy ORDER BY path_prefix")?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 解析路径对应的隐私档位：最长匹配前缀优先；无命中 → hybrid（默认）。
pub fn privacy_resolve(conn: &Connection, path: &str) -> Result<String> {
    let rules = privacy_list(conn)?;
    let mut best: Option<(usize, &str)> = None;
    for (prefix, tier) in &rules {
        if path.starts_with(prefix.as_str()) {
            let len = prefix.len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, tier.as_str()));
            }
        }
    }
    Ok(best.map(|(_, t)| t.to_string()).unwrap_or_else(|| "hybrid".into()))
}
