//! 插件运行时状态：SQLite 连接（单写者纪律 → Mutex）与取消令牌表。
//! M3 起持有 LAN 传输面身份、配对码状态与服务端句柄。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use rm_transfer::{Identity, PairingState, ServerHandle};

/// 共享状态。
pub struct RmState {
    pub conn: Mutex<rusqlite::Connection>,
    pub device_id: String,
    pub data_dir: PathBuf,
    /// job_id → cancel 令牌。
    pub cancel_flags: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    /// 本机 Ed25519 身份（`identity.key`）。
    pub identity: Arc<Identity>,
    /// 配对码状态（服务端内存态）。
    pub pairing: Arc<Mutex<PairingState>>,
    /// LAN 传输面句柄；deactivate 时 Drop 停止监听。
    pub server: Mutex<Option<ServerHandle>>,
    /// 流交付服务句柄（`docs/03` §2.6）；deactivate 时 Drop 停止监听。
    pub stream: Mutex<Option<crate::stream::StreamHandle>>,
}

impl RmState {
    /// 在锁内执行查询并折叠 `Result<T, E>`（E: Display）；锁中毒/查询失败 → BizError。
    pub fn with_conn<T, E: std::fmt::Display>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, E>,
    ) -> Result<T, rm_proto::BizError> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| rm_proto::BizError::new(rm_proto::codes::F301, "目录库锁中毒"))?;
        f(&guard).map_err(|e| rm_proto::BizError::new(rm_proto::codes::F301, e.to_string()))
    }

    /// 在锁内执行可变操作（事务由调用方开）。
    pub fn with_conn_mut<T>(
        &self,
        f: impl FnOnce(&mut rusqlite::Connection) -> T,
    ) -> Result<T, rm_proto::BizError> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| rm_proto::BizError::new(rm_proto::codes::F301, "目录库锁中毒"))?;
        Ok(f(&mut guard))
    }

    /// 新作业 id。
    pub fn new_job_id(&self, kind: &str) -> String {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("jb_{kind}_{:x}{n:08x}", rm_catalog::now() as u64)
    }
}

/// 把 BizError 转成 ling CallError。
pub fn to_call(e: rm_proto::BizError) -> ling_sdk::CallError {
    ling_sdk::CallError::new(&e.code, e.message)
}

/// 把 TransferError 转成 ling CallError。
pub fn transfer_to_call(e: rm_transfer::TransferError) -> ling_sdk::CallError {
    ling_sdk::CallError::new(&e.code, e.message)
}
