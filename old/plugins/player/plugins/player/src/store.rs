//! player.db（`docs/01` §5.11，P6）：会话表 + 设置表，WAL。
//!
//! - 会话主键 = `content_id`（rm 内容指纹）；读回供 `player.play` 续播，
//!   写侧在进度/暂停/seek/音量/字幕方法与停用 flush 时**写穿**。
//! - `setting` 表：`device_id`（get-or-create）与输出选择态（#39，`outputs.selected`）。
//! - 打开失败 → 调用方降级纯内存（不阻塞装载/播放）；单次读写失败按
//!   best-effort 忽略——持久化是增强面，绝不能打断播放主链。
//! - P7：`SyncPayload`（§5.9 同步报文投影）+ `apply_sync`（LWW 接收——
//!   仅落库、不扰在播会话；自报/旧报/同秒并列语义见 `SyncOutcome`）。

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

/// §5.11 schema 照表落地（`IF NOT EXISTS` 幂等；WAL 在 open 时设）。
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS session (
    content_id     TEXT PRIMARY KEY,
    position_sec   REAL NOT NULL DEFAULT 0.0,
    volume         REAL NOT NULL DEFAULT 1.0,
    subtitle_track INTEGER NOT NULL DEFAULT -1,
    audio_track    INTEGER NOT NULL DEFAULT 0,
    playback_rate  REAL NOT NULL DEFAULT 1.0,
    updated_at     INTEGER NOT NULL,
    device_id      TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS setting (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// 已持久化的会话快照（`play` 读回做续播）。
#[derive(Debug, Clone, PartialEq)]
pub struct SavedSession {
    pub position_sec: f64,
    pub volume: f64,
    pub subtitle_track: i32,
    pub audio_track: i32,
    pub playback_rate: f64,
    pub updated_at: i64,
    pub device_id: String,
}

/// 连接持锁串行化（backend 事件线程与方法线程并发写）。
pub struct Store {
    conn: Mutex<Connection>,
    device_id: String,
}

/// 当前 Unix 秒（`updated_at` 盖戳与 P7 同步节流共用时钟）。
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Store {
    /// 打开（不存在则建目录/库）；WAL + busy_timeout；建表；确保 `device_id`。
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("建数据目录 {} 失败：{e}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| format!("打开 {} 失败：{e}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("设 WAL 失败：{e}"))?;
        conn.busy_timeout(Duration::from_millis(5000))
            .map_err(|e| format!("设 busy_timeout 失败：{e}"))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("建表失败：{e}"))?;
        let device_id = Self::ensure_device_id(&conn)?;
        Ok(Self { conn: Mutex::new(conn), device_id })
    }

    /// 本机设备号（setting 表 get-or-create；首版随机 8 字节 hex——
    /// P7 对等同步的对端标识沿用此值）。
    fn ensure_device_id(conn: &Connection) -> Result<String, String> {
        if let Some(v) = conn
            .query_row(
                "SELECT value FROM setting WHERE key = 'device_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| format!("读 device_id 失败：{e}"))?
        {
            return Ok(v);
        }
        let mut raw = [0u8; 8];
        let _ = getrandom::getrandom(&mut raw);
        let v = raw.iter().map(|b| format!("{b:02x}")).collect::<String>();
        conn.execute(
            "INSERT INTO setting (key, value) VALUES ('device_id', ?1)",
            params![v],
        )
        .map_err(|e| format!("写 device_id 失败：{e}"))?;
        Ok(v)
    }

    /// 本机设备号；P7 对等同步/接力作对端标识消费（附录 B 登记）。
    #[allow(dead_code)] // 当前仅单测断言 get-or-create 语义
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 读回会话快照；无行 → `None`。
    pub fn load_session(&self, content_id: &str) -> Option<SavedSession> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT position_sec, volume, subtitle_track, audio_track,
                    playback_rate, updated_at, device_id
             FROM session WHERE content_id = ?1",
            params![content_id],
            |row| {
                Ok(SavedSession {
                    position_sec: row.get(0)?,
                    volume: row.get(1)?,
                    subtitle_track: row.get(2)?,
                    audio_track: row.get(3)?,
                    playback_rate: row.get(4)?,
                    updated_at: row.get(5)?,
                    device_id: row.get(6)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 写穿 upsert（整行覆盖；`updated_at`/`device_id` 由调用方盖戳）。
    pub fn upsert_session(&self, content_id: &str, s: &SavedSession) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session
                (content_id, position_sec, volume, subtitle_track,
                 audio_track, playback_rate, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(content_id) DO UPDATE SET
                position_sec   = excluded.position_sec,
                volume         = excluded.volume,
                subtitle_track = excluded.subtitle_track,
                audio_track    = excluded.audio_track,
                playback_rate  = excluded.playback_rate,
                updated_at     = excluded.updated_at,
                device_id      = excluded.device_id",
            params![
                content_id,
                s.position_sec,
                s.volume,
                s.subtitle_track,
                s.audio_track,
                s.playback_rate,
                s.updated_at,
                s.device_id
            ],
        )
        .map_err(|e| format!("upsert session 失败：{e}"))?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT value FROM setting WHERE key = ?1", params![key], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .ok()
        .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO setting (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|e| format!("写 setting {key} 失败：{e}"))?;
        Ok(())
    }

    /// 纯盖戳（now + 本机 device_id，不落库）——写库调 `upsert_session`、
    /// 同步报文取同一批盖戳值，保证「本地行」与「推出去的报文」一致。
    pub fn stamp(&self, s: &SavedSession) -> SavedSession {
        SavedSession { updated_at: now_secs(), device_id: self.device_id.clone(), ..s.clone() }
    }

    /// P7 LWW 接收（§5.9「updated_at 更大者胜」）：
    /// - 自报 → `SelfDevice`（不写）；
    /// - 本机已有行且报文 `updated_at` **严格更小** → `Stale`（不写）；
    ///   同秒并列（< 改为 <= 的补集）→ **后到者胜**——秒级时间戳下先到的
    ///   同秒报文不得吞掉后到的暂停/停止报文（附录 B 登记）；
    /// - 合入时保留本机已有 `audio_track`/`playback_rate`（报文不含两列，
    ///   新行用 schema 默认 0/1.0）。
    pub fn apply_sync(&self, p: &SyncPayload) -> Result<SyncOutcome, String> {
        if p.device_id == self.device_id {
            return Ok(SyncOutcome::SelfDevice);
        }
        if let Some(existing) = self.load_session(&p.content_id) {
            if p.updated_at < existing.updated_at {
                return Ok(SyncOutcome::Stale);
            }
            let merged = SavedSession {
                position_sec: p.position_sec,
                volume: p.volume,
                subtitle_track: p.subtitle_track,
                audio_track: existing.audio_track,
                playback_rate: existing.playback_rate,
                updated_at: p.updated_at,
                device_id: p.device_id.clone(),
            };
            self.upsert_session(&p.content_id, &merged)?;
        } else {
            let fresh = SavedSession {
                position_sec: p.position_sec,
                volume: p.volume,
                subtitle_track: p.subtitle_track,
                audio_track: 0,
                playback_rate: 1.0,
                updated_at: p.updated_at,
                device_id: p.device_id.clone(),
            };
            self.upsert_session(&p.content_id, &fresh)?;
        }
        Ok(SyncOutcome::Applied)
    }
}

/// P7 §5.9 同步报文（严格照表：无 audio_track/playback_rate——接收侧
/// 保留本机已有值，新行用 schema 默认，附录 B 登记）。
#[derive(Debug, Clone, PartialEq)]
pub struct SyncPayload {
    pub content_id: String,
    pub position_sec: f64,
    pub volume: f64,
    pub subtitle_track: i32,
    pub updated_at: i64,
    pub device_id: String,
}

impl SyncPayload {
    /// 解析 + 校验（协议面错误一律返回 Err(原因)，由控制面翻 P100）：
    /// `type` 必须 `session.sync`；content_id/device_id 非空；updated_at>0；
    /// position_sec ≥0 有限；volume ∈ 0..=1；subtitle_track 在 i32 内。
    pub fn parse(v: &serde_json::Value) -> Result<Self, String> {
        let ty = v.get("type").and_then(|x| x.as_str()).ok_or("缺 type")?;
        if ty != "session.sync" {
            return Err(format!("type 须为 session.sync，收到：{ty}"));
        }
        let content_id = v.get("content_id").and_then(|x| x.as_str()).unwrap_or("");
        if content_id.is_empty() {
            return Err("content_id 缺失或为空".into());
        }
        let device_id = v.get("device_id").and_then(|x| x.as_str()).unwrap_or("");
        if device_id.is_empty() {
            return Err("device_id 缺失或为空".into());
        }
        let position_sec = v
            .get("position_sec")
            .and_then(|x| x.as_f64())
            .ok_or("position_sec 缺失")?;
        if !position_sec.is_finite() || position_sec < 0.0 {
            return Err("position_sec 须为 ≥0 的有限数".into());
        }
        let volume = v.get("volume").and_then(|x| x.as_f64()).ok_or("volume 缺失")?;
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            return Err("volume 须在 0..=1".into());
        }
        let subtitle_track = v
            .get("subtitle_track")
            .and_then(|x| x.as_i64())
            .ok_or("subtitle_track 缺失")?;
        let subtitle_track = i32::try_from(subtitle_track).map_err(|_| "subtitle_track 超出 i32 范围")?;
        let updated_at = v.get("updated_at").and_then(|x| x.as_i64()).ok_or("updated_at 缺失")?;
        if updated_at <= 0 {
            return Err("updated_at 须为正 Unix 秒".into());
        }
        Ok(Self {
            content_id: content_id.to_string(),
            position_sec,
            volume,
            subtitle_track,
            updated_at,
            device_id: device_id.to_string(),
        })
    }
}

/// LWW 接收结果（`player.control.status` 计数与 `/sync` 响应 reason 消费）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// 合入（新行或 updated_at 胜出覆盖）。
    Applied,
    /// 旧报（updated_at 严格小于本机已有）→ 丢弃。
    Stale,
    /// 自报（device_id == 本机）→ 丢弃（接力不成环）。
    SelfDevice,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        (dir, path)
    }

    fn sample() -> SavedSession {
        SavedSession {
            position_sec: 5.5,
            volume: 0.42,
            subtitle_track: -1,
            audio_track: 0,
            playback_rate: 1.0,
            updated_at: 1,
            device_id: "x".into(),
        }
    }

    /// t1：建库即 WAL + 两表就位（§5.11 schema）。
    #[test]
    fn t1_open_wal_and_schema() {
        let (_d, db) = tmp_db("t1.db");
        let store = Store::open(&db).unwrap();
        let mode: String = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(mode.to_lowercase(), "wal", "WAL 模式");
        let n: i64 = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'
                 AND name IN ('session','setting')",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(n, 2, "session/setting 两表");
    }

    /// t2：upsert → load 往返；重复 upsert 覆盖（冲突路径）。
    #[test]
    fn t2_upsert_load_roundtrip_and_overwrite() {
        let (_d, db) = tmp_db("t2.db");
        let store = Store::open(&db).unwrap();
        assert!(store.load_session("blake3:aa").is_none(), "无行 → None");

        store.upsert_session("blake3:aa", &sample()).unwrap();
        let got = store.load_session("blake3:aa").unwrap();
        assert_eq!(got, sample(), "整行往返一致");

        let upd = SavedSession { position_sec: 9.5, subtitle_track: 1, ..sample() };
        store.upsert_session("blake3:aa", &upd).unwrap();
        assert_eq!(store.load_session("blake3:aa").unwrap(), upd, "冲突覆盖");
        assert!(store.load_session("blake3:bb").is_none(), "他键不受影响");
    }

    /// t3：setting get/set 往返；缺键 → None。
    #[test]
    fn t3_settings_roundtrip() {
        let (_d, db) = tmp_db("t3.db");
        let store = Store::open(&db).unwrap();
        assert!(store.get_setting("outputs.selected").is_none());
        store.set_setting("outputs.selected", "alsa_output.demo").unwrap();
        assert_eq!(
            store.get_setting("outputs.selected").as_deref(),
            Some("alsa_output.demo")
        );
        store.set_setting("outputs.selected", "auto").unwrap();
        assert_eq!(store.get_setting("outputs.selected").as_deref(), Some("auto"));
    }

    /// t4：device_id get-or-create 跨重开稳定。
    #[test]
    fn t4_device_id_stable_across_reopen() {
        let (_d, db) = tmp_db("t4.db");
        let first = Store::open(&db).unwrap().device_id().to_string();
        assert_eq!(first.len(), 16, "8 字节 hex");
        let again = Store::open(&db).unwrap().device_id().to_string();
        assert_eq!(first, again, "重开沿用同一 device_id");
    }

    /// t5：库路径的父目录不存在 → 自动创建（PLAYER_DATA_DIR 指到新目录场景）。
    #[test]
    fn t5_open_creates_parent_dirs() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("nested").join("deeper").join("player.db");
        assert!(!db.exists());
        Store::open(&db).unwrap();
        assert!(db.is_file());
    }

    /// t6：stamp 盖戳（now>0、device_id=本机）。
    #[test]
    fn t6_stamp_stamps() {
        let (_d, db) = tmp_db("t6.db");
        let store = Store::open(&db).unwrap();
        let raw = SavedSession { updated_at: 0, device_id: String::new(), ..sample() };
        let stamped = store.stamp(&raw);
        store.upsert_session("blake3:cc", &stamped).unwrap();
        let got = store.load_session("blake3:cc").unwrap();
        assert!(got.updated_at > 0, "updated_at 盖当前时间");
        assert_eq!(got.device_id, store.device_id(), "device_id 盖本机号");
    }

    // ===== P7：§5.9 同步报文与 LWW 接收 =====

    fn sync_payload(updated_at: i64, device_id: &str, pos: f64) -> SyncPayload {
        SyncPayload {
            content_id: "blake3:sync".into(),
            position_sec: pos,
            volume: 0.8,
            subtitle_track: 0,
            updated_at,
            device_id: device_id.into(),
        }
    }

    fn sync_json() -> serde_json::Value {
        serde_json::json!({
            "type": "session.sync",
            "content_id": "blake3:sync",
            "position_sec": 1935.2,
            "volume": 0.8,
            "subtitle_track": 0,
            "updated_at": 1729180800,
            "device_id": "dv_peer"
        })
    }

    /// t7：报文解析——照 §5.9 全字段通过；缺字段/type 错/越界值逐一拒绝。
    #[test]
    fn t7_sync_payload_parse_and_reject() {
        let ok = SyncPayload::parse(&sync_json()).unwrap();
        assert_eq!(ok.content_id, "blake3:sync");
        assert_eq!(ok.device_id, "dv_peer");
        assert_eq!(ok.updated_at, 1729180800);
        assert!((ok.position_sec - 1935.2).abs() < 1e-9);

        let mut bad = sync_json();
        bad["type"] = serde_json::json!("catalog.push");
        assert!(SyncPayload::parse(&bad).is_err(), "type 错须拒");

        let mut bad = sync_json();
        bad["content_id"] = serde_json::json!("");
        assert!(SyncPayload::parse(&bad).is_err(), "空 content_id 须拒");

        let mut bad = sync_json();
        bad["volume"] = serde_json::json!(null);
        assert!(SyncPayload::parse(&bad).is_err(), "缺 volume 须拒");

        let mut bad = sync_json();
        bad["volume"] = serde_json::json!(1.5);
        assert!(SyncPayload::parse(&bad).is_err(), "volume 越界须拒");

        let mut bad = sync_json();
        bad["position_sec"] = serde_json::json!(-1.0);
        assert!(SyncPayload::parse(&bad).is_err(), "负进度须拒");

        let mut bad = sync_json();
        bad["updated_at"] = serde_json::json!(0);
        assert!(SyncPayload::parse(&bad).is_err(), "updated_at≤0 须拒");
    }

    /// t8：LWW——新行合入；更新的覆盖；旧报 Stale 丢弃；同秒后到者胜。
    #[test]
    fn t8_lww_apply_fresh_newer_stale_tie() {
        let (_d, db) = tmp_db("t8.db");
        let store = Store::open(&db).unwrap();

        // 新行 → Applied，行值来自报文（aid/rate 用 schema 默认）
        let out = store.apply_sync(&sync_payload(1000, "dv_peer", 10.0)).unwrap();
        assert_eq!(out, SyncOutcome::Applied);
        let row = store.load_session("blake3:sync").unwrap();
        assert!((row.position_sec - 10.0).abs() < 1e-9);
        assert_eq!(row.device_id, "dv_peer");
        assert_eq!(row.updated_at, 1000);
        assert_eq!(row.audio_track, 0, "新行默认 aid");
        assert!((row.playback_rate - 1.0).abs() < 1e-9, "新行默认倍速");

        // 更新的 → 覆盖
        let out = store.apply_sync(&sync_payload(2000, "dv_peer", 20.0)).unwrap();
        assert_eq!(out, SyncOutcome::Applied);
        assert!((store.load_session("blake3:sync").unwrap().position_sec - 20.0).abs() < 1e-9);

        // 旧报（严格更小）→ Stale，行不动
        let out = store.apply_sync(&sync_payload(1999, "dv_peer", 99.0)).unwrap();
        assert_eq!(out, SyncOutcome::Stale);
        assert!((store.load_session("blake3:sync").unwrap().position_sec - 20.0).abs() < 1e-9);

        // 同秒并列 → 后到者胜（防先到同秒报文吞掉后到的暂停报文）
        let out = store.apply_sync(&sync_payload(2000, "dv_peer", 21.0)).unwrap();
        assert_eq!(out, SyncOutcome::Applied);
        assert!((store.load_session("blake3:sync").unwrap().position_sec - 21.0).abs() < 1e-9);
    }

    /// t9：自报（device_id == 本机）→ SelfDevice，且不写行。
    #[test]
    fn t9_lww_self_device_rejected() {
        let (_d, db) = tmp_db("t9.db");
        let store = Store::open(&db).unwrap();
        let me = store.device_id().to_string();

        let out = store.apply_sync(&sync_payload(9999, &me, 42.0)).unwrap();
        assert_eq!(out, SyncOutcome::SelfDevice);
        assert!(store.load_session("blake3:sync").is_none(), "自报不应落库");

        // 本机已有行时自报也不动行
        store.upsert_session("blake3:sync", &store.stamp(&sample())).unwrap();
        let before = store.load_session("blake3:sync").unwrap();
        let out = store.apply_sync(&sync_payload(9999, &me, 42.0)).unwrap();
        assert_eq!(out, SyncOutcome::SelfDevice);
        assert_eq!(store.load_session("blake3:sync").unwrap(), before, "自报不改行");
    }

    /// t10：合入保留本机 audio_track/playback_rate（报文不含两列）。
    #[test]
    fn t10_lww_merge_keeps_audio_and_rate() {
        let (_d, db) = tmp_db("t10.db");
        let store = Store::open(&db).unwrap();
        let local = SavedSession { audio_track: 3, playback_rate: 1.5, ..sample() };
        store.upsert_session("blake3:sync", &local).unwrap();

        let out = store.apply_sync(&sync_payload(999999, "dv_peer", 55.0)).unwrap();
        assert_eq!(out, SyncOutcome::Applied);
        let row = store.load_session("blake3:sync").unwrap();
        assert!((row.position_sec - 55.0).abs() < 1e-9, "进度来自报文");
        assert_eq!(row.audio_track, 3, "aid 保留本机");
        assert!((row.playback_rate - 1.5).abs() < 1e-9, "倍速保留本机");
        assert_eq!(row.device_id, "dv_peer", "最后更新设备来自报文");
    }
}
