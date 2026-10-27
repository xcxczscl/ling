//! 重复组查询：按 copy_count>1 列内容，展开其放置。

use rm_catalog::repo::placement;

/// 一个放置的去重视图。
#[derive(Debug, Clone)]
pub struct DupPlacement {
    pub placement_id: i64,
    pub location_id: String,
    pub rel_path: String,
    pub state: String,
    pub pinned: bool,
    pub inode: Option<i64>,
    pub last_verified: Option<i64>,
}

/// 一个重复组。
#[derive(Debug, Clone)]
pub struct DupGroup {
    pub content_id: String,
    pub size: i64,
    pub asset_type: String,
    pub copy_count: i64,
    pub placements: Vec<DupPlacement>,
}

/// 列重复组（copy_count>1），分页。
pub fn dup_groups(
    conn: &rusqlite::Connection,
    min_size: Option<i64>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<DupGroup>, u64), rm_proto::BizError> {
    let min = min_size.unwrap_or(0);
    let total: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM content WHERE copy_count > 1 AND size >= ?1",
            rusqlite::params![min],
            |r| r.get(0),
        )
        .map_err(|e| rm_proto::BizError::new(rm_proto::codes::F301, e.to_string()))?;
    let mut st = conn
        .prepare(
            "SELECT blake3, size, asset_type, copy_count FROM content
             WHERE copy_count > 1 AND size >= ?1
             ORDER BY size DESC, blake3 LIMIT ?2 OFFSET ?3",
        )
        .map_err(|e| rm_proto::BizError::new(rm_proto::codes::F301, e.to_string()))?;
    let ids: Vec<(String, i64, String, i64)> = st
        .query_map(rusqlite::params![min, limit, offset], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|e| rm_proto::BizError::new(rm_proto::codes::F301, e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| rm_proto::BizError::new(rm_proto::codes::F301, e.to_string()))?;
    let mut groups = Vec::with_capacity(ids.len());
    for (content_id, size, asset_type, copy_count) in ids {
        let placements = placement::placements_of_content(conn, &content_id)
            .map_err(|e| rm_proto::BizError::new(rm_proto::codes::F301, e.to_string()))?
            .into_iter()
            .map(|(id, loc, path, state, pinned, inode, lv)| DupPlacement {
                placement_id: id,
                location_id: loc,
                rel_path: path,
                state,
                pinned,
                inode,
                last_verified: lv,
            })
            .collect();
        groups.push(DupGroup {
            content_id,
            size,
            asset_type,
            copy_count,
            placements,
        });
    }
    Ok((groups, total))
}
