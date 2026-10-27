//! `rm-dedup`：重复组与安全删除（`docs/01` §7.3）。
//!
//! 四道检查写死不可配置绕过：
//! 1. 除要删的这份外，至少还有 1 份**在线且近期校验过**的副本；
//! 2. 离线/冷/丢失副本永不作为删除候选；
//! 3. 同盘优先硬链接合并（见 `hardlink`）；
//! 4. 放置归零且无版本清单引用才允许清除内容。

pub mod group;
pub mod safety_delete;

pub use group::{dup_groups, DupGroup};
pub use safety_delete::{execute_delete, hardlink_merge, plan_delete, DeletePlan, DeleteVerdict};

#[cfg(test)]
mod tests {
    use super::*;
    use rm_catalog::repo::placement;

    fn setup() -> rusqlite::Connection {
        let conn = rm_catalog::open_memory().unwrap();
        let _ = conn.execute(
            "INSERT INTO device VALUES('dv1','dev','linux','fp','online',0)",
            [],
        );
        let _ = conn.execute(
            "INSERT INTO location VALUES('loc1','dv1','/tmp/a','local','online',NULL)",
            [],
        );
        let _ = conn.execute(
            "INSERT INTO location VALUES('loc2','dv1','/tmp/b','local','online',NULL)",
            [],
        );
        for h in ["h1", "h2", "h3", "h4"] {
            let _ = conn.execute(
                "INSERT INTO content VALUES(?1,'other',1,'plain',NULL,'none',0,'none',1)",
                [h],
            );
        }
        conn
    }

    #[test]
    fn sole_online_copy_is_blocked() {
        let conn = setup();
        placement::placement_add(&conn, "loc1", "only.bin", "h1", 1, 0, None, "online", 1).unwrap();
        let pid: i64 = conn
            .query_row(
                "SELECT id FROM placement WHERE rel_path='only.bin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let plan = plan_delete(&conn, &[pid], false).unwrap();
        assert!(matches!(plan.verdict, DeleteVerdict::Blocked { .. }));
    }

    #[test]
    fn verified_other_online_copy_allows_delete() {
        let conn = setup();
        placement::placement_add(&conn, "loc1", "a.bin", "h2", 1, 0, None, "online", 1).unwrap();
        placement::placement_add(&conn, "loc2", "b.bin", "h2", 1, 0, None, "online", 1).unwrap();
        let pid_a: i64 = conn
            .query_row("SELECT id FROM placement WHERE rel_path='a.bin'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let pid_b: i64 = conn
            .query_row("SELECT id FROM placement WHERE rel_path='b.bin'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // 给 b 打上校验时间。
        conn.execute("UPDATE placement SET last_verified=1 WHERE id=?1", [pid_b])
            .unwrap();
        let plan = plan_delete(&conn, &[pid_a], false).unwrap();
        assert!(matches!(plan.verdict, DeleteVerdict::Allowed));
        assert_eq!(plan.deletable, vec![pid_a]);
    }

    #[test]
    fn pinned_placement_is_blocked() {
        let conn = setup();
        placement::placement_add(&conn, "loc1", "p.bin", "h3", 1, 0, None, "online", 1).unwrap();
        let pid: i64 = conn
            .query_row("SELECT id FROM placement WHERE rel_path='p.bin'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute("UPDATE placement SET pinned=1 WHERE id=?1", [pid])
            .unwrap();
        let plan = plan_delete(&conn, &[pid], true).unwrap(); // force 也不能删 pinned
        assert!(matches!(plan.verdict, DeleteVerdict::Blocked { .. }));
    }

    #[test]
    fn offline_copy_is_blocked_as_candidate() {
        let conn = setup();
        placement::placement_add(&conn, "loc1", "o.bin", "h4", 1, 0, None, "cold", 1).unwrap();
        let pid: i64 = conn
            .query_row("SELECT id FROM placement WHERE rel_path='o.bin'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let plan = plan_delete(&conn, &[pid], false).unwrap();
        assert!(matches!(plan.verdict, DeleteVerdict::Blocked { .. }));
    }
}
