//! M3 方法：局域网配对、按需传输、驱逐、策略、LAN 记录同步（`docs/03` §2.3）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ling_sdk::{CallError, Context, Value};

use rm_catalog::repo;
use rm_proto::codes;
use rm_proto::msg;
use rm_sync::merge::{apply_ops, export_bundle, BundleOp};
use rm_transfer::client::{fetch, FetchTask};
use rm_transfer::op_client::{pull_ops, push_ops};
use rm_transfer::pairing::pair;
use rm_transfer::policy as tpolicy;
use rm_transfer::server::ServerCatalog;

use crate::events;
use crate::state::{to_call, RmState};

/// 注册 M3 方法（含 LAN 传输面已在 activate 起动）。
pub fn register_m3(ctx: &Context, state: &Arc<RmState>) -> Result<(), ling_sdk::ContextError> {
    let reg =
        |name: &str, handler: Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>| {
            ctx.register_method(name, handler, Box::new(|| {}), None)
        };

    // ===== 设备配对 =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.device.pairing_code",
            Box::new(move |_| {
                let (code, ttl) = s
                    .pairing
                    .lock()
                    .map_err(|_| CallError::new(codes::F301, "配对状态锁中毒"))?
                    .issue();
                Ok(serde_json::json!({
                    "code": code,
                    "expires_in_sec": ttl,
                }))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.device.pair",
            Box::new(move |args| {
                let req: msg::DevicePairReq = crate::methods::from_args(&args)?;
                let addr = req
                    .addr
                    .clone()
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| {
                        CallError::new(
                            codes::F404,
                            "device.pair 需要 addr（对端 LAN 传输面地址）",
                        )
                    })?;
                // 客户端侧 pin：若 peer 表已有同 addr 记录，握手时核对指纹（F402）。
                let pinned = s
                    .with_conn(|conn| {
                        repo::peer::list(conn)
                            .map_err(|e| e.to_string())?
                            .into_iter()
                            .find(|p| p.1.as_deref() == Some(addr.as_str()))
                            .map(|p| p.2)
                            .ok_or_else(|| "无".to_string())
                    })
                    .ok();
                let my_dev = s.device_id.clone();
                let my_name = crate::methods::hostname();
                let my_plat = std::env::consts::OS.to_string();
                let identity = Arc::clone(&s.identity);
                let peer = pair(
                    &addr,
                    &req.code,
                    identity.as_ref(),
                    &my_dev,
                    &my_name,
                    &my_plat,
                    pinned.as_deref(),
                )
                .map_err(|e| CallError::new(&e.code, e.message))?;
                // 写 peer + device。
                let now = rm_catalog::now();
                s.with_conn(|conn| {
                    repo::peer::upsert(
                        conn,
                        &peer.device_id,
                        Some(&addr),
                        &peer.fingerprint,
                        Some(&peer.pubkey_hex),
                        now,
                    )
                    .map_err(|e| format!("peer 登记失败：{e}"))?;
                    repo::placement::device_upsert(
                        conn,
                        &peer.device_id,
                        &peer.name,
                        &peer.platform,
                        &peer.fingerprint,
                        now,
                    )
                    .map_err(|e| format!("device 登记失败：{e}"))
                })
                .map_err(|e| CallError::new(codes::F301, e.to_string()))?;
                events::emit_catalog_changed(&ctx2, "device");
                Ok(serde_json::json!({
                    "device_id": peer.device_id,
                    "fingerprint": peer.fingerprint,
                    "addr": addr,
                }))
            }),
        )?;
    }

    // ===== 按需传输 =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.transfer.fetch",
            Box::new(move |args| {
                let req: msg::TransferFetchReq = crate::methods::from_args(&args)?;
                let peer_id = req.peer_id.clone().unwrap_or_default();
                let addr = match (&req.addr, &peer_id) {
                    (Some(a), _) if !a.is_empty() => a.clone(),
                    (_, pid) if !pid.is_empty() => s
                        .with_conn(|conn| repo::peer::get(conn, pid))
                        .map_err(to_call)?
                        .and_then(|p| p.1)
                        .ok_or_else(|| {
                            CallError::new(codes::F404, format!("对端 {pid} 未登记 addr"))
                        })?,
                    _ => {
                        return Err(CallError::new(
                            codes::F301,
                            "transfer.fetch 需要 peer_id 或 addr",
                        ));
                    }
                };
                let peer_key = if peer_id.is_empty() {
                    addr.clone()
                } else {
                    peer_id.clone()
                };
                // 内容尺寸来自本机 content 表（记录已同步）。
                let total = s
                    .with_conn(|conn| {
                        repo::content::get(conn, &req.content_blake3)
                            .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?
                    .ok_or_else(|| {
                        CallError::new(codes::F301, "内容记录不存在（先同步记录）")
                    })?
                    .0;
                let target = match &req.target_path {
                    Some(p) => PathBuf::from(p),
                    None => s.data_dir.join("incoming").join(&req.content_blake3),
                };
                let target_s = target.display().to_string();
                // 策略守卫：对 target 路径 + 内容求值；无匹配默认 local_only → F403。
                let mode = s
                    .with_conn(|conn| {
                        repo::policy::eval_for_placement(conn, 0, &target_s, &req.content_blake3)
                            .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                if let Err(e) = tpolicy::guard_fetch(&mode) {
                    return Err(CallError::new(&e.code, e.message));
                }
                // 幂等登记任务（续传不重置 offset）。
                let (job_id, offset, state_name) = s
                    .with_conn(|conn| {
                        repo::transfer::upsert(conn, &peer_key, &req.content_blake3, &target_s, rm_catalog::now())
                            .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                if state_name == "done" && target.is_file() {
                    return Ok(serde_json::json!({"job_id": job_id, "state": "done"}));
                }
                // 取消令牌。
                let cancel = Arc::new(AtomicBool::new(false));
                {
                    let mut flags = s.cancel_flags.lock().unwrap();
                    flags.insert(job_id.clone(), Arc::clone(&cancel));
                }
                let total_u = total.max(0) as u64;
                // 后台拉取。
                let s_bg = Arc::clone(&s);
                let ctx_bg = ctx2.clone();
                let job_id_bg = job_id.clone();
                let identity = Arc::clone(&s.identity);
                let device_id = s.device_id.clone();
                let hash = req.content_blake3.clone();
                let addr_bg = addr.clone();
                let peer_dev = peer_key.clone();
                let start = offset.max(0) as u64;
                let target_bg = target.clone();
                events::emit_job_progress(&ctx_bg, &job_id_bg, 0.0, 0);
                std::thread::Builder::new()
                    .name(format!("rm-fetch-{job_id_bg}"))
                    .spawn(move || {
                        let progress_job = job_id_bg.clone();
                        let s_prog = Arc::clone(&s_bg);
                        let ctx_prog = ctx_bg.clone();
                        let on_progress = Arc::new(move |off: u64| {
                            let _ = s_prog.with_conn(|conn| {
                                repo::transfer::set_progress(
                                    conn,
                                    &progress_job,
                                    off as i64,
                                    total_u as i64,
                                    rm_catalog::now(),
                                )
                                .map_err(|e| e.to_string())
                            });
                            let pct = if total_u > 0 {
                                off as f64 / total_u as f64
                            } else {
                                1.0
                            };
                            events::emit_job_progress(&ctx_prog, &progress_job, pct, off);
                        });
                        let task = FetchTask {
                            addr: addr_bg,
                            identity,
                            device_id,
                            content_blake3: hash,
                            target_path: target_bg,
                            total: total_u,
                            start_offset: start,
                            on_progress,
                        };
                        let result = fetch(&task);
                        let now = rm_catalog::now();
                        match result {
                            Ok(_) => {
                                if cancel.load(Ordering::SeqCst) {
                                    let _ = s_bg.with_conn(|conn| {
                                        repo::transfer::set_state(
                                            conn, &job_id_bg, "cancelled", None, now,
                                        )
                                        .map_err(|e| e.to_string())
                                    });
                                    return;
                                }
                                // 登记 placement。
                                let reg = s_bg.with_conn(|conn| {
                                    register_fetched_placement(
                                        conn,
                                        &s_bg.device_id,
                                        &s_bg.data_dir,
                                        &task.content_blake3,
                                        task.total as i64,
                                        &task.target_path,
                                    )
                                });
                                match reg {
                                    Ok(()) => {
                                        let _ = s_bg.with_conn(|conn| {
                                            repo::transfer::set_state(
                                                conn, &job_id_bg, "done", None, now,
                                            )
                                            .map_err(|e| e.to_string())
                                        });
                                        events::emit_job_done(
                                            &ctx_bg,
                                            &job_id_bg,
                                            "done",
                                            serde_json::json!({
                                                "target_path": task.target_path.display().to_string(),
                                            }),
                                        );
                                    }
                                    Err(e) => {
                                        let _ = s_bg.with_conn(|conn| {
                                            repo::transfer::set_state(
                                                conn, &job_id_bg, "failed", Some(&e.to_string()), now,
                                            )
                                            .map_err(|e| e.to_string())
                                        });
                                        events::emit_job_done(
                                            &ctx_bg,
                                            &job_id_bg,
                                            "failed",
                                            serde_json::json!({"error": e.to_string()}),
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                let msg = format!("{}: {}", e.code, e.message);
                                let _ = s_bg.with_conn(|conn| {
                                    repo::transfer::set_state(
                                        conn, &job_id_bg, "failed", Some(&msg), now,
                                    )
                                    .map_err(|e| e.to_string())
                                });
                                events::emit_job_done(
                                    &ctx_bg,
                                    &job_id_bg,
                                    "failed",
                                    serde_json::json!({"error": msg}),
                                );
                            }
                        }
                        let mut flags = s_bg.cancel_flags.lock().unwrap();
                        flags.remove(&job_id_bg);
                    })
                    .map_err(|e| CallError::new(codes::F301, format!("启动拉取线程失败：{e}")))?;
                let _ = peer_dev;
                Ok(serde_json::json!({"job_id": job_id}))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.transfer.status",
            Box::new(move |args| {
                let req: msg::TransferStatusReq = crate::methods::from_args(&args)?;
                let (rows, total) = s
                    .with_conn(|conn| {
                        repo::transfer::list(
                            conn,
                            req.job_id.as_deref(),
                            req.page.limit(),
                            req.page.offset(),
                        )
                        .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "total": total,
                    "rows": rows.iter().map(|(id, peer, hash, path, off, tot, st, err, upd)| {
                        serde_json::json!({
                            "id": id, "peer_device_id": peer, "content_blake3": hash,
                            "target_path": path, "offset": off, "total": tot,
                            "state": st, "error": err, "updated": upd,
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
            "resources-manager.transfer.cancel",
            Box::new(move |args| {
                let req: msg::TransferCancelReq = crate::methods::from_args(&args)?;
                {
                    let flags = s.cancel_flags.lock().unwrap();
                    if let Some(f) = flags.get(&req.job_id) {
                        f.store(true, Ordering::SeqCst);
                    }
                }
                s.with_conn(|conn| {
                    repo::transfer::set_state(
                        conn,
                        &req.job_id,
                        "cancelled",
                        None,
                        rm_catalog::now(),
                    )
                    .map_err(|e| e.to_string())
                })
                .map_err(to_call)?;
                let _ = &ctx2;
                Ok(serde_json::json!({}))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.transfer.evict",
            Box::new(move |args| {
                let req: msg::TransferEvictReq = crate::methods::from_args(&args)?;
                let (evicted, blocked) = s
                    .with_conn(|conn| {
                        evict_placements(conn, &req.placement_ids)
                    })
                    .map_err(to_call)?;
                if evicted > 0 {
                    events::emit_catalog_changed(&ctx2, "placement");
                }
                let mut out = serde_json::json!({"evicted": evicted});
                if !blocked.is_empty() {
                    out["blocked"] = serde_json::json!(blocked);
                }
                Ok(out)
            }),
        )?;
    }

    // ===== 按需策略 =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.transfer.policy.set",
            Box::new(move |args| {
                let req: msg::TransferPolicySetReq = crate::methods::from_args(&args)?;
                if repo::policy::parse_mode(&req.mode).is_none() {
                    return Err(CallError::new(codes::F301, format!("非法策略档：{}", req.mode)));
                }
                if repo::policy::parse_scope_kind(&req.scope_kind).is_none() {
                    return Err(CallError::new(
                        codes::F301,
                        format!("非法 scope_kind：{}", req.scope_kind),
                    ));
                }
                let id = s
                    .with_conn(|conn| {
                        repo::policy::set(
                            conn,
                            &req.scope_kind,
                            &req.scope_id,
                            &req.mode,
                            req.priority.unwrap_or(0),
                            rm_catalog::now(),
                        )
                        .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "policy");
                Ok(serde_json::json!({"policy_id": id}))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.transfer.policy.list",
            Box::new(move |_| {
                let rows = s
                    .with_conn(|conn| {
                        repo::policy::list(conn).map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.iter().map(|(id, sk, sid, mode, prio)| {
                        serde_json::json!({
                            "id": id, "scope_kind": sk, "scope_id": sid,
                            "mode": mode, "priority": prio,
                        })
                    }).collect::<Vec<_>>(),
                }))
            }),
        )?;
    }

    // ===== 同步（M3：有 addr 走局域网）=====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.sync.run",
            Box::new(move |args| {
                let req: msg::SyncRunReq = crate::methods::from_args(&args)?;
                // 选定对端：peer_id 指定或取第一个有 addr 的。
                let peer = match &req.peer_id {
                    Some(pid) => s
                        .with_conn(|conn| repo::peer::get(conn, pid))
                        .map_err(to_call)?
                        .ok_or_else(|| CallError::new(codes::F301, format!("对端不存在：{pid}")))?,
                    None => s
                        .with_conn(|conn| {
                            repo::peer::list(conn)
                                .map_err(|e| e.to_string())?
                                .into_iter()
                                .find(|p| p.1.as_deref().is_some_and(|a| !a.is_empty()))
                                .ok_or_else(|| "无带 addr 的对端".to_string())
                        })
                        .map_err(to_call)?,
                };
                let peer_id = peer.0.clone();
                let Some(addr) = peer.1.clone().filter(|a| !a.is_empty()) else {
                    // 无 addr：M2 文件桥提示。
                    return Ok(serde_json::json!({
                        "bridge": "export_import",
                        "hint": "对端无 LAN addr，使用 sync.export / sync.import 或 rmgr sync export/import",
                    }));
                };
                let known = peer.3;
                let identity = Arc::clone(&s.identity);
                let my_dev = s.device_id.clone();
                // 拉对端流水 → 本地合并。
                let remote_ops = pull_ops(&addr, &identity, &my_dev, known, 1000)
                    .map_err(|e| CallError::new(&e.code, e.message))?;
                let peer_dev_remote = remote_ops
                    .first()
                    .and_then(|o| o["device_id"].as_str())
                    .unwrap_or(&peer_id)
                    .to_string();
                let bundle: Vec<BundleOp> = remote_ops
                    .iter()
                    .filter_map(|v| serde_json::from_value::<BundleOp>(v.clone()).ok())
                    .collect();
                let max_remote = bundle.iter().map(|o| o.seq).max().unwrap_or(0);
                let report = s
                    .with_conn(|conn| apply_ops(conn, &my_dev, &peer_dev_remote, &bundle))
                    .map_err(to_call)?;
                // 推本地流水。
                let local_ops = s
                    .with_conn(|conn| export_bundle(conn, known, 1000))
                    .map_err(to_call)?;
                let local_vals: Vec<serde_json::Value> = local_ops
                    .iter()
                    .map(|o| serde_json::to_value(o).unwrap_or_default())
                    .collect();
                let import_report = push_ops(&addr, &identity, &my_dev, &local_vals)
                    .map_err(|e| CallError::new(&e.code, e.message))?;
                let max_local = local_ops.iter().map(|o| o.seq).max().unwrap_or(0);
                let progress = max_remote.max(max_local).max(known);
                let now = rm_catalog::now();
                let _ = s.with_conn(|conn| {
                    repo::peer::set_progress(conn, &peer_id, progress, now)
                });
                let applied = report.applied + import_report.applied;
                let skipped = report.skipped + import_report.skipped;
                let conflicts = report.conflicts + import_report.conflicts;
                events::emit_sync_done(&ctx2, &peer_id, applied, skipped, conflicts);
                if conflicts > 0 {
                    events::emit_sync_conflict(&ctx2, conflicts);
                }
                Ok(serde_json::json!({
                    "transport": "lan",
                    "peer_id": peer_id,
                    "applied": applied,
                    "skipped": skipped,
                    "conflicts": conflicts,
                    "pushed": local_vals.len(),
                }))
            }),
        )?;
    }

    Ok(())
}

/// 已登记 peer 中按 addr 匹配的 pinned 指纹（首次配对无）。
// pin 逻辑已并入 device.pair 处理器。

/// fetch 成功后登记 placement：确保覆盖 target 的 location 存在，再 insert 放置。
fn register_fetched_placement(
    conn: &rusqlite::Connection,
    device_id: &str,
    data_dir: &Path,
    blake3: &str,
    size: i64,
    target: &Path,
) -> Result<(), String> {
    let target_s = target.display().to_string();
    // 已有放置则跳过。
    let locations = repo::placement::location_list(conn).map_err(|e| e.to_string())?;
    let mut matched: Option<(String, String)> = None; // (location_id, rel_path)
    for (id, root, kind, state, _) in &locations {
        if state != "online" {
            continue;
        }
        let root_c = root.trim_end_matches('/');
        if target_s.starts_with(root_c) {
            let rel = target_s
                .trim_start_matches(root_c)
                .trim_start_matches('/')
                .to_string();
            matched = Some((id.clone(), rel));
            break;
        }
        let _ = kind;
    }
    let (loc_id, rel) = match matched {
        Some(m) => m,
        None => {
            // 默认收件 location：data_dir/incoming。
            let root = data_dir.join("incoming");
            std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            let loc = "lc_incoming";
            repo::placement::location_add(conn, loc, device_id, &root.display().to_string(), "local", Some("incoming"))
                .map_err(|e| e.to_string())?;
            let root_s = root.display().to_string();
            let rel = target_s
                .trim_start_matches(root_s.trim_end_matches('/'))
                .trim_start_matches('/')
                .to_string();
            (loc.to_string(), rel)
        }
    };
    if repo::placement::placement_find(conn, &loc_id, &rel)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(());
    }
    let mtime = std::fs::metadata(target)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or_else(rm_catalog::now);
    let inode = rm_fs::platform::inode_of_path(target);
    repo::content::insert_if_absent(conn, blake3, size, "blob", rm_catalog::now())
        .map_err(|e| e.to_string())?;
    repo::placement::placement_add(
        conn, &loc_id, &rel, blake3, size, mtime, inode, "online", rm_catalog::now(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 驱逐放置：仅 `evictable` 且有其他在线副本时删本体留记录。
fn evict_placements(
    conn: &rusqlite::Connection,
    ids: &[i64],
) -> Result<(u64, Vec<serde_json::Value>), rm_proto::BizError> {
    let mut evicted = 0u64;
    let mut blocked = Vec::new();
    for &id in ids {
        let row = repo::placement::placement_get(conn, id)
            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
        let Some(p) = row else {
            blocked.push(serde_json::json!({
                "placement_id": id, "reason": "放置不存在",
            }));
            continue;
        };
        // (id, location_id, rel_path, content_id, size, ...)
        let location_id = p.1.clone();
        let rel_path = p.2.clone();
        let content_id = p.3.clone();
        let loc = repo::placement::location_get(conn, &location_id)
            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
        let Some((_, root, _, loc_state, _)) = loc else {
            blocked.push(serde_json::json!({
                "placement_id": id, "reason": "位置不存在",
            }));
            continue;
        };
        let abs = format!("{}/{}", root.trim_end_matches('/'), rel_path);
        let mode = repo::policy::eval_for_placement(conn, id, &abs, &content_id)
            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
        if mode != "evictable" {
            blocked.push(serde_json::json!({
                "placement_id": id, "reason": format!("策略 {mode} 不允许驱逐"),
            }));
            continue;
        }
        if loc_state == "cold" {
            blocked.push(serde_json::json!({
                "placement_id": id, "reason": "冷位置不参与驱逐",
            }));
            continue;
        }
        // 冗余副本检查：同内容其他 online 放置。
        let others = repo::placement::placements_of_content(conn, &content_id)
            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
        let other_online = others
            .iter()
            .any(|(oid, _, _, st, _, _, _)| *oid != id && st == "online");
        if !other_online {
            blocked.push(serde_json::json!({
                "placement_id": id, "reason": "无其他在线副本，拒绝驱逐",
            }));
            continue;
        }
        // 删本体 + 删 placement 行（copy_count 由 placement_delete 维护）。
        if abs.starts_with('/') && std::path::Path::new(&abs).is_file() {
            let _ = std::fs::remove_file(&abs);
        }
        repo::placement::placement_delete(conn, id)
            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
        evicted += 1;
    }
    Ok((evicted, blocked))
}

/// 服务端目录适配：把 RmState 暴露成 `ServerCatalog`。
pub struct CatalogForServer(pub Arc<RmState>);

impl ServerCatalog for CatalogForServer {
    fn peer_fingerprint(&self, device_id: &str) -> Option<String> {
        let conn = self.0.conn.lock().ok()?;
        conn.query_row(
            "SELECT fingerprint FROM peer WHERE device_id=?1",
            rusqlite::params![device_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
    }

    fn peer_pubkey(&self, device_id: &str) -> Option<String> {
        let conn = self.0.conn.lock().ok()?;
        repo::peer::pubkey(&conn, device_id).ok().flatten()
    }

    fn upsert_peer(
        &self,
        device_id: &str,
        name: &str,
        platform: &str,
        fingerprint: &str,
        pubkey_hex: &str,
        addr: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.0.conn.lock().map_err(|_| "目录库锁中毒".to_string())?;
        let now = rm_catalog::now();
        repo::peer::upsert(&conn, device_id, addr, fingerprint, Some(pubkey_hex), now)
            .map_err(|e| e.to_string())?;
        repo::placement::device_upsert(&conn, device_id, name, platform, fingerprint, now)
            .map_err(|e| e.to_string())
    }

    fn serving_paths(&self, content_blake3: &str) -> Result<Vec<(i64, String, i64)>, String> {
        let conn = self.0.conn.lock().map_err(|_| "目录库锁中毒".to_string())?;
        let paths = repo::placement::serving_paths_for_content(
            &conn,
            &self.0.device_id,
            content_blake3,
        )
        .map_err(|e| e.to_string())?;
        // 再过一次策略：record_only 不作数据源。
        let mut out = Vec::new();
        for (pid, path, size) in paths {
            let mode = repo::policy::eval_for_placement(&conn, pid, &path, content_blake3)
                .map_err(|e| e.to_string())?;
            if tpolicy::allows_serving(&mode) {
                out.push((pid, path, size));
            }
        }
        Ok(out)
    }

    fn export_ops(&self, after: i64, limit: u32) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.0.conn.lock().map_err(|_| "目录库锁中毒".to_string())?;
        let ops = export_bundle(&conn, after, limit).map_err(|e| e.to_string())?;
        Ok(ops
            .iter()
            .map(|o| serde_json::to_value(o).unwrap_or_default())
            .collect())
    }

    fn import_ops(
        &self,
        peer_device: &str,
        ops: Vec<serde_json::Value>,
    ) -> Result<(usize, usize, usize), String> {
        let conn = self.0.conn.lock().map_err(|_| "目录库锁中毒".to_string())?;
        let bundle: Vec<BundleOp> = ops
            .into_iter()
            .filter_map(|v| serde_json::from_value::<BundleOp>(v).ok())
            .collect();
        let report =
            apply_ops(&conn, &self.0.device_id, peer_device, &bundle).map_err(|e| e.to_string())?;
        let max_seq = bundle.iter().map(|o| o.seq).max().unwrap_or(0);
        if max_seq > 0 {
            let _ = repo::peer::set_progress(&conn, peer_device, max_seq, rm_catalog::now());
        }
        Ok((
            report.applied as usize,
            report.skipped as usize,
            report.conflicts as usize,
        ))
    }
}

/// 启动 LAN 传输面并把端口写入 `data_dir/transfer.json`（`docs/03` §5.2）。
pub fn start_lan(state: &Arc<RmState>) -> Result<u16, rm_proto::BizError> {
    let catalog = Arc::new(CatalogForServer(Arc::clone(state)));
    let cfg = rm_transfer::ServerConfig {
        device_id: state.device_id.clone(),
        name: crate::methods::hostname(),
        platform: std::env::consts::OS.to_string(),
        identity: Arc::clone(&state.identity),
        port: 0, // 7912 起顺延
    };
    let handle = rm_transfer::server::start(cfg, catalog, Arc::clone(&state.pairing))
        .map_err(|e| rm_proto::BizError::new(&e.code, e.message))?;
    let port = handle.port();
    write_transfer_port(&state.data_dir, port);
    *state.server.lock().unwrap() = Some(handle);
    Ok(port)
}

fn write_transfer_port(data_dir: &Path, port: u16) {
    let path = data_dir.join("transfer.json");
    let text = serde_json::json!({
        "lan_port": port,
        "proto": rm_transfer::PROTO,
        "updated": rm_catalog::now(),
    })
    .to_string();
    let _ = std::fs::write(path, text);
    // 尽力合并进宿主 registry.json（kernel 重写时可能丢未知字段；见附录 B）。
    let registry = data_dir.join("../../registry.json");
    if let Some(text) = std::fs::read_to_string(&registry).ok() {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("transfer_port".into(), serde_json::json!(port));
                let _ = std::fs::write(
                    &registry,
                    serde_json::to_string_pretty(&v).unwrap_or_default(),
                );
            }
        }
    }
}
