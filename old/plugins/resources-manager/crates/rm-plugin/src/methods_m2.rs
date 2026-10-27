//! M2 方法：关系、设备配对、记录同步、冲突队列（`docs/03` §2.3）。

use std::sync::Arc;

use ling_sdk::{CallError, Context, Value};

use rm_catalog::repo;
use rm_proto::codes;
use rm_proto::msg;
use rm_sync::merge::{apply_ops, export_bundle, BundleOp};

use crate::events;
use crate::state::{to_call, RmState};

/// 注册 M2 方法。
pub fn register_m2(ctx: &Context, state: &Arc<RmState>) -> Result<(), ling_sdk::ContextError> {
    let reg =
        |name: &str, handler: Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>| {
            ctx.register_method(name, handler, Box::new(|| {}), None)
        };

    // ===== 关系 =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.connection.add",
            Box::new(move |args| {
                let req: msg::ConnectionAddReq = crate::methods::from_args(&args)?;
                let id = s
                    .with_conn(|conn| {
                        rm_semantic::connection_add(
                            conn,
                            &req.from_kind,
                            &req.from_id,
                            &req.to_kind,
                            &req.to_id,
                            &req.ctype,
                            req.ordinal,
                        )
                    })
                    .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "connection");
                Ok(serde_json::json!({"connection_id": id}))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.connection.list",
            Box::new(move |args| {
                let req: msg::ConnectionListReq = crate::methods::from_args(&args)?;
                let from = match (req.from_kind.as_deref(), req.from_id.as_deref()) {
                    (Some(k), Some(id)) => Some((k, id)),
                    _ => None,
                };
                let to = match (req.to_kind.as_deref(), req.to_id.as_deref()) {
                    (Some(k), Some(id)) => Some((k, id)),
                    _ => None,
                };
                let (rows, total) = s
                    .with_conn(|conn| {
                        rm_semantic::connection_list(
                            conn,
                            from,
                            to,
                            req.page.limit(),
                            req.page.offset(),
                        )
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "total": total,
                    "rows": rows.iter().map(|(id, fk, fi, tk, ti, ty, ord, created)| {
                        serde_json::json!({
                            "id": id, "from_kind": fk, "from_id": fi,
                            "to_kind": tk, "to_id": ti, "type": ty,
                            "ordinal": ord, "created": created,
                        })
                    }).collect::<Vec<_>>(),
                }))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.connection.del",
            Box::new(move |args| {
                let req: msg::ConnectionDelReq = crate::methods::from_args(&args)?;
                s.with_conn(|conn| rm_semantic::connection_del(conn, &req.connection_id))
                    .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "connection");
                Ok(serde_json::json!({}))
            }),
        )?;
    }

    // 设备配对（真握手）由 rm-plugin::methods_m3 注册。

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.device.forget",
            Box::new(move |args| {
                let req: msg::DeviceForgetReq = crate::methods::from_args(&args)?;
                s.with_conn(|conn| repo::peer::forget(conn, &req.device_id))
                    .map_err(to_call)?;
                Ok(serde_json::json!({}))
            }),
        )?;
    }

    // ===== 同步 =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.sync.status",
            Box::new(move |_| {
                let (peers, pending_ops, conflict_count, last_sync) = s
                    .with_conn(|conn| {
                        let berr = |e: rusqlite::Error| {
                            rm_proto::BizError::new(codes::F301, e.to_string())
                        };
                        let peers = repo::peer::list(conn).map_err(berr)?;
                        let known = peers.iter().map(|p| p.3).min().unwrap_or(0);
                        let pending = repo::op::pending_for_peer(conn, known).map_err(berr)?;
                        let conflicts = rm_sync::pending_count(conn)?;
                        let last = peers.iter().map(|p| p.4).max().unwrap_or(0);
                        Ok::<_, rm_proto::BizError>((peers, pending, conflicts, last))
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "peers": peers.iter().map(|(id, addr, fp, known, last)| {
                        serde_json::json!({
                            "device_id": id, "addr": addr, "fingerprint": fp,
                            "known_seq": known, "last_sync": last,
                        })
                    }).collect::<Vec<_>>(),
                    "pending_ops": pending_ops,
                    "conflict_count": conflict_count,
                    "last_sync": last_sync,
                }))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.sync.export",
            Box::new(move |args| {
                let req: msg::SyncExportReq = crate::methods::from_args(&args)?;
                let after = match req.peer_id.as_deref() {
                    Some(pid) => s
                        .with_conn(|conn| repo::peer::get(conn, pid))
                        .map_err(to_call)?
                        .map(|p| p.3)
                        .unwrap_or(0),
                    None => 0,
                };
                let ops = s
                    .with_conn(|conn| export_bundle(conn, after, req.limit))
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "ops": ops,
                    "count": ops.len(),
                }))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.sync.import",
            Box::new(move |args| {
                let req: msg::SyncImportReq = crate::methods::from_args(&args)?;
                let self_dev = s.device_id.clone();
                // peer_device：取 ops 里第一个 device_id（M2 文件桥）。
                let peer = req
                    .ops
                    .first()
                    .map(|o| o.device_id.clone())
                    .unwrap_or_else(|| "unknown".into());
                let bundle: Vec<BundleOp> = req
                    .ops
                    .into_iter()
                    .map(|o| BundleOp {
                        seq: o.seq,
                        device_id: o.device_id,
                        op_id: o.op_id,
                        kind: o.kind,
                        payload: o.payload,
                        ts: o.ts,
                    })
                    .collect();
                let report = s
                    .with_conn(|conn| apply_ops(conn, &self_dev, &peer, &bundle))
                    .map_err(to_call)?;
                // 更新 known_seq：对端流水全部见过 → 本机 peer.known_seq 取对端最大 seq。
                let max_seq = bundle.iter().map(|o| o.seq).max().unwrap_or(0);
                if max_seq > 0 {
                    let _ = s.with_conn(|conn| {
                        repo::peer::set_progress(conn, &peer, max_seq, rm_catalog::now())
                    });
                }
                events::emit_sync_done(
                    &ctx2,
                    &peer,
                    report.applied,
                    report.skipped,
                    report.conflicts,
                );
                if report.conflicts > 0 {
                    events::emit_sync_conflict(&ctx2, report.conflicts);
                }
                Ok(serde_json::json!({
                    "applied": report.applied,
                    "skipped": report.skipped,
                    "conflicts": report.conflicts,
                    "errors": report.errors,
                }))
            }),
        )?;
    }
    // sync.run：M2 文件桥占位已由 M3 LAN 实现覆盖（methods_m3 注册真握手版；
    // 无 addr 对端仍返回 bridge 提示——见 methods_m3::register_m3）。

    // ===== 冲突队列 =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.sync.conflict.list",
            Box::new(move |args| {
                let req: msg::SyncConflictListReq = crate::methods::from_args(&args)?;
                let (rows, total) = s
                    .with_conn(|conn| {
                        rm_sync::list_pending(
                            conn,
                            req.state.as_deref(),
                            req.page.limit(),
                            req.page.offset(),
                        )
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "total": total,
                    "rows": rows.iter().map(|r| {
                        serde_json::json!({
                            "id": r.0, "object_kind": r.1, "object_id": r.2,
                            "field": r.3,
                            "my_value": r.4.as_ref().and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok()),
                            "their_value": r.5.as_ref().and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok()),
                            "my_device": r.6, "their_device": r.7,
                            "my_ts": r.8, "their_ts": r.9,
                            "conflict_kind": r.10, "state": r.11,
                        })
                    }).collect::<Vec<_>>(),
                }))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.sync.conflict.resolve",
            Box::new(move |args| {
                let req: msg::SyncConflictResolveReq = crate::methods::from_args(&args)?;
                let device = s.device_id.clone();
                s.with_conn(|conn| {
                    rm_sync::resolve_conflict(
                        conn,
                        &device,
                        &req.conflict_id,
                        &req.choice,
                        req.value.clone(),
                    )
                })
                .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "conflict");
                Ok(serde_json::json!({}))
            }),
        )?;
    }

    Ok(())
}
