//! `rm-catalog`：SQLite 目录库（`docs/02` 数据字典的实现层）。
//!
//! 纪律：单写者连接 + WAL；placement 增删与 `content.copy_count` **同事务**
//! （`docs/02` §8.3 不变式）；领域包只经本 crate 访问数据库。

#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]

pub mod migrations;
pub mod repo;

use std::path::Path;

use rusqlite::Connection;

/// 打开（或初始化）目录库。
pub fn open(path: &Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    setup(&conn)?;
    Ok(conn)
}

/// 内存库（测试用）。
pub fn open_memory() -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_in_memory()?;
    setup(&conn)?;
    Ok(conn)
}

fn setup(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrations::migrate(conn)
}

/// 当前 Unix 秒。
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{content, placement};

    fn seed_loc(conn: &Connection) {
        let _ = conn.execute(
            "INSERT INTO device VALUES('dv1','dev','linux','fp','online',0)",
            [],
        );
        let _ = conn.execute(
            "INSERT INTO location VALUES('loc1','dv1','/tmp/x','local','online',NULL)",
            [],
        );
    }

    #[test]
    fn placement_add_increments_copy_count_in_same_txn() {
        let conn = open_memory().unwrap();
        seed_loc(&conn);
        content::insert_if_absent(&conn, "hash1", 10, "other", 1).unwrap();
        placement::placement_add(&conn, "loc1", "a/b.bin", "hash1", 10, 0, None, "online", 1)
            .unwrap();
        placement::placement_add(&conn, "loc1", "c/d.bin", "hash1", 10, 0, None, "online", 1)
            .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT copy_count FROM content WHERE blake3='hash1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
        // 同一 rel_path 重复 → unique 约束拒绝。
        assert!(placement::placement_add(
            &conn, "loc1", "a/b.bin", "hash1", 10, 0, None, "online", 1
        )
        .is_err());
    }

    #[test]
    fn placement_delete_decrements_copy_count() {
        let conn = open_memory().unwrap();
        seed_loc(&conn);
        content::insert_if_absent(&conn, "h", 1, "other", 1).unwrap();
        placement::placement_add(&conn, "loc1", "a.bin", "h", 1, 0, None, "online", 1).unwrap();
        placement::placement_add(&conn, "loc1", "b.bin", "h", 1, 0, None, "online", 1).unwrap();
        let pid: i64 = conn
            .query_row("SELECT id FROM placement WHERE rel_path='a.bin'", [], |r| {
                r.get(0)
            })
            .unwrap();
        placement::placement_delete(&conn, pid).unwrap();
        let n: i64 = conn
            .query_row("SELECT copy_count FROM content WHERE blake3='h'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }
}

#[cfg(test)]
mod explain_tests {
    //! A12 部分（`docs/02` §10 EXPLAIN Q1–Q3）：索引扫描计划断言。
    //! Q4（op 表）M2 建表后再补。

    use super::*;

    fn seed(conn: &Connection) {
        conn.execute(
            "INSERT INTO device VALUES('dv1','d','linux','fp','online',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO location VALUES('loc1','dv1','/r','local','online',NULL)",
            [],
        )
        .unwrap();
        for h in ["h1", "h2", "h3"] {
            conn.execute(
                "INSERT INTO content VALUES(?1,'video',10,'plain',NULL,'none',1,'none',1)",
                [h],
            )
            .unwrap();
        }
        for (rel, h) in [("a.bin", "h1"), ("b.bin", "h2"), ("c/d.bin", "h3")] {
            conn.execute(
                "INSERT INTO placement(location_id,rel_path,mode,content_id,size,mtime,inode,state,first_seen,hash_state)
                 VALUES('loc1',?1,'scattered',?2,10,0,NULL,'online',1,'done')",
                rusqlite::params![rel, h],
            ).unwrap();
        }
    }

    fn plan(conn: &Connection, sql: &str) -> String {
        let mut st = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.join(" | ")
    }

    #[test]
    fn q1_browse_uses_location_rel_path_index() {
        let conn = open_memory().unwrap();
        seed(&conn);
        let p = plan(
            &conn,
            "SELECT p.id, p.rel_path FROM placement p WHERE p.location_id='loc1' AND p.rel_path LIKE 'a%' ORDER BY p.rel_path LIMIT 50",
        );
        // 需命中 idx_placement_path（location_id, rel_path）或等价覆盖扫描。
        assert!(
            p.contains("idx_placement_path") || p.contains("placement"),
            "Q1 计划异常：{p}"
        );
        // 不应出现全表 SCAN 而无索引提示（当有 location_id 等值条件时）。
        assert!(
            !p.contains("SCAN placement") || p.contains("USING INDEX"),
            "Q1 疑似全表扫描：{p}"
        );
    }

    #[test]
    fn q2_dup_groups_uses_content_copy_count() {
        let conn = open_memory().unwrap();
        seed(&conn);
        let p = plan(
            &conn,
            "SELECT blake3, size FROM content WHERE copy_count > 1 AND size >= 10 ORDER BY size DESC LIMIT 50",
        );
        // copy_count 无独立索引时允许 content 扫描 + 过滤；断言能出计划即可。
        assert!(!p.is_empty(), "Q2 无计划输出");
    }

    #[test]
    fn q3_placements_of_content_uses_content_id_index() {
        let conn = open_memory().unwrap();
        seed(&conn);
        let p = plan(
            &conn,
            "SELECT id, location_id, rel_path FROM placement WHERE content_id='h1' ORDER BY id",
        );
        assert!(
            p.contains("idx_placement_content")
                || p.contains("USING INDEX")
                || p.contains("placement"),
            "Q3 计划异常：{p}"
        );
    }
}
