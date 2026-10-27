//! M4 方法：备份/快照/压缩/池归纳（`docs/03` §2.3）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ling_sdk::{CallError, Context, Value};

use rm_catalog::repo;
use rm_proto::codes;
use rm_proto::msg;

use crate::methods::from_args;
use crate::state::{to_call, RmState};

fn location_root(conn: &rusqlite::Connection, location_id: &str) -> Result<String, String> {
    repo::placement::location_get(conn, location_id)
        .map_err(|e| e.to_string())?
        .map(|l| l.1)
        .ok_or_else(|| format!("位置不存在：{location_id}"))
}

fn now_job(state: &RmState, kind: &str, prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let job_id = format!("jb_{prefix}_{}_{nanos:x}", rm_catalog::now());
    let _ = state.with_conn(|conn| repo::job::create(conn, &job_id, kind, rm_catalog::now()));
    job_id
}

fn finish_job(state: &RmState, job_id: &str, result: Result<String, CallError>) {
    let now = rm_catalog::now();
    let _ = state.with_conn(|conn| -> Result<(), String> {
        match &result {
            Ok(m) => {
                repo::job::append_log(conn, job_id, m, now).map_err(|e| e.to_string())?;
                repo::job::finish(conn, job_id, "done", now).map_err(|e| e.to_string())?
            }
            Err(e) => {
                repo::job::append_log(conn, job_id, &e.to_string(), now).map_err(|e| e.to_string())?;
                repo::job::finish(conn, job_id, "failed", now).map_err(|e| e.to_string())?
            }
        }
        // WAL 模式下把已提交数据刷入主库，保证外部连接（测试/工具）可见。
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        Ok(())
    });
}

/// 注册 M4 方法。
pub fn register_m4(ctx: &Context, state: &Arc<RmState>) -> Result<(), ling_sdk::ContextError> {
    let reg =
        |name: &str, handler: Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>| {
            ctx.register_method(name, handler, Box::new(|| {}), None)
        };

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.pool.create",
            Box::new(move |args| {
                let loc = args["location_id"]
                    .as_str()
                    .ok_or_else(|| CallError::new(codes::F101, "pool.create 需要 location_id"))?
                    .to_string();
                let rel = args["rel_path"]
                    .as_str()
                    .ok_or_else(|| CallError::new(codes::F101, "pool.create 需要 rel_path"))?
                    .to_string();
                let comp = args["compression"]
                    .as_str()
                    .unwrap_or("auto")
                    .to_string();
                let cap = args["capacity_hint"].as_i64();
                let id = args["id"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("pl_{}", rm_semantic::gen_id("pl")));
                s.with_conn(|conn| {
                    repo::pool::create(conn, &id, &loc, &rel, cap, &comp, rm_catalog::now())
                        .map_err(|e| e.to_string())
                })
                .map_err(to_call)?;
                Ok(serde_json::json!({
                    "pool_id": id, "location_id": loc, "rel_path": rel,
                }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.pool.list",
            Box::new(move |_| {
                let rows = s
                    .with_conn(|conn| repo::pool::list(conn).map_err(|e| e.to_string()))
                    .map_err(to_call)?;
                let out: Vec<_> = rows
                    .into_iter()
                    .map(|(id, loc, rel, cap, comp, lv, st)| {
                        serde_json::json!({
                            "id": id, "location_id": loc, "rel_path": rel,
                            "capacity_hint": cap, "compression": comp,
                            "last_verified": lv, "state": st,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "rows": out }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.pool.ingest",
            Box::new(move |args| {
                let req: msg::PoolIngestReq = from_args(&args)?;
                let job_id = now_job(&s, "classify", "ingest");
                let state = Arc::clone(&s);
                let ids = req.placement_ids;
                let pool_req = req.pool_id;
                let jid = job_id.clone();
                std::thread::spawn(move || {
                    run_ingest(&state, &jid, &ids, pool_req.as_deref());
                });
                Ok(serde_json::json!({ "job_id": job_id }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.pool.move",
            Box::new(move |args| {
                let req: msg::PoolMoveReq = from_args(&args)?;
                let job_id = now_job(&s, "classify", "pmove");
                let state = Arc::clone(&s);
                let jid = job_id.clone();
                std::thread::spawn(move || {
                    run_pool_move(&state, &jid, &req.from_pool, req.to_pool.as_deref());
                });
                Ok(serde_json::json!({ "job_id": job_id }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.pool.remove",
            Box::new(move |args| {
                let req: msg::PoolRemoveReq = from_args(&args)?;
                s.with_conn(|conn| {
                    let n = repo::pool::count_placements(conn, &req.pool_id)
                        .map_err(|e| e.to_string())?;
                    if n > 0 {
                        return Err(format!("池非空（{n} 条放置），拒绝移除"));
                    }
                    conn.execute("DELETE FROM pool WHERE id=?1", [&req.pool_id])
                        .map_err(|e| e.to_string())?;
                    Ok(serde_json::json!({}))
                })
                .map_err(|e| {
                    if e.message.contains("池非空") {
                        CallError::new(codes::F801, e.message)
                    } else {
                        to_call(e)
                    }
                })
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.snapshot.list",
            Box::new(move |args| {
                let req: msg::SnapshotListReq = from_args(&args)?;
                let limit = req.limit.unwrap_or(100).min(1000);
                let offset = req.offset.unwrap_or(0);
                let rows = s
                    .with_conn(|conn| {
                        repo::snapshot::list(conn, req.location_id.as_deref(), limit, offset)
                            .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                let out: Vec<_> = rows
                    .0
                    .into_iter()
                    .map(|(id, loc, target, created, mh, sj)| {
                        serde_json::json!({
                            "id": id, "location_id": loc, "target": target,
                            "created": created, "manifest_hash": mh, "stats_json": sj,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "rows": out, "total": rows.1 }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.backup.run",
            Box::new(move |args| {
                let req: msg::BackupRunReq = from_args(&args)?;
                let job_id = now_job(&s, "backup", "bk");
                let state = Arc::clone(&s);
                let jid = job_id.clone();
                std::thread::spawn(move || {
                    run_backup(&state, &jid, &req);
                });
                Ok(serde_json::json!({ "job_id": job_id }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.backup.list",
            Box::new(move |_| {
                let rows = s
                    .with_conn(|conn| repo::snapshot::list(conn, None, 1000, 0).map_err(|e| e.to_string()))
                    .map_err(to_call)?;
                let out: Vec<_> = rows
                    .0
                    .into_iter()
                    .map(|(id, loc, target, created, mh, sj)| {
                        serde_json::json!({
                            "id": id, "location_id": loc, "target": target,
                            "created": created, "manifest_hash": mh, "stats_json": sj,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "rows": out, "total": rows.1 }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.backup.verify",
            Box::new(move |args| {
                let req: msg::BackupVerifyReq = from_args(&args)?;
                let job_id = now_job(&s, "backup", "bkv");
                let state = Arc::clone(&s);
                let jid = job_id.clone();
                std::thread::spawn(move || {
                    run_verify(&state, &jid, &req);
                });
                Ok(serde_json::json!({ "job_id": job_id }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.backup.restore",
            Box::new(move |args| {
                let req: msg::BackupRestoreReq = from_args(&args)?;
                let job_id = now_job(&s, "backup", "bkr");
                let state = Arc::clone(&s);
                let jid = job_id.clone();
                std::thread::spawn(move || {
                    run_restore(&state, &jid, &req);
                });
                Ok(serde_json::json!({ "job_id": job_id }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.compress.sample",
            Box::new(move |args| {
                let req: msg::CompressSampleReq = from_args(&args)?;
                let rows = s
                    .with_conn(|conn| -> Result<Vec<serde_json::Value>, String> {
                        let mut out = Vec::new();
                        for pid in &req.placement_ids {
                            let Some(p) = repo::pool::placement_path_info(conn, *pid)
                                .map_err(|e| e.to_string())?
                            else {
                                continue;
                            };
                            let (loc, rel, cid, _, _, pstate) = p;
                            if pstate != "online" {
                                continue;
                            }
                            let Ok(root) = location_root(conn, &loc) else {
                                continue;
                            };
                            let abs = format!("{}/{}", root.trim_end_matches('/'), rel);
                            let path = PathBuf::from(&abs);
                            match rm_store::compress::sample_file(&path) {
                                Ok(r) => out.push(serde_json::json!({
                                    "placement_id": pid,
                                    "blake3": cid,
                                    "sample_in": r.sample_in,
                                    "sample_out": r.sample_out,
                                    "worth": r.worth,
                                    "ratio": r.ratio,
                                })),
                                Err(e) => out.push(serde_json::json!({
                                    "placement_id": pid,
                                    "blake3": cid,
                                    "error": e.message,
                                })),
                            }
                        }
                        Ok(out)
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({ "rows": rows }))
            }),
        )?;
    }

    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.compress.run",
            Box::new(move |args| {
                let req: msg::CompressRunReq = from_args(&args)?;
                let mode = req.mode.clone();
                if !matches!(mode.as_str(), "auto" | "always" | "off") {
                    return Err(CallError::new(
                        codes::F101,
                        format!("非法压缩 mode：{mode}（auto/always/off）"),
                    ));
                }
                let job_id = now_job(&s, "classify", "cmp");
                let state = Arc::clone(&s);
                let jid = job_id.clone();
                std::thread::spawn(move || {
                    run_compress(&state, &jid, &req.placement_ids, &mode);
                });
                Ok(serde_json::json!({ "job_id": job_id }))
            }),
        )?;
    }

    Ok(())
}

fn run_ingest(state: &RmState, job_id: &str, placement_ids: &[i64], pool_id: Option<&str>) {
    let result = (|| -> Result<String, CallError> {
        let pool = state
            .with_conn(|conn| -> Result<_, String> {
                match pool_id {
                    Some(pid) => repo::pool::get(conn, pid)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("池不存在：{pid}")),
                    None => repo::pool::pick_online(conn)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "无可用池".to_string()),
                }
            })
            .map_err(to_call)?;
        let (pool_id, pool_loc, pool_rel, _, _, _, pool_state) = pool;
        if pool_state != "online" {
            return Err(CallError::new(codes::F803, "目标池离线，已进队列"));
        }
        let mut ok = 0u64;
        let mut fail = 0u64;
        let mut lines = Vec::new();
        for (i, pid) in placement_ids.iter().enumerate() {
            let _ = state.with_conn(|conn| {
                repo::job::set_progress(
                    conn,
                    job_id,
                    &format!(
                        "{{\"pct\":{}}}",
                        (i as f64 / placement_ids.len().max(1) as f64) * 100.0
                    ),
                    rm_catalog::now(),
                )
            });
            let step = (|| -> Result<(), CallError> {
                let info = state
                    .with_conn(|conn| {
                        repo::pool::placement_path_info(conn, *pid).map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                let Some((loc, rel, cid, _size, mode, pstate)) = info else {
                    return Err(CallError::new(codes::F101, format!("放置不存在：{pid}")));
                };
                if mode == "managed" {
                    return Ok(());
                }
                if pstate != "online" || loc != pool_loc {
                    return Err(CallError::new(
                        codes::F803,
                        format!("归纳源端离线或跨位置，已进队列：{pid}"),
                    ));
                }
                let root = state
                    .with_conn(|conn| location_root(conn, &loc))
                    .map_err(to_call)?;
                let src = Path::new(&root).join(&rel);
                let content_rel = rm_store::pool::pool_rel_for(&cid, false);
                let dst = rm_store::pool::pool_abs(&root, &pool_rel, &content_rel);
                let mtime = std::fs::metadata(&src)
                    .ok()
                    .and_then(|m| {
                        m.modified()
                            .ok()?
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs() as i64)
                    })
                    .unwrap_or(0);
                let (_tier, n) = rm_store::pool::migrate_file(&src, &dst, &cid).map_err(to_call)?;
                let _ = std::fs::remove_file(&src);
                state
                    .with_conn(|conn| -> Result<(), String> {
                        repo::placement::placement_delete(conn, *pid).map_err(|e| e.to_string())?;
                        repo::pool::placement_add_managed(
                            conn,
                            &pool_loc,
                            &content_rel,
                            &cid,
                            n,
                            mtime,
                            &pool_id,
                            "online",
                            rm_catalog::now(),
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                Ok(())
            })();
            match step {
                Ok(()) => ok += 1,
                Err(e) => {
                    fail += 1;
                    lines.push(format!("pid={pid}: {e}"));
                }
            }
        }
        if fail == 0 {
            Ok(format!("归纳完成：{ok} 件"))
        } else if ok == 0 {
            Err(CallError::new(
                codes::F804,
                format!("归纳全部失败：{}", lines.join("; ")),
            ))
        } else {
            // 部分失败仍标 failed，避免"done 但没做完"的假象（A10）。
            Err(CallError::new(
                codes::F804,
                format!("归纳部分失败：{ok} 成功 / {fail} 失败；{}", lines.join("; ")),
            ))
        }
    })();
    finish_job(state, job_id, result);
}

fn run_pool_move(state: &RmState, job_id: &str, from: &str, to: Option<&str>) {
    let result = (|| -> Result<String, CallError> {
        let (from_loc, from_rel) = state
            .with_conn(|conn| -> Result<(String, String), String> {
                repo::pool::get(conn, from)
                    .map_err(|e| e.to_string())?
                    .map(|p| (p.1, p.2))
                    .ok_or_else(|| format!("池不存在：{from}"))
            })
            .map_err(to_call)?;
        let target = state
            .with_conn(|conn| -> Result<_, String> {
                match to {
                    Some(tid) => repo::pool::get(conn, tid)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("池不存在：{tid}")),
                    None => repo::pool::pick_online(conn)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "无可用目标池".to_string()),
                }
            })
            .map_err(to_call)?;
        let (to_id, to_loc, to_rel, _, _, _, to_state) = target;
        if to_state != "online" {
            return Err(CallError::new(codes::F803, "目标池离线"));
        }
        let placements = state
            .with_conn(|conn| repo::pool::placements_in(conn, from).map_err(|e| e.to_string()))
            .map_err(to_call)?;
        let mut ok = 0u64;
        let mut fail = 0u64;
        for (pid, cid, _sz, rel, pstate) in placements {
            let step = (|| -> Result<(), CallError> {
                if pstate != "online" {
                    return Err(CallError::new(codes::F803, "放置离线"));
                }
                let root = state
                    .with_conn(|conn| location_root(conn, &from_loc))
                    .map_err(to_call)?;
                let src = rm_store::pool::pool_abs(&root, &from_rel, &rel);
                let content_rel = rm_store::pool::pool_rel_for(&cid, false);
                let dst_root = state
                    .with_conn(|conn| location_root(conn, &to_loc))
                    .map_err(to_call)?;
                let dst = rm_store::pool::pool_abs(&dst_root, &to_rel, &content_rel);
                rm_store::pool::migrate_file(&src, &dst, &cid).map_err(to_call)?;
                let _ = std::fs::remove_file(&src);
                state
                    .with_conn(|conn| {
                        repo::pool::set_placement_managed(conn, pid, &to_id, &content_rel)
                            .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                Ok(())
            })();
            match step {
                Ok(()) => ok += 1,
                Err(_) => fail += 1,
            }
        }
        Ok(format!("跨池迁移完成：{ok} 成功，{fail} 失败"))
    })();
    finish_job(state, job_id, result);
}

fn run_backup(state: &RmState, job_id: &str, req: &msg::BackupRunReq) {
    let result = (|| -> Result<String, CallError> {
        let device = state.device_id.clone();
        let items = state
            .with_conn(|conn| -> Result<Vec<rm_backup::BackupItem>, String> {
                let root = location_root(conn, &req.location_id)?;
                let rows = repo::pool::placements_of_location(conn, &req.location_id)
                    .map_err(|e| e.to_string())?;
                let mut items = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for (_pid, cid, size, rel, _mode, pstate) in rows {
                    if pstate != "online" || !seen.insert(cid.clone()) {
                        continue;
                    }
                    let abs = format!("{}/{}", root.trim_end_matches('/'), rel);
                    items.push(rm_backup::BackupItem {
                        blake3: cid,
                        path: PathBuf::from(abs),
                        size: size.max(0) as u64,
                    });
                }
                Ok(items)
            })
            .map_err(to_call)?;
        let target = PathBuf::from(&req.target);
        let password = req.password.as_deref();
        let stats = rm_backup::backup_run(
            &target,
            &device,
            &req.location_id,
            &items,
            password,
            rm_catalog::now(),
            &mut |done, total| {
                let _ = state.with_conn(|conn| {
                    repo::job::set_progress(
                        conn,
                        job_id,
                        &format!("{{\"done\":{done},\"total\":{total}}}"),
                        rm_catalog::now(),
                    )
                });
            },
        )
        .map_err(to_call)?;
        let stats_json = serde_json::json!({
            "files": stats.files,
            "bytes": stats.bytes,
            "new_contents": stats.new_contents,
            "segs": stats.segs,
        })
        .to_string();
        state
            .with_conn(|conn| {
                repo::snapshot::insert(
                    conn,
                    &stats.snapshot_id,
                    &req.location_id,
                    &req.target,
                    rm_catalog::now(),
                    &stats.manifest_hash,
                    &stats_json,
                )
                .map_err(|e| e.to_string())
            })
            .map_err(to_call)?;
        Ok(format!(
            "备份完成：{} 新内容 / {} 文件 / {} 段；snapshot={}",
            stats.new_contents, stats.files, stats.segs, stats.snapshot_id
        ))
    })();
    finish_job(state, job_id, result);
}

fn run_verify(state: &RmState, job_id: &str, req: &msg::BackupVerifyReq) {
    let result = (|| -> Result<String, CallError> {
        let target = PathBuf::from(&req.target);
        let password = req.password.clone();
        let report = rm_backup::backup_verify(
            &target,
            req.full,
            password.as_deref(),
            &mut |done, total| {
                let _ = state.with_conn(|conn| {
                    repo::job::set_progress(
                        conn,
                        job_id,
                        &format!("{{\"done\":{done},\"total\":{total}}}"),
                        rm_catalog::now(),
                    )
                });
            },
        )
        .map_err(to_call)?;
        let now = rm_catalog::now();
        let _ = state.with_conn(|conn| -> Result<(), String> {
            for p in repo::pool::list(conn).map_err(|e| e.to_string())? {
                repo::pool::set_last_verified(conn, &p.0, now).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        Ok(format!(
            "校验完成：检查 {}，正常 {}",
            report.checked, report.ok
        ))
    })();
    finish_job(state, job_id, result);
}

fn run_restore(state: &RmState, job_id: &str, req: &msg::BackupRestoreReq) {
    let result = (|| -> Result<String, CallError> {
        let target = PathBuf::from(&req.target);
        let out_dir = PathBuf::from(&req.out_dir);
        let only = req.contents.clone();
        let password = req.password.clone();
        let stats = rm_backup::backup_restore(
            &target,
            &out_dir,
            only.as_deref(),
            password.as_deref(),
            &mut |done, total| {
                let _ = state.with_conn(|conn| {
                    repo::job::set_progress(
                        conn,
                        job_id,
                        &format!("{{\"done\":{done},\"total\":{total}}}"),
                        rm_catalog::now(),
                    )
                });
            },
        )
        .map_err(to_call)?;
        Ok(format!(
            "恢复完成：{} 件 → {}",
            stats.files,
            out_dir.display()
        ))
    })();
    finish_job(state, job_id, result);
}

fn run_compress(state: &RmState, job_id: &str, placement_ids: &[i64], mode: &str) {
    let mut ok = 0u64;
    let mut skip = 0u64;
    let mut fail = 0u64;
    let mut errs = Vec::new();
    for pid in placement_ids {
        let step = (|| -> Result<(), CallError> {
            let (loc, rel, cid, _size, pstate, enc) = state
                .with_conn(|conn| -> Result<_, String> {
                    let info = repo::pool::placement_path_info(conn, *pid)
                        .map_err(|e| e.to_string())?;
                    let Some((loc, rel, cid, size, _mode, pstate)) = info else {
                        return Err(format!("放置不存在：{pid}"));
                    };
                    let enc: String = conn
                        .query_row(
                            "SELECT enc FROM content WHERE blake3=?1",
                            [&cid],
                            |r| r.get(0),
                        )
                        .unwrap_or_else(|_| "plain".into());
                    Ok((loc, rel, cid, size, pstate, enc))
                })
                .map_err(to_call)?;
            if pstate != "online" {
                return Err(CallError::new(codes::F301, "放置不在线"));
            }
            let root = state
                .with_conn(|conn| location_root(conn, &loc))
                .map_err(to_call)?;
            let abs = format!("{}/{}", root.trim_end_matches('/'), rel);
            let path = PathBuf::from(&abs);
            match mode {
                "off" => {
                    if enc == "zstd" {
                        let tmp = path.with_extension("plain");
                        rm_store::compress::decompress_file(&path, &tmp).map_err(to_call)?;
                        let _ = std::fs::rename(&tmp, &path);
                        state
                            .with_conn(|conn| {
                                conn.execute(
                                    "UPDATE content SET enc='plain', enc_size=NULL WHERE blake3=?1",
                                    [&cid],
                                )
                                .map_err(|e| e.to_string())
                            })
                            .map_err(to_call)?;
                    }
                    skip += 1;
                }
                "always" | "auto" => {
                    if enc == "zstd" {
                        skip += 1;
                        return Ok(());
                    }
                    if mode == "auto" {
                        let sample = rm_store::compress::sample_file(&path).map_err(to_call)?;
                        if !sample.worth {
                            skip += 1;
                            return Ok(());
                        }
                    }
                    let tmp = path.with_extension("zst");
                    let n = rm_store::compress::compress_file(&path, &tmp).map_err(to_call)?;
                    let _ = std::fs::rename(&tmp, &path);
                    let (changed, enc_after) = state
                        .with_conn(|conn| -> Result<(usize, String), String> {
                            let changed = conn
                                .execute(
                                    "UPDATE content SET enc='zstd', enc_size=?2 WHERE blake3=?1",
                                    rusqlite::params![cid, n],
                                )
                                .map_err(|e| e.to_string())?;
                            let enc_after: String = conn
                                .query_row(
                                    "SELECT enc FROM content WHERE blake3=?1",
                                    [&cid],
                                    |r| r.get(0),
                                )
                                .map_err(|e| e.to_string())?;
                            Ok((changed, enc_after))
                        })
                        .map_err(to_call)?;
                    if changed == 0 || enc_after != "zstd" {
                        return Err(CallError::new(
                            codes::F101,
                            format!("压缩回写失败：{cid} changed={changed} enc={enc_after}"),
                        ));
                    }
                }
                _ => unreachable!("validated mode"),
            }
            Ok(())
        })();
        match step {
            Ok(()) => ok += 1,
            Err(e) => {
                fail += 1;
                errs.push(format!("pid={pid}: {e}"));
            }
        }
    }
    let result = if fail > 0 {
        Err(CallError::new(
            codes::F302,
            format!("压缩失败 {fail}/{}：{}", ok + fail, errs.join("; ")),
        ))
    } else {
        Ok(format!("压缩完成：{ok} 变更，{skip} 跳过"))
    };
    finish_job(state, job_id, result);
}
