//! 事件主题常量与载荷（`docs/03` §4；core-ui `/events` 托管推送）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JOB_PROGRESS: &str = "resources-manager.job.progress";
pub const JOB_DONE: &str = "resources-manager.job.done";
pub const SCAN_DONE: &str = "resources-manager.scan.done";
pub const CATALOG_CHANGED: &str = "resources-manager.catalog.changed";
pub const SYNC_DONE: &str = "resources-manager.sync.done";
pub const SYNC_CONFLICT: &str = "resources-manager.sync.conflict";
pub const AI_SUGGEST_CREATED: &str = "resources-manager.ai.suggest.created";

/// 本插件只允许广播 `resources-manager.` 前缀主题（ling 内核强制校验）。
pub fn assert_namespace(topic: &str) -> bool {
    topic.starts_with("resources-manager.")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgress {
    pub job_id: String,
    pub pct: f64,
    pub processed: u64,
    pub total: Option<u64>,
    pub current_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobDone {
    pub job_id: String,
    pub state: String,
    pub summary: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanDone {
    pub location_id: String,
    pub files: u64,
    pub bytes: u64,
    pub dup_groups: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogChanged {
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncDone {
    pub peer_id: String,
    pub applied: u64,
    pub skipped: u64,
    pub conflicts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConflict {
    pub conflict_count: u64,
}
