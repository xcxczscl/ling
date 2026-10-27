//! `rm-semantic`：词条/版本/标签的业务规则（`docs/01` §4.4）。
//!
//! 客观属性（分辨率、编码）是版本表结构化列，不物化成标签（A-8）。

#![allow(clippy::too_many_arguments)]

use rm_catalog::repo::{entry, tag, version};
use rm_proto::codes;

fn err(msg: impl Into<String>) -> rm_proto::BizError {
    rm_proto::BizError::new(codes::F302, msg.into())
}

/// 生成短 id：`en_` / `vr_` / `tg_` 前缀 + 序列 + 时间片。
pub fn gen_id(prefix: &str) -> String {
    let ts = rm_catalog::now() as u64;
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{prefix}_{ts:x}{n:08x}")
}

/// 新建词条。kind ∈ work/concept/collection/series/person。
pub fn create_entry(
    conn: &rusqlite::Connection,
    title: &str,
    kind: &str,
    subtitle: Option<&str>,
) -> Result<String, rm_proto::BizError> {
    const KINDS: [&str; 5] = ["work", "concept", "collection", "series", "person"];
    if !KINDS.contains(&kind) {
        return Err(err(format!("未知词条类型：{kind}")));
    }
    if title.trim().is_empty() {
        return Err(err("标题不能为空"));
    }
    let id = gen_id("en");
    let now = rm_catalog::now();
    entry::create(conn, &id, kind, title.trim(), subtitle, now).map_err(|e| err(e.to_string()))?;
    // 同步流水（M2）：本机写 op，便于导出给对端。
    let _ = crate::op_append(
        conn,
        "entry_add",
        serde_json::json!({
            "id": id, "title": title.trim(), "kind": kind,
            "subtitle": subtitle, "ts": now,
        }),
    );
    Ok(id)
}

/// 词条详情：基础信息 + 别名 + 版本 + 直接标签 + 派生标签。
pub fn entry_detail(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<serde_json::Value, rm_proto::BizError> {
    let e = entry::get(conn, id)
        .map_err(|e| err(e.to_string()))?
        .ok_or_else(|| err(format!("词条不存在：{id}")))?;
    let aliases = entry::aliases_of(conn, id).map_err(|e| err(e.to_string()))?;
    let versions = version::versions_of(conn, id).map_err(|e| err(e.to_string()))?;
    let tags = tag::entry_tags(conn, id).map_err(|e| err(e.to_string()))?;
    let derived = tag::derived_entry_tags(conn, id).map_err(|e| err(e.to_string()))?;
    Ok(serde_json::json!({
        "id": e.0,
        "kind": e.1,
        "title": e.2,
        "subtitle": e.3,
        "created": e.4,
        "updated": e.5,
        "aliases": aliases,
        "versions": versions.into_iter().map(|(id, title, ordinal, state, res, codec)| {
            serde_json::json!({"id": id, "title": title, "ordinal": ordinal,
                               "state": state, "resolution": res, "codec": codec})
        }).collect::<Vec<_>>(),
        "tags": tags.into_iter().map(|(id, name, domain, source)| {
            serde_json::json!({"id": id, "name": name, "domain": domain, "source": source})
        }).collect::<Vec<_>>(),
        "derived_tags": derived.into_iter().map(|(id, name, domain)| {
            serde_json::json!({"id": id, "name": name, "domain": domain})
        }).collect::<Vec<_>>(),
    }))
}

/// 给词条加版本。
pub fn add_version(
    conn: &rusqlite::Connection,
    entry_id: &str,
    title: &str,
    ordinal: i64,
    edition: Option<&str>,
    resolution: Option<i64>,
    codec: Option<&str>,
    language: Option<&str>,
    source: Option<&str>,
) -> Result<String, rm_proto::BizError> {
    if entry::get(conn, entry_id)
        .map_err(|e| err(e.to_string()))?
        .is_none()
    {
        return Err(err(format!("词条不存在：{entry_id}")));
    }
    let id = gen_id("vr");
    version::add(
        conn,
        &id,
        entry_id,
        title,
        ordinal,
        edition,
        resolution,
        codec,
        language,
        source.unwrap_or("unknown"),
        rm_catalog::now(),
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            err(format!("词条 {entry_id} 下序号 {ordinal} 已存在"))
        } else {
            err(e.to_string())
        }
    })?;
    let _ = crate::op_append(
        conn,
        "version_add",
        serde_json::json!({
            "id": id, "entry_id": entry_id, "title": title, "ordinal": ordinal,
            "edition": edition, "resolution": resolution, "codec": codec,
            "language": language, "source": source.unwrap_or("unknown"),
            "created": rm_catalog::now(),
        }),
    );
    Ok(id)
}

/// 手动贴标签（source=manual，confidence=1.0）。
pub fn apply_tag(
    conn: &rusqlite::Connection,
    target_kind: &str,
    target_id: &str,
    tag_id: &str,
) -> Result<(), rm_proto::BizError> {
    if tag::tag_get(conn, tag_id)
        .map_err(|e| err(e.to_string()))?
        .is_none()
    {
        return Err(err(format!("标签不存在：{tag_id}")));
    }
    let now = rm_catalog::now();
    match target_kind {
        "entry" => {
            if entry::get(conn, target_id)
                .map_err(|e| err(e.to_string()))?
                .is_none()
            {
                return Err(err(format!("词条不存在：{target_id}")));
            }
            tag::entry_tag_apply(conn, target_id, tag_id, "manual", 1.0, Some("manual"), now)
                .map_err(|e| err(e.to_string()))?;
            let _ = crate::op_append(
                conn,
                "tag_assign",
                serde_json::json!({
                    "target_kind": "entry", "target_id": target_id, "tag_id": tag_id,
                }),
            );
            Ok(())
        }
        "version" => {
            tag::version_tag_apply(conn, target_id, tag_id, "manual", 1.0, Some("manual"), now)
                .map_err(|e| err(e.to_string()))?;
            let _ = crate::op_append(
                conn,
                "tag_assign",
                serde_json::json!({
                    "target_kind": "version", "target_id": target_id, "tag_id": tag_id,
                }),
            );
            Ok(())
        }
        other => Err(err(format!("不支持的贴附对象：{other}"))),
    }
}

/// 确保标签存在（不存在则按域名建顶级标签）。返回 tag_id。
pub fn ensure_tag(
    conn: &rusqlite::Connection,
    name: &str,
    domain: &str,
) -> Result<String, rm_proto::BizError> {
    tag::ensure_builtin_domains(conn).map_err(|e| err(e.to_string()))?;
    // 自定义域：不存在则建（FK tag.domain → tag_domain.id）。
    let domain_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM tag_domain WHERE id=?1",
            rusqlite::params![domain],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !domain_exists {
        conn.execute(
            "INSERT INTO tag_domain(id, name, builtin, default_subject) VALUES(?1,?1,0,'entry')",
            rusqlite::params![domain],
        )
        .map_err(|e| err(e.to_string()))?;
    }
    let existing = tag::tag_list(conn, Some(domain))
        .map_err(|e| err(e.to_string()))?
        .into_iter()
        .find(|t| t.3 == name)
        .map(|t| t.0);
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = gen_id("tg");
    tag::tag_upsert(conn, &id, None, domain, name, rm_catalog::now())
        .map_err(|e| err(e.to_string()))?;
    Ok(id)
}

// ===== M2：关系（Connection） =====

/// 建立关系。返回 connection id。
pub fn connection_add(
    conn: &rusqlite::Connection,
    from_kind: &str,
    from_id: &str,
    to_kind: &str,
    to_id: &str,
    ctype: &str,
    ordinal: Option<i64>,
) -> Result<String, rm_proto::BizError> {
    if !rm_catalog::repo::connection::type_known(ctype) {
        return Err(err(format!("未知关系类型：{ctype}")));
    }
    for (k, id) in [(from_kind, from_id), (to_kind, to_id)] {
        if k == "entry"
            && entry::get(conn, id)
                .map_err(|e| err(e.to_string()))?
                .is_none()
        {
            return Err(err(format!("词条不存在：{id}")));
        }
        if k == "version" {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM version WHERE id=?1",
                    rusqlite::params![id],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if !exists {
                return Err(err(format!("版本不存在：{id}")));
            }
        }
    }
    if from_kind == to_kind && from_id == to_id {
        return Err(err("不能对自身建立关系"));
    }
    let id = gen_id("cn");
    rm_catalog::repo::connection::add(
        conn,
        &id,
        from_kind,
        from_id,
        to_kind,
        to_id,
        ctype,
        ordinal,
        "manual",
        1.0,
        rm_catalog::now(),
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            err("同一条连线已存在")
        } else {
            err(e.to_string())
        }
    })?;
    let created = rm_catalog::now();
    let _ = crate::op_append(
        conn,
        "connection_add",
        serde_json::json!({
            "id": id, "from_kind": from_kind, "from_id": from_id,
            "to_kind": to_kind, "to_id": to_id, "type": ctype,
            "ordinal": ordinal, "attrs_json": "{}",
            "source": "manual", "confidence": 1.0, "created": created,
        }),
    );
    Ok(id)
}

/// 立墓碑删除关系。
pub fn connection_del(
    conn: &rusqlite::Connection,
    connection_id: &str,
) -> Result<(), rm_proto::BizError> {
    let n = rm_catalog::repo::connection::tombstone(conn, connection_id, rm_catalog::now())
        .map_err(|e| err(e.to_string()))?;
    if n == 0 {
        return Err(err(format!("关系不存在：{connection_id}")));
    }
    let _ = crate::op_append(
        conn,
        "connection_del",
        serde_json::json!({"id": connection_id}),
    );
    Ok(())
}

/// 本机设备编号（op 流水用）；取 device 表第一条。
pub fn local_device(conn: &rusqlite::Connection) -> String {
    conn.query_row("SELECT id FROM device LIMIT 1", [], |r| r.get(0))
        .unwrap_or_else(|_| "dv_local".into())
}

/// 写一条本机 op（payload 已 JSON）。
pub fn op_append(
    conn: &rusqlite::Connection,
    kind: &str,
    payload: serde_json::Value,
) -> Result<i64, rm_proto::BizError> {
    let device = local_device(conn);
    let op_id = format!(
        "op_{:x}{:08x}",
        rm_catalog::now() as u64,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    );
    rm_catalog::repo::op::append(
        conn,
        &device,
        &op_id,
        kind,
        &payload.to_string(),
        rm_catalog::now(),
    )
    .map_err(|e| err(e.to_string()))
}

/// 关系行：(id, from_kind, from_id, to_kind, to_id, type, ordinal, created)。
pub type ConnectionRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    i64,
);

/// 列关系。
pub fn connection_list(
    conn: &rusqlite::Connection,
    from: Option<(&str, &str)>,
    to: Option<(&str, &str)>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<ConnectionRow>, u64), rm_proto::BizError> {
    rm_catalog::repo::connection::list(conn, from, to, limit, offset)
        .map_err(|e| err(e.to_string()))
}
