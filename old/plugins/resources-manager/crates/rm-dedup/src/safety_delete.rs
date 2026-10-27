//! 安全删除的四道检查与硬链接合并（`docs/01` §7.3）。

use rm_catalog::repo::{content, placement, version};
use rm_proto::codes;

/// 删除判定结果。
#[derive(Debug, Clone)]
pub enum DeleteVerdict {
    /// 允许删除这些放置。
    Allowed,
    /// 被拦截：命令行退出码 3（`docs/03` §3.2）。
    Blocked { reason: String, detail: String },
}

/// 删除计划：通过检查后按序执行。
#[derive(Debug, Clone)]
pub struct DeletePlan {
    pub verdict: DeleteVerdict,
    /// 可删除的 placement id（唯一副本场景下为空）。
    pub deletable: Vec<i64>,
    /// 被固定的放置（pinned=1，永不作为候选）。
    pub pinned: Vec<i64>,
}

/// 执行四道检查。`placement_ids` 是**要删除**的放置。
///
/// force=true 时跳过检查 1（但仍受 2/4 约束），调用方负责写风险记录。
pub fn plan_delete(
    conn: &rusqlite::Connection,
    placement_ids: &[i64],
    force: bool,
) -> Result<DeletePlan, rm_proto::BizError> {
    let err = |msg: String| rm_proto::BizError::new(codes::F301, msg);
    let mut deletable = Vec::new();
    let mut pinned = Vec::new();

    // 按内容分组要删的放置。
    let mut by_content: std::collections::BTreeMap<String, Vec<i64>> = Default::default();
    let mut targets = Vec::new();
    for &pid in placement_ids {
        let p = placement::placement_get(conn, pid)
            .map_err(|e| err(e.to_string()))?
            .ok_or_else(|| err(format!("放置不存在：{pid}")))?;
        // p: (id, location_id, rel_path, content_id, size, mtime, inode, state,
        //     first_seen, last_verified, pinned, hash_state, mode)
        if p.10 != 0 {
            pinned.push(pid);
            return Ok(DeletePlan {
                verdict: DeleteVerdict::Blocked {
                    reason: "pinned".into(),
                    detail: format!("放置 {pid} 已被固定，拒绝删除"),
                },
                deletable,
                pinned,
            });
        }
        // 检查 2：离线/冷/丢失副本永不作为删除候选。
        if p.7 != "online" {
            return Ok(DeletePlan {
                verdict: DeleteVerdict::Blocked {
                    reason: "unavailable_copy".into(),
                    detail: format!("放置 {pid} 状态为 {}，离线/冷/丢失副本永不删除", p.7),
                },
                deletable,
                pinned,
            });
        }
        by_content.entry(p.3.clone()).or_default().push(pid);
        targets.push((pid, p.3.clone(), p.9));
    }

    for content_id in by_content.keys() {
        // 检查 4 前置：版本清单引用 → 不允许物理清除内容（放置删除后内容仍被引用则保留行）。
        let all_placements =
            placement::placements_of_content(conn, content_id).map_err(|e| err(e.to_string()))?;
        // 检查 1：除要删的外，至少还有 1 份在线且近期校验过的副本。
        let remaining: Vec<_> = all_placements
            .iter()
            .filter(|(id, _, _, state, _pin, _, lv)| {
                !pids_contains(placement_ids, *id) && state == "online" && lv.is_some()
            })
            .collect();
        if !force && remaining.is_empty() {
            // 没有在线且校验过的其他副本 → 拦截。
            return Ok(DeletePlan {
                verdict: DeleteVerdict::Blocked {
                    reason: "sole_online_copy".into(),
                    detail: format!("内容 {content_id} 除待删放置外没有在线且校验过的副本"),
                },
                deletable,
                pinned,
            });
        }
        // 内容仍被版本清单引用 → 放置可删，但内容行保留（检查 4：不物理清除内容）。
        let _referenced =
            version::referenced_content(conn, content_id).map_err(|e| err(e.to_string()))?;
    }

    for (pid, _cid, _lv) in targets {
        deletable.push(pid);
    }
    Ok(DeletePlan {
        verdict: DeleteVerdict::Allowed,
        deletable,
        pinned,
    })
}

fn pids_contains(ids: &[i64], x: i64) -> bool {
    ids.contains(&x)
}

/// 硬链接合并（检查 3 的落地）：同内容、同位置（同盘）的放置合并 inode。
///
/// 保留 `keep_id` 指向的放置；其余在线放置 unlink 后 link 到保留路径。
/// 返回 (合并数, 释放字节)。
pub fn hardlink_merge(
    conn: &rusqlite::Connection,
    content_id: &str,
    keep_id: Option<i64>,
) -> Result<(u64, i64), rm_proto::BizError> {
    let err = |msg: String| rm_proto::BizError::new(codes::F301, msg);
    let placements =
        placement::placements_of_content(conn, content_id).map_err(|e| err(e.to_string()))?;
    if placements.len() < 2 {
        return Err(rm_proto::BizError::new(
            codes::F301,
            format!("内容 {content_id} 不足两份放置，无需合并"),
        ));
    }
    // 选保留者：keep_id > 未固定且在线的第一份 > 第一份。
    let keep = keep_id
        .or_else(|| {
            placements
                .iter()
                .find(|p| p.3 == "online" && !p.4)
                .map(|p| p.0)
        })
        .unwrap_or(placements[0].0);
    let keep_row = placements
        .iter()
        .find(|p| p.0 == keep)
        .ok_or_else(|| err(format!("保留放置不存在：{keep}")))?;
    let (loc, rel) = (&keep_row.1, &keep_row.2);
    let root: String = conn
        .query_row(
            "SELECT root_path FROM location WHERE id=?1",
            rusqlite::params![loc],
            |r| r.get(0),
        )
        .map_err(|e| err(e.to_string()))?;
    let keep_path = std::path::Path::new(&root).join(rel);
    let size: i64 = conn
        .query_row(
            "SELECT size FROM content WHERE blake3=?1",
            rusqlite::params![content_id],
            |r| r.get(0),
        )
        .map_err(|e| err(e.to_string()))?;

    let mut linked = 0u64;
    let mut freed = 0i64;
    let mut merged_ids = Vec::new();
    for p in &placements {
        if p.0 == keep || p.3 != "online" || p.4 {
            continue;
        }
        let other_root: String = conn
            .query_row(
                "SELECT root_path FROM location WHERE id=?1",
                rusqlite::params![p.1],
                |r| r.get(0),
            )
            .map_err(|e| err(e.to_string()))?;
        // 同盘判定：同根路径（M1 简化——跨位置硬链接仅当 root 相同）。
        if other_root != root {
            continue;
        }
        let other_path = std::path::Path::new(&other_root).join(&p.2);
        // 替换为硬链接：先删旧文件（内容已确认与 keep 相同——同 content_id），再建链接。
        if let Err(e) = std::fs::remove_file(&other_path) {
            return Err(err(format!("{}: {e}", other_path.display())));
        }
        if let Err(e) = std::fs::hard_link(&keep_path, &other_path) {
            return Err(err(format!(
                "硬链接失败 {} → {}: {e}",
                keep_path.display(),
                other_path.display()
            )));
        }
        linked += 1;
        freed += size;
        merged_ids.push(p.0);
    }
    if linked > 0 {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| err(e.to_string()))?;
        let keep_inode = keep_row.5;
        let inode = keep_inode.unwrap_or(0);
        placement::placement_merge_inode(&tx, &merged_ids, inode, content_id)
            .map_err(|e| err(e.to_string()))?;
        tx.commit().map_err(|e| err(e.to_string()))?;
    }
    Ok((linked, freed))
}

/// 执行删除（通过检查后）：删文件 + 删放置（同事务 copy_count-1）。
pub fn execute_delete(
    conn: &mut rusqlite::Connection,
    plan: &DeletePlan,
    roots: &dyn Fn(&str) -> Option<String>,
) -> Result<u64, rm_proto::BizError> {
    if !matches!(plan.verdict, DeleteVerdict::Allowed) {
        return Err(rm_proto::BizError::new(
            codes::F303,
            "删除计划未通过安全检查",
        ));
    }
    let err = |msg: String| rm_proto::BizError::new(codes::F301, msg);
    let mut deleted = 0u64;
    let mut path_errors = Vec::new();
    // 先删文件（DB 之后；文件删失败则该条不进 DB 删除）。
    let mut ok_ids = Vec::new();
    for &pid in &plan.deletable {
        let p = placement::placement_get(conn, pid)
            .map_err(|e| err(e.to_string()))?
            .ok_or_else(|| err(format!("放置不存在：{pid}")))?;
        let root = roots(&p.1).ok_or_else(|| err(format!("位置无根路径：{}", p.1)))?;
        let path = std::path::Path::new(&root).join(&p.2);
        match std::fs::remove_file(&path) {
            Ok(()) => ok_ids.push(pid),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ok_ids.push(pid),
            Err(e) => path_errors.push(format!("{}: {e}", path.display())),
        }
    }
    let tx = conn.transaction().map_err(|e| err(e.to_string()))?;
    for pid in ok_ids {
        placement::placement_delete(&tx, pid).map_err(|e| err(e.to_string()))?;
        deleted += 1;
    }
    tx.commit().map_err(|e| err(e.to_string()))?;
    if !path_errors.is_empty() {
        return Err(err(format!("部分文件删除失败：{}", path_errors.join("; "))));
    }
    let _ = content::get; // 触达 unused 守卫（检查 4 的内容清除留 doctor）
    Ok(deleted)
}
