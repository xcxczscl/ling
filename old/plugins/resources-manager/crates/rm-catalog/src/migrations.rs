//! 建表与迁移（`docs/02` 数据字典；schema 版本号单调递增）。

use rusqlite::Connection;

/// 当前 schema 版本。M1=1；M2=2：关系 + 同步层（op/peer/sync_conflict）；
/// M3=3：传输任务 + 按需策略（transfer/placement_policy）；
/// M4=4：快照表（snapshot）；M5=5：AI 建议表 + 隐私档位表。
const SCHEMA_VERSION: i64 = 5;

pub fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    let current: i64 = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key='version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if current >= SCHEMA_VERSION {
        return Ok(());
    }

    if current < 1 {
        conn.execute_batch(SCHEMA_V1)?;
    }
    if current < 2 {
        conn.execute_batch(SCHEMA_V2)?;
    }
    if current < 3 {
        conn.execute_batch(SCHEMA_V3)?;
    }
    if current < 4 {
        conn.execute_batch(SCHEMA_V4)?;
    }
    if current < 5 {
        conn.execute_batch(SCHEMA_V5)?;
    }
    conn.execute(
        "INSERT INTO schema_meta(key, value) VALUES('version', ?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// M1 全量表（`docs/02`）。
const SCHEMA_V1: &str = r#"
-- ===== 物理层 =====
CREATE TABLE device (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  platform    TEXT NOT NULL,
  fingerprint TEXT NOT NULL UNIQUE,
  last_seen   INTEGER NOT NULL DEFAULT 0,
  state       TEXT NOT NULL DEFAULT 'online'
);

CREATE TABLE location (
  id         TEXT PRIMARY KEY,
  device_id  TEXT NOT NULL REFERENCES device(id),
  root_path  TEXT NOT NULL,
  kind       TEXT NOT NULL DEFAULT 'local',
  state      TEXT NOT NULL DEFAULT 'online',
  label      TEXT,
  UNIQUE(device_id, root_path)
);

CREATE TABLE content (
  blake3         TEXT PRIMARY KEY,
  asset_type     TEXT NOT NULL DEFAULT 'other',
  size           INTEGER NOT NULL,
  enc            TEXT NOT NULL DEFAULT 'plain',
  enc_size       INTEGER,
  chunking       TEXT NOT NULL DEFAULT 'none',
  copy_count     INTEGER NOT NULL DEFAULT 0,
  verified_level TEXT NOT NULL DEFAULT 'none',
  created        INTEGER NOT NULL
);
CREATE INDEX idx_content_asset ON content(asset_type);

CREATE TABLE placement (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  location_id   TEXT NOT NULL REFERENCES location(id),
  rel_path      TEXT NOT NULL,
  mode          TEXT NOT NULL DEFAULT 'scattered',
  pool_id       TEXT,
  content_id    TEXT NOT NULL REFERENCES content(blake3),
  size          INTEGER NOT NULL,
  mtime         INTEGER NOT NULL,
  inode         INTEGER,
  state         TEXT NOT NULL DEFAULT 'online',
  first_seen    INTEGER NOT NULL,
  last_verified INTEGER,
  pinned        INTEGER NOT NULL DEFAULT 0,
  hash_state    TEXT NOT NULL DEFAULT 'done',
  UNIQUE(location_id, rel_path)
);
CREATE INDEX idx_placement_content ON placement(content_id);
CREATE INDEX idx_placement_loc_mode_state ON placement(location_id, mode, state);
CREATE INDEX idx_placement_mode ON placement(mode);
CREATE INDEX idx_placement_pool ON placement(pool_id);

CREATE TABLE pool (
  id             TEXT PRIMARY KEY,
  location_id    TEXT NOT NULL REFERENCES location(id),
  rel_path       TEXT NOT NULL,
  capacity_hint  INTEGER,
  compression    TEXT NOT NULL DEFAULT 'off',
  last_verified  INTEGER,
  state          TEXT NOT NULL DEFAULT 'online',
  UNIQUE(location_id, rel_path)
);

-- ===== 语义层 =====
CREATE TABLE entry (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL DEFAULT 'work',
  title       TEXT NOT NULL,
  sort_title  TEXT NOT NULL,
  subtitle    TEXT,
  attrs_json  TEXT NOT NULL DEFAULT '{}',
  created     INTEGER NOT NULL,
  updated     INTEGER NOT NULL
);
CREATE INDEX idx_entry_title ON entry(title);

CREATE TABLE entry_alias (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  entry_id TEXT NOT NULL REFERENCES entry(id),
  alias    TEXT NOT NULL,
  UNIQUE(entry_id, alias)
);
CREATE INDEX idx_entry_alias_alias ON entry_alias(alias);

CREATE TABLE entry_ext (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  entry_id    TEXT NOT NULL REFERENCES entry(id),
  source      TEXT NOT NULL,
  external_id TEXT NOT NULL,
  url         TEXT,
  fetched_at  INTEGER,
  UNIQUE(entry_id, source)
);

CREATE TABLE version (
  id          TEXT PRIMARY KEY,
  entry_id    TEXT NOT NULL REFERENCES entry(id),
  title       TEXT NOT NULL,
  ordinal     INTEGER NOT NULL,
  edition     TEXT,
  resolution  INTEGER,
  codec       TEXT,
  language    TEXT,
  released_at INTEGER,
  source      TEXT NOT NULL DEFAULT 'unknown',
  state       TEXT NOT NULL DEFAULT 'draft',
  attrs_json  TEXT NOT NULL DEFAULT '{}',
  created     INTEGER NOT NULL,
  updated     INTEGER NOT NULL,
  UNIQUE(entry_id, ordinal)
);
CREATE INDEX idx_version_attrs ON version(resolution, codec);
CREATE INDEX idx_version_codec ON version(codec);

CREATE TABLE version_file (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  version_id      TEXT NOT NULL REFERENCES version(id),
  rel_path        TEXT NOT NULL,
  content_blake3  TEXT NOT NULL,
  size            INTEGER NOT NULL,
  role            TEXT NOT NULL DEFAULT 'other'
);
CREATE INDEX idx_version_file_content ON version_file(content_blake3);

CREATE TABLE instance (
  id           TEXT PRIMARY KEY,
  version_id   TEXT NOT NULL REFERENCES version(id),
  location_id  TEXT NOT NULL REFERENCES location(id),
  path         TEXT NOT NULL,
  state        TEXT NOT NULL DEFAULT 'unverified',
  matched_by   TEXT NOT NULL DEFAULT 'manual',
  created      INTEGER NOT NULL,
  UNIQUE(version_id, location_id, path)
);

CREATE TABLE tag_domain (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  builtin         INTEGER NOT NULL DEFAULT 0,
  default_subject TEXT NOT NULL DEFAULT 'either'
);

CREATE TABLE tag (
  id        TEXT PRIMARY KEY,
  parent_id TEXT REFERENCES tag(id),
  domain    TEXT NOT NULL REFERENCES tag_domain(id),
  name      TEXT NOT NULL,
  created   INTEGER NOT NULL,
  UNIQUE(parent_id, name)
);

CREATE TABLE tag_alias (
  id      INTEGER PRIMARY KEY AUTOINCREMENT,
  tag_id  TEXT NOT NULL REFERENCES tag(id),
  alias   TEXT NOT NULL
);

CREATE TABLE entry_tag (
  entry_id    TEXT NOT NULL REFERENCES entry(id),
  tag_id      TEXT NOT NULL REFERENCES tag(id),
  source      TEXT NOT NULL DEFAULT 'manual',
  confidence  REAL NOT NULL DEFAULT 1.0,
  applied_by  TEXT,
  created     INTEGER NOT NULL,
  PRIMARY KEY (entry_id, tag_id)
);
CREATE INDEX idx_entry_tag_tag ON entry_tag(tag_id, entry_id);

CREATE TABLE version_tag (
  version_id  TEXT NOT NULL REFERENCES version(id),
  tag_id      TEXT NOT NULL REFERENCES tag(id),
  source      TEXT NOT NULL DEFAULT 'manual',
  confidence  REAL NOT NULL DEFAULT 1.0,
  applied_by  TEXT,
  created     INTEGER NOT NULL,
  PRIMARY KEY (version_id, tag_id)
);
CREATE INDEX idx_version_tag_tag ON version_tag(tag_id, version_id);

-- ===== 辅助表 =====
CREATE TABLE rule (
  id          TEXT PRIMARY KEY,
  priority    INTEGER NOT NULL DEFAULT 100,
  match_json  TEXT NOT NULL,
  action_json TEXT NOT NULL,
  enabled     INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE job (
  id            TEXT PRIMARY KEY,
  kind          TEXT NOT NULL,
  state         TEXT NOT NULL DEFAULT 'running',
  progress_json TEXT NOT NULL DEFAULT '{}',
  log           TEXT NOT NULL DEFAULT '',
  created       INTEGER NOT NULL,
  updated       INTEGER NOT NULL
);

CREATE TABLE store_entry (
  plugin_id TEXT NOT NULL,
  key       TEXT NOT NULL,
  value     TEXT NOT NULL,
  PRIMARY KEY (plugin_id, key)
);
"#;

/// M2 增量：关系 + 同步层（`docs/02` §3.11、§4）。
const SCHEMA_V2: &str = r#"
CREATE TABLE connection (
  id         TEXT PRIMARY KEY,
  from_kind  TEXT NOT NULL,
  from_id    TEXT NOT NULL,
  to_kind    TEXT NOT NULL,
  to_id      TEXT NOT NULL,
  type       TEXT NOT NULL,
  ordinal    INTEGER,
  attrs_json TEXT NOT NULL DEFAULT '{}',
  source     TEXT NOT NULL DEFAULT 'manual',
  confidence REAL NOT NULL DEFAULT 1.0,
  created    INTEGER NOT NULL,
  deleted_at INTEGER,
  UNIQUE(from_kind, from_id, to_kind, to_id, type)
);
CREATE INDEX idx_connection_from ON connection(from_kind, from_id);
CREATE INDEX idx_connection_to ON connection(to_kind, to_id);

CREATE TABLE op (
  seq     INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id TEXT NOT NULL,
  op_id   TEXT NOT NULL UNIQUE,
  kind    TEXT NOT NULL,
  payload TEXT NOT NULL,
  ts      INTEGER NOT NULL
);
CREATE INDEX idx_op_kind ON op(kind, seq);

CREATE TABLE peer (
  device_id   TEXT PRIMARY KEY,
  addr        TEXT,
  fingerprint TEXT NOT NULL,
  known_seq   INTEGER NOT NULL DEFAULT 0,
  last_sync   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE sync_conflict (
  id            TEXT PRIMARY KEY,
  object_kind   TEXT NOT NULL,
  object_id     TEXT NOT NULL,
  field         TEXT NOT NULL,
  my_value      TEXT,
  their_value   TEXT,
  my_device     TEXT NOT NULL,
  their_device  TEXT NOT NULL,
  my_ts         INTEGER NOT NULL,
  their_ts      INTEGER NOT NULL,
  my_source     TEXT,
  their_source  TEXT,
  conflict_kind TEXT NOT NULL,
  state         TEXT NOT NULL DEFAULT 'pending',
  resolution    TEXT,
  resolved_at   INTEGER,
  discovered    INTEGER NOT NULL,
  UNIQUE(object_kind, object_id, field, their_device)
);
CREATE INDEX idx_sync_conflict_state ON sync_conflict(state, discovered);
"#;

/// M3 增量：传输任务 + 按需策略（`docs/02` §4.4、§4.5）。
const SCHEMA_V3: &str = r#"
CREATE TABLE transfer (
  id             TEXT PRIMARY KEY,
  peer_device_id TEXT NOT NULL,
  content_blake3 TEXT NOT NULL,
  target_path    TEXT NOT NULL,
  offset         INTEGER NOT NULL DEFAULT 0,
  total          INTEGER NOT NULL DEFAULT 0,
  state          TEXT NOT NULL DEFAULT 'queued',
  error          TEXT,
  created        INTEGER NOT NULL,
  updated        INTEGER NOT NULL,
  UNIQUE(content_blake3, target_path)
);
CREATE INDEX idx_transfer_state ON transfer(state, updated);

ALTER TABLE peer ADD COLUMN pubkey TEXT;

CREATE TABLE placement_policy (
  id          TEXT PRIMARY KEY,
  scope_kind  TEXT NOT NULL,
  scope_id    TEXT NOT NULL,
  mode        TEXT NOT NULL,
  priority    INTEGER NOT NULL DEFAULT 100,
  created     INTEGER NOT NULL,
  UNIQUE(scope_kind, scope_id)
);
"#;

/// M4 增量：快照表（`docs/02` §6.3）；清单/索引文件落备份目标，不进库。
const SCHEMA_V4: &str = r#"
CREATE TABLE snapshot (
  id            TEXT PRIMARY KEY,
  location_id   TEXT NOT NULL REFERENCES location(id),
  target        TEXT NOT NULL,
  created       INTEGER NOT NULL,
  manifest_hash TEXT NOT NULL,
  stats_json    TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX idx_snapshot_unique ON snapshot(location_id, target, created);
CREATE INDEX idx_snapshot_loc_created ON snapshot(location_id, created);
"#;

/// M5 增量：AI 建议表 + 隐私档位表（`docs/02` §5.1）。
/// 建议表不参与同步（本地可重算草稿）；只有"接受之后"的结果才进 op 流水。
const SCHEMA_V5: &str = r#"
CREATE TABLE ai_suggestion (
  id          TEXT PRIMARY KEY,
  target_kind TEXT NOT NULL,
  target_id   TEXT NOT NULL,
  kind        TEXT NOT NULL,
  payload     TEXT NOT NULL,
  confidence  REAL NOT NULL,
  model       TEXT NOT NULL,
  prompt_hash TEXT NOT NULL,
  input_hash  TEXT NOT NULL,
  state       TEXT NOT NULL DEFAULT 'pending',
  source      TEXT NOT NULL,
  created     INTEGER NOT NULL,
  applied_at  INTEGER
);
CREATE INDEX idx_sugg_state ON ai_suggestion(state, created);
CREATE INDEX idx_sugg_target ON ai_suggestion(target_kind, target_id);
CREATE INDEX idx_sugg_source ON ai_suggestion(source);

CREATE TABLE ai_privacy (
  path_prefix TEXT PRIMARY KEY,
  tier        TEXT NOT NULL
);
"#;
