//! `rm-sync`：记录同步（`docs/01` §7.5，附录 A-11/A-16）。
//!
//! 纯点对点：每台设备一条只增 op 流水；同步 = 交换"你还没有的那部分"。
//! 本 crate 只做**领域逻辑**（导出、应用合并、墓碑宽限、冲突登记/裁决），
//! 传输（文件桥 M2 / 局域网 M3）由上层（rm-plugin / rmgr）负责。

pub mod conflict;
pub mod merge;
pub mod oplog;
pub mod tombstone;

pub use conflict::{list_pending, pending_count, resolve_conflict};
/// 文件桥/同机双库导入：`apply_ops` 的别名。
pub use merge::apply_ops as import_bundle;
pub use merge::{apply_ops, export_bundle, BundleOp, MergeReport};
pub use oplog::{append_local, gen_op_id};
pub use tombstone::{is_grace_active, purge_expired_tombstones, TOMBSTONE_GRACE_DAYS};

#[allow(dead_code)]
fn err(msg: impl Into<String>) -> rm_proto::BizError {
    rm_proto::BizError::new(rm_proto::codes::F401, msg.into())
}
