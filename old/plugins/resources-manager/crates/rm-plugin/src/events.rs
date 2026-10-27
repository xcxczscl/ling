//! 事件：目录变动广播与 core-ui 页面贡献（`docs/03` §4）。

use ling_sdk::{ActivateError, Context};

use rm_proto::event::CATALOG_CHANGED;

/// 订阅 kernel.plugin.state：core-ui 若在，刷新无特殊处理（贡献自动撤下由
/// core-ui 自己管）。M1 占位：目录变动事件由方法层直接广播。
pub fn watch_catalog(_ctx: &Context) -> Result<(), ActivateError> {
    Ok(())
}

/// 广播目录变动（通用刷新通知）。
pub fn emit_catalog_changed(ctx: &Context, kind: &str) {
    let _ = ctx.emit(CATALOG_CHANGED, serde_json::json!({"kind": kind}));
}

/// 广播作业进度。
pub fn emit_job_progress(ctx: &Context, job_id: &str, pct: f64, processed: u64) {
    let _ = ctx.emit(
        rm_proto::event::JOB_PROGRESS,
        serde_json::json!({"job_id": job_id, "pct": pct, "processed": processed}),
    );
}

/// 广播作业完成。
pub fn emit_job_done(ctx: &Context, job_id: &str, state: &str, summary: serde_json::Value) {
    let _ = ctx.emit(
        rm_proto::event::JOB_DONE,
        serde_json::json!({"job_id": job_id, "state": state, "summary": summary}),
    );
}

/// 记录同步完成（`docs/03` §4）。
pub fn emit_sync_done(ctx: &Context, peer_id: &str, applied: u64, skipped: u64, conflicts: u64) {
    let _ = ctx.emit(
        rm_proto::event::SYNC_DONE,
        serde_json::json!({
            "peer_id": peer_id, "applied": applied,
            "skipped": skipped, "conflicts": conflicts,
        }),
    );
}

/// 新发现同步冲突（界面徽标）。
pub fn emit_sync_conflict(ctx: &Context, conflict_count: u64) {
    let _ = ctx.emit(
        rm_proto::event::SYNC_CONFLICT,
        serde_json::json!({"conflict_count": conflict_count}),
    );
}

/// 新增 AI 待审建议（`docs/03` §4）。
pub fn emit_ai_suggest_created(ctx: &Context, count: u32) {
    let _ = ctx.emit(
        rm_proto::event::AI_SUGGEST_CREATED,
        serde_json::json!({"count": count}),
    );
}
