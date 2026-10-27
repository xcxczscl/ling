//! M1 对外方法的入参/返回结构（`docs/03` §2.3 方法目录的 M1 子集）。

use serde::{Deserialize, Serialize};

use crate::PageReq;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanStartReq {
    pub location_id: String,
    /// 强制重算哈希（缺省走增量：路径/大小/mtime 未变则跳过）。
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobIdOut {
    pub job_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanStatsOut {
    pub files: u64,
    pub bytes: u64,
    pub contents: u64,
    pub dup_groups: u64,
    pub dup_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrowseQueryReq {
    pub tag: Option<String>,
    pub location_id: Option<String>,
    pub path_prefix: Option<String>,
    pub q: Option<String>,
    #[serde(flatten)]
    pub page: PageReq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseRow {
    pub placement_id: i64,
    pub rel_path: String,
    pub location_id: String,
    pub content_id: String,
    pub size: i64,
    pub asset_type: String,
    pub state: String,
    pub mode: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListOut<T> {
    pub rows: Vec<T>,
    pub total: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntryListReq {
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub q: Option<String>,
    #[serde(flatten)]
    pub page: PageReq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub sort_title: String,
    pub subtitle: Option<String>,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryCreateReq {
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub subtitle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdOut {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionAddReq {
    pub entry_id: String,
    pub title: String,
    pub ordinal: i64,
    #[serde(default)]
    pub edition: Option<String>,
    #[serde(default)]
    pub resolution: Option<i64>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionFileItem {
    pub rel_path: String,
    pub content_blake3: String,
    pub size: i64,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "other".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionLinkFilesReq {
    pub version_id: String,
    pub files: Vec<VersionFileItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub domain: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagApplyReq {
    pub target_kind: String,
    pub target_id: String,
    pub tag_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DupListReq {
    pub min_size: Option<i64>,
    #[serde(flatten)]
    pub page: PageReq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupPlacement {
    pub placement_id: i64,
    pub location_id: String,
    pub rel_path: String,
    pub state: String,
    pub pinned: bool,
    pub inode: Option<i64>,
    pub last_verified: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupGroup {
    pub content_id: String,
    pub size: i64,
    pub asset_type: String,
    pub copy_count: i64,
    pub placements: Vec<DupPlacement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupLinkReq {
    pub content_id: String,
    /// 保留哪一份（placement_id）；缺省保留最早 first_seen。
    #[serde(default)]
    pub keep: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupLinkOut {
    pub linked: u64,
    pub freed_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupDeleteReq {
    pub placement_ids: Vec<i64>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupDeleteBlocked {
    pub reason: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupDeleteOut {
    pub deleted: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<DupDeleteBlocked>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_record_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationAddReq {
    pub path: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationSetStateReq {
    pub location_id: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationRow {
    pub id: String,
    pub device_id: String,
    pub root_path: String,
    pub kind: String,
    pub state: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub state: String,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRow {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub progress: serde_json::Value,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLogOut {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClassifyRule {
    pub id: String,
    pub priority: i64,
    /// JSON 键名 `match`（`docs/02` §6.1）；Rust 侧避开关键字。
    #[serde(rename = "match")]
    pub match_spec: serde_json::Value,
    pub action: serde_json::Value,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifyRulesReq {
    /// 缺省 = 列表；带字段 = 写入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<ClassifyRule>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifyRunReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyRunReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_id: Option<String>,
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceRegisterReq {
    pub version_id: String,
    pub location_id: String,
    pub path: String,
}

// ===== M2：关系 / 设备配对 / 记录同步 =====

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionAddReq {
    pub from_kind: String,
    pub from_id: String,
    pub to_kind: String,
    pub to_id: String,
    #[serde(rename = "type")]
    pub ctype: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionListReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_id: Option<String>,
    #[serde(default)]
    pub page: PageReq,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionDelReq {
    pub connection_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DevicePairReq {
    /// 8 位配对码（由对端 `device.pairing_code` 生成；M3 真握手）。
    pub code: String,
    /// 对端 LAN 传输面地址，如 `192.168.1.23:7912`；缺省时用 peer 表里已登记的 addr。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DevicePairingCodeOut {
    pub code: String,
    pub expires_in_sec: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DevicePairOut {
    pub device_id: String,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceForgetReq {
    pub device_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncRunReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// M2 文件桥：从该路径导入对端流水（绝对路径或相对 cwd）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_path: Option<String>,
    /// M2 文件桥：导出到该路径后再导入对端。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncExportReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    #[serde(default = "default_sync_limit")]
    pub limit: u32,
}

fn default_sync_limit() -> u32 {
    1000
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncImportReq {
    /// 操作流水数组（`docs/03` §5.1 第 2 步的载荷）。
    pub ops: Vec<SyncOpMsg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOpMsg {
    pub seq: i64,
    pub device_id: String,
    pub op_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub ts: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncConflictListReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default)]
    pub page: PageReq,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncConflictResolveReq {
    pub conflict_id: String,
    /// keep_mine / keep_theirs / manual / restore_delete
    pub choice: String,
    /// choice=manual 时的值
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOpExport {
    pub seq: i64,
    pub device_id: String,
    pub op_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub ts: i64,
}

// ===== M3 局域网传输（docs/03 §2.3、§5.2）=====

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferFetchReq {
    /// 对端设备编号；与 `addr` 二选一（都不给则报错）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// 对端 LAN 传输面地址；缺省时用 peer 表登记的 addr。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// 要取回的内容指纹（64 位十六进制 BLAKE3）。
    pub content_blake3: String,
    /// 本机落地路径；缺省为 `<data_dir>/incoming/<blake3>`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferStatusReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default)]
    pub page: PageReq,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferRow {
    pub id: String,
    pub peer_device_id: String,
    pub content_blake3: String,
    pub target_path: String,
    pub offset: i64,
    pub total: i64,
    /// queued / running / done / failed / cancelled（docs/02 §7.6）
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub updated: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferCancelReq {
    pub job_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferEvictReq {
    pub placement_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferEvictOut {
    pub evicted: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferPolicySetReq {
    /// path_prefix / tag
    pub scope_kind: String,
    /// 路径前缀或标签编号
    pub scope_id: String,
    /// local_only / mirror / evictable / record_only（docs/01 §7.6、02 §7.7）
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferPolicyRow {
    pub id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub mode: String,
    pub priority: i64,
}

// ===== M4：备份与压缩 =====

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupRunReq {
    pub location_id: String,
    pub target: String,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupVerifyReq {
    pub target: String,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupRestoreReq {
    pub target: String,
    pub snapshot_id: String,
    pub out_dir: String,
    #[serde(default)]
    pub contents: Option<Vec<String>>,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotListReq {
    #[serde(default)]
    pub location_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotRow {
    pub id: String,
    pub location_id: String,
    pub target: String,
    pub created: i64,
    pub manifest_hash: String,
    pub stats_json: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompressSampleReq {
    pub placement_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompressSampleRow {
    pub placement_id: i64,
    pub blake3: String,
    pub sample_in: u64,
    pub sample_out: u64,
    pub worth: bool,
    pub ratio: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompressRunReq {
    pub placement_ids: Vec<i64>,
    /// auto / always / off
    pub mode: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolIngestReq {
    pub placement_ids: Vec<i64>,
    #[serde(default)]
    pub pool_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolMoveReq {
    pub from_pool: String,
    #[serde(default)]
    pub to_pool: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolRemoveReq {
    pub pool_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolRowOut {
    pub id: String,
    pub location_id: String,
    pub rel_path: String,
    pub capacity_hint: Option<i64>,
    pub compression: String,
    pub last_verified: Option<i64>,
    pub state: String,
}

// ===== M5：AI 建议（`docs/03` §2.3 / §5.4） =====

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuggestListReq {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuggestRow {
    pub id: String,
    pub target_kind: String,
    pub target_id: String,
    pub kind: String,
    pub payload: String,
    pub confidence: f64,
    pub model: String,
    pub state: String,
    pub source: String,
    pub created: i64,
    #[serde(default)]
    pub applied_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuggestApplyReq {
    pub suggestion_ids: Vec<String>,
    /// 可选：接受前修改 payload（JSON 字符串）。
    #[serde(default)]
    pub patch: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuggestRejectReq {
    pub suggestion_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuggestRevokeReq {
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiRunReq {
    /// 目标对象：`entry` / `version` / `metadata`（`metadata` 时用 target_id=路径前缀）
    pub target_kind: String,
    pub target_id: String,
    /// 预算上限（模型调用次数；耗尽即 F703）。
    #[serde(default)]
    pub budget: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiPrivacySetReq {
    pub path_prefix: String,
    /// local / cloud / hybrid
    pub tier: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiPrivacyRow {
    pub path_prefix: String,
    pub tier: String,
}

/// `stream.open`（03 §2.6.1）：placement_id 兼容 JSON 数字与字符串。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamOpenReq {
    pub placement_id: serde_json::Value,
    /// 可选审计标签：play / render / copy。
    #[serde(default)]
    pub purpose: Option<String>,
}

/// `get_local_path`（03 §2.6.4）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetLocalPathReq {
    pub placement_id: serde_json::Value,
}
