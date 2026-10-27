//! 方法注册（`docs/03` §2.3 的 M1 子集）。长任务立即返回 `{job_id}`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ling_sdk::{CallError, Capability, Context, Value};

use rm_catalog::repo;
use rm_proto::codes;
use rm_proto::msg;

use crate::events;
use crate::state::{to_call, RmState};

/// 注册全部 M1 方法。
pub fn register_all(ctx: &Context, state: &Arc<RmState>) -> Result<(), ling_sdk::ContextError> {
    let reg = |name: &str,
               handler: Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>,
               cap: Option<Capability>| {
        ctx.register_method(name, handler, Box::new(|| {}), cap)
    };

    // ===== 位置与设备 =====
    // ===== UI（M1 静态页，直接返回 HTML；core-ui 贡献在 activate 里做） =====
    {
        reg(
            "resources-manager.ui.page",
            Box::new(move |args| {
                let path = args["path"].as_str().unwrap_or("/resources/browse");
                let html = crate::ui::page_html(path)
                    .ok_or_else(|| CallError::new(codes::F101, format!("未知页面：{path}")))?;
                Ok(serde_json::json!({"path": path, "html": html}))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.location.add",
            Box::new(move |args| {
                let req: msg::LocationAddReq = from_args(&args)?;
                let id = rm_semantic::gen_id("lc");
                s.with_conn(|conn| {
                    repo::placement::location_add(
                        conn,
                        &id,
                        &s.device_id,
                        &req.path,
                        req.kind.as_deref().unwrap_or("local"),
                        req.label.as_deref(),
                    )
                })
                .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "location");
                Ok(serde_json::json!({"id": id}))
            }),
            Some(Capability::Fs),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.location.list",
            Box::new(move |_| {
                let rows = s
                    .with_conn(repo::placement::location_list)
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|(id, path, kind, state, label)| {
                        serde_json::json!({"id": id, "root_path": path, "kind": kind,
                                           "state": state, "label": label})
                    }).collect::<Vec<_>>()
                }))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.location.set_state",
            Box::new(move |args| {
                let req: msg::LocationSetStateReq = from_args(&args)?;
                if rm_proto::LocationState::parse(&req.state).is_none() {
                    return Err(CallError::new(
                        codes::F101,
                        format!("未知状态：{}", req.state),
                    ));
                }
                let n = s
                    .with_conn(|conn| {
                        repo::placement::location_set_state(conn, &req.location_id, &req.state)
                    })
                    .map_err(to_call)?;
                if n == 0 {
                    return Err(CallError::new(
                        codes::F101,
                        format!("位置不存在：{}", req.location_id),
                    ));
                }
                events::emit_catalog_changed(&ctx2, "location_state");
                Ok(serde_json::json!({}))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.device.list",
            Box::new(move |_| {
                let rows = s.with_conn(repo::placement::device_list).map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|(id, name, platform, state, last_seen)| {
                        serde_json::json!({"id": id, "name": name, "platform": platform,
                                           "state": state, "last_seen": last_seen})
                    }).collect::<Vec<_>>()
                }))
            }),
            None,
        )?;
    }

    // ===== 扫描 =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.scan.start",
            Box::new(move |args| {
                let req: msg::ScanStartReq = from_args(&args)?;
                let loc = s
                    .with_conn(|conn| repo::placement::location_get(conn, &req.location_id))
                    .map_err(to_call)?
                    .ok_or_else(|| {
                        CallError::new(codes::F101, format!("位置不存在：{}", req.location_id))
                    })?;
                // loc = (id, root_path, kind, state, label)
                let root_path = loc.1.clone();
                let job_id = s.new_job_id("scan");
                let cancel = Arc::new(AtomicBool::new(false));
                s.cancel_flags
                    .lock()
                    .unwrap()
                    .insert(job_id.clone(), Arc::clone(&cancel));
                s.with_conn(|conn| repo::job::create(conn, &job_id, "scan", rm_catalog::now()))
                    .map_err(to_call)?;
                spawn_scan(
                    Arc::clone(&s),
                    ctx2.clone(),
                    job_id.clone(),
                    req.location_id.clone(),
                    root_path,
                    req.full,
                    cancel,
                );
                Ok(serde_json::json!({"job_id": job_id}))
            }),
            Some(Capability::Fs),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.scan.cancel",
            Box::new(move |args| {
                let job_id = args["job_id"]
                    .as_str()
                    .ok_or_else(|| CallError::new(codes::F301, "缺少 job_id"))?;
                if let Some(f) = s.cancel_flags.lock().unwrap().get(job_id) {
                    f.store(true, Ordering::SeqCst);
                    Ok(serde_json::json!({}))
                } else {
                    Err(CallError::new(
                        codes::F301,
                        format!("作业不存在或已结束：{job_id}"),
                    ))
                }
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.scan.stats",
            Box::new(move |_| {
                let (files, bytes, contents) =
                    s.with_conn(repo::placement::scan_totals).map_err(to_call)?;
                let (dup_groups, dup_bytes) =
                    s.with_conn(repo::placement::dup_totals).map_err(to_call)?;
                Ok(serde_json::json!({
                    "files": files, "bytes": bytes, "contents": contents,
                    "dup_groups": dup_groups, "dup_bytes": dup_bytes,
                }))
            }),
            None,
        )?;
    }

    // ===== 作业 =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.job.list",
            Box::new(move |args| {
                let kind = args["kind"].as_str().map(str::to_string);
                let rows = s
                    .with_conn(|conn| repo::job::list(conn, kind.as_deref(), 100))
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|(id, kind, state, progress, created, updated)| {
                        serde_json::json!({"id": id, "kind": kind, "state": state,
                                           "progress": serde_json::from_str::<Value>(&progress)
                                             .unwrap_or(Value::Null),
                                           "created": created, "updated": updated})
                    }).collect::<Vec<_>>()
                }))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.job.log",
            Box::new(move |args| {
                let job_id = args["job_id"]
                    .as_str()
                    .ok_or_else(|| CallError::new(codes::F301, "缺少 job_id"))?
                    .to_string();
                let log = s
                    .with_conn(|conn| repo::job::log_of(conn, &job_id))
                    .map_err(to_call)?;
                Ok(serde_json::json!({"lines": log.lines().collect::<Vec<_>>()}))
            }),
            None,
        )?;
    }

    // ===== 浏览 =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.browse.query",
            Box::new(move |args| {
                let req: msg::BrowseQueryReq = from_args(&args)?;
                let (rows, total) = s
                    .with_conn(|conn| {
                        repo::placement::browse(
                            conn,
                            req.location_id.as_deref(),
                            req.path_prefix.as_deref(),
                            req.page.limit(),
                            req.page.offset(),
                        )
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|(id, rel, loc, cid, size, asset, state, mode)| {
                        serde_json::json!({"placement_id": id, "rel_path": rel,
                                           "location_id": loc, "content_id": cid,
                                           "size": size, "asset_type": asset,
                                           "state": state, "mode": mode})
                    }).collect::<Vec<_>>(),
                    "total": total,
                }))
            }),
            None,
        )?;
    }

    // ===== 词条 =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.entry.create",
            Box::new(move |args| {
                let req: msg::EntryCreateReq = from_args(&args)?;
                let id = rm_semantic::create_entry(
                    &s.conn.lock().unwrap(),
                    &req.title,
                    &req.kind,
                    req.subtitle.as_deref(),
                )
                .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "entry");
                Ok(serde_json::json!({"id": id}))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.entry.list",
            Box::new(move |args| {
                let req: msg::EntryListReq = from_args(&args)?;
                let (rows, total) = s
                    .with_conn(|conn| {
                        repo::entry::list(
                            conn,
                            req.kind.as_deref(),
                            req.q.as_deref(),
                            req.page.limit(),
                            req.page.offset(),
                        )
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|(id, kind, title, subtitle, created, updated)| {
                        serde_json::json!({"id": id, "kind": kind, "title": title,
                                           "subtitle": subtitle, "created": created,
                                           "updated": updated})
                    }).collect::<Vec<_>>(),
                    "total": total,
                }))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.entry.get",
            Box::new(move |args| {
                let entry_id = args["entry_id"]
                    .as_str()
                    .ok_or_else(|| CallError::new(codes::F302, "缺少 entry_id"))?;
                let detail = rm_semantic::entry_detail(&s.conn.lock().unwrap(), entry_id)
                    .map_err(to_call)?;
                Ok(detail)
            }),
            None,
        )?;
    }

    // ===== 版本 =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.version.add",
            Box::new(move |args| {
                let req: msg::VersionAddReq = from_args(&args)?;
                let id = rm_semantic::add_version(
                    &s.conn.lock().unwrap(),
                    &req.entry_id,
                    &req.title,
                    req.ordinal,
                    req.edition.as_deref(),
                    req.resolution,
                    req.codec.as_deref(),
                    req.language.as_deref(),
                    req.source.as_deref(),
                )
                .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "version");
                Ok(serde_json::json!({"id": id}))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.version.link_files",
            Box::new(move |args| {
                let req: msg::VersionLinkFilesReq = from_args(&args)?;
                s.with_conn(|conn| {
                    for f in &req.files {
                        repo::version::link_file(
                            conn,
                            &req.version_id,
                            &f.rel_path,
                            &f.content_blake3,
                            f.size,
                            &f.role,
                        )?;
                    }
                    Ok::<(), rusqlite::Error>(())
                })
                .map_err(to_call)?;
                Ok(serde_json::json!({}))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.instance.register",
            Box::new(move |args| {
                let req: msg::InstanceRegisterReq = from_args(&args)?;
                let id = rm_semantic::gen_id("in");
                s.with_conn(|conn| {
                    repo::version::instance_register(
                        conn,
                        &id,
                        &req.version_id,
                        &req.location_id,
                        &req.path,
                        rm_catalog::now(),
                    )
                })
                .map_err(to_call)?;
                Ok(serde_json::json!({"id": id}))
            }),
            None,
        )?;
    }

    // ===== 标签 =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.tag.create",
            Box::new(move |args| {
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| CallError::new(codes::F101, "缺少 name"))?;
                let domain = args["domain"].as_str().unwrap_or("genre");
                let id = rm_semantic::ensure_tag(&s.conn.lock().unwrap(), name, domain)
                    .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "tag");
                Ok(serde_json::json!({"id": id}))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.tag.list",
            Box::new(move |args| {
                let domain = args["domain"].as_str().map(str::to_string);
                let rows = s
                    .with_conn(|conn| {
                        repo::tag::ensure_builtin_domains(conn)?;
                        repo::tag::tag_list(conn, domain.as_deref())
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|(id, parent, domain, name)| {
                        serde_json::json!({"id": id, "parent_id": parent,
                                           "domain": domain, "name": name})
                    }).collect::<Vec<_>>()
                }))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.tag.apply",
            Box::new(move |args| {
                let req: msg::TagApplyReq = from_args(&args)?;
                rm_semantic::apply_tag(
                    &s.conn.lock().unwrap(),
                    &req.target_kind,
                    &req.target_id,
                    &req.tag_id,
                )
                .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "tag");
                Ok(serde_json::json!({}))
            }),
            None,
        )?;
    }

    // ===== 去重 =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.dup.list",
            Box::new(move |args| {
                let req: msg::DupListReq = from_args(&args)?;
                let (groups, total) = s
                    .with_conn(|conn| {
                        rm_dedup::dup_groups(
                            conn,
                            req.min_size,
                            req.page.limit(),
                            req.page.offset(),
                        )
                    })
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "groups": groups.into_iter().map(|g| {
                        serde_json::json!({
                            "content_id": g.content_id, "size": g.size,
                            "asset_type": g.asset_type, "copy_count": g.copy_count,
                            "placements": g.placements.into_iter().map(|p| {
                                serde_json::json!({
                                    "placement_id": p.placement_id,
                                    "location_id": p.location_id,
                                    "rel_path": p.rel_path, "state": p.state,
                                    "pinned": p.pinned, "inode": p.inode,
                                    "last_verified": p.last_verified,
                                })
                            }).collect::<Vec<_>>(),
                        })
                    }).collect::<Vec<_>>(),
                    "total": total,
                }))
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.dup.link",
            Box::new(move |args| {
                let req: msg::DupLinkReq = from_args(&args)?;
                let (linked, freed) = s
                    .with_conn(|conn| rm_dedup::hardlink_merge(conn, &req.content_id, req.keep))
                    .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "dup");
                Ok(serde_json::json!({"linked": linked, "freed_bytes": freed}))
            }),
            Some(Capability::Fs),
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.dup.delete",
            Box::new(move |args| {
                let req: msg::DupDeleteReq = from_args(&args)?;
                if req.placement_ids.is_empty() {
                    return Err(CallError::new(codes::F301, "placement_ids 不能为空"));
                }
                let plan = s
                    .with_conn(|conn| rm_dedup::plan_delete(conn, &req.placement_ids, req.force))
                    .map_err(to_call)?;
                match &plan.verdict {
                    rm_dedup::DeleteVerdict::Blocked { reason, detail } => {
                        // 业务结果（HTTP 200 语义，`docs/03` §2.5）：带 blocked 字段。
                        Ok(serde_json::json!({
                            "deleted": 0u64,
                            "blocked": {"reason": reason, "detail": detail},
                        }))
                    }
                    rm_dedup::DeleteVerdict::Allowed => {
                        // 建根路径查找闭包。
                        let roots = s.with_conn(|conn| {
                            repo::placement::location_list(conn).map(|rows| {
                                rows.into_iter()
                                    .map(|(id, path, _, _, _)| (id, path))
                                    .collect::<std::collections::BTreeMap<_, _>>()
                            })
                        });
                        let roots = match roots {
                            Ok(r) => r,
                            Err(e) => return Err(to_call(e)),
                        };
                        let deleted = s
                            .with_conn_mut(|conn| {
                                rm_dedup::execute_delete(conn, &plan, &|lid| {
                                    roots.get(lid).cloned()
                                })
                            })
                            .map_err(to_call)?;
                        events::emit_catalog_changed(&ctx2, "delete");
                        Ok(serde_json::json!({
                            "deleted": deleted,
                            "risk_record_id": if req.force {
                                Some(format!("risk_{}", rm_catalog::now()))
                            } else { None },
                        }))
                    }
                }
            }),
            Some(Capability::Fs),
        )?;
    }

    // ===== 分类规则 =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.classify.rules",
            Box::new(move |args| {
                let req: msg::ClassifyRulesReq = from_args(&args)?;
                match req.rules {
                    Some(rules) => {
                        let out = rm_classify::import_rules(&s.conn.lock().unwrap(), &rules)
                            .map_err(to_call)?;
                        Ok(serde_json::json!({"imported": out.imported}))
                    }
                    None => {
                        let rules =
                            rm_classify::list_rules(&s.conn.lock().unwrap()).map_err(to_call)?;
                        Ok(serde_json::json!({"rules": rules}))
                    }
                }
            }),
            None,
        )?;
    }
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.classify.run",
            Box::new(move |args| {
                let req: msg::ClassifyRunReq = from_args(&args)?;
                let out = rm_classify::run_rules(&s.conn.lock().unwrap(), req.rule_ids.as_deref())
                    .map_err(to_call)?;
                events::emit_catalog_changed(&ctx2, "classify");
                Ok(serde_json::json!({
                    "placements_scanned": out.placements_scanned,
                    "tags_applied": out.tags_applied,
                    "rules_fired": out.rules_fired,
                }))
            }),
            None,
        )?;
    }

    // ===== 流交付（03 §2.6 契约落地；本模块实现契约，不改设计） =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.stream.open",
            Box::new(move |args| {
                let req: msg::StreamOpenReq = from_args(&args)?;
                let pid = parse_placement_id(&req.placement_id)?;
                // 开流时校验一次（后续每次 HTTP 请求还会实时查）。
                let (p_state, loc_state, enc) = s.with_conn(|conn| {
                    repo::optional(conn.query_row(
                        "SELECT p.state, l.state, c.enc
                         FROM placement p
                         JOIN location l ON l.id = p.location_id
                         JOIN content c ON c.blake3 = p.content_id
                         WHERE p.id=?1",
                        rusqlite::params![pid],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
                    ))
                    .map_err(|e| e.to_string())
                })
                .map_err(to_call)?
                .ok_or_else(|| CallError::new(codes::F501, "放置不存在或已删除"))?;
                if p_state != "online" || loc_state != "online" {
                    return Err(CallError::new(codes::F501, "数据当前不可用"));
                }
                let _ = enc;
                // 铸 token + 组 URL（锁内只做 mint/url，DB 查询在锁外）。
                let url = {
                    let guard = s.stream.lock().unwrap();
                    let handle = guard
                        .as_ref()
                        .ok_or_else(|| CallError::new(codes::F504, "流服务未启动，请重试 stream.open"))?;
                    let (stream_id, token) = handle.mint(pid);
                    handle.url_for(&stream_id, &token)
                };
                stream_open_response(&s, pid, url)
            }),
            Some(Capability::Fs),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.get_local_path",
            Box::new(move |args| {
                let req: msg::GetLocalPathReq = from_args(&args)?;
                let pid = parse_placement_id(&req.placement_id)?;
                let info = s.with_conn(|conn| {
                    repo::optional(conn.query_row(
                        "SELECT p.state, l.state, l.root_path, p.rel_path, c.enc
                         FROM placement p
                         JOIN location l ON l.id = p.location_id
                         JOIN content c ON c.blake3 = p.content_id
                         WHERE p.id=?1",
                        rusqlite::params![pid],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, String>(3)?,
                                r.get::<_, String>(4)?,
                            ))
                        },
                    ))
                    .map_err(|e| e.to_string())
                })
                .map_err(to_call)?
                .ok_or_else(|| CallError::new(codes::F501, "放置不存在或已删除"))?;
                let (p_state, loc_state, root, rel, enc) = info;
                if p_state != "online" || loc_state != "online" {
                    return Err(CallError::new(codes::F501, "数据当前不可用"));
                }
                if enc != "plain" {
                    return Err(CallError::new(codes::F503, "当前形态不支持路径交付"));
                }
                let path = std::path::Path::new(&root).join(&rel);
                Ok(serde_json::json!({ "path": path.to_string_lossy() }))
            }),
            Some(Capability::Fs),
        )?;
    }

    crate::methods_m2::register_m2(ctx, state)?;
    crate::methods_m3::register_m3(ctx, state)?;
    crate::methods_m4::register_m4(ctx, state)?;
    crate::methods_m5::register_m5(ctx, state)?;

    Ok(())
}

/// `stream.open` 响应（03 §2.6.1）：复算逻辑长度并组契约形态。
fn stream_open_response(
    s: &Arc<crate::state::RmState>,
    pid: i64,
    url: String,
) -> Result<Value, CallError> {
    let (enc, content_size, file_len) = s.with_conn(|conn| {
        repo::optional(conn.query_row(
            "SELECT c.enc, c.size, p.size
             FROM placement p JOIN content c ON c.blake3 = p.content_id
             WHERE p.id=?1",
            rusqlite::params![pid],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        ))
        .map_err(|e| e.to_string())
    })
    .map_err(to_call)?
    .ok_or_else(|| CallError::new(codes::F501, "放置不存在或已删除"))?;
    let content_len = if enc == "plain" {
        file_len.max(0) as u64
    } else {
        content_size.max(0) as u64
    };
    let stream_id = url
        .rsplit("/s/")
        .next()
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
        .to_string();
    Ok(serde_json::json!({
        "stream_id": stream_id,
        "url": url,
        "content_len": content_len,
        "seekable": true,
        "idle_ttl_sec": 600,
    }))
}

/// placement_id：JSON 数字或字符串均可（契约示例为字符串，browse 返回数字）。
pub(crate) fn parse_placement_id(v: &serde_json::Value) -> Result<i64, CallError> {
    match v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| CallError::new(codes::F301, "placement_id 不是整数")),
        serde_json::Value::String(s) => s
            .parse::<i64>()
            .map_err(|_| CallError::new(codes::F301, "placement_id 不是整数")),
        _ => Err(CallError::new(codes::F301, "placement_id 缺失")),
    }
}

pub(crate) fn from_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, CallError> {
    serde_json::from_value(args.clone())
        .map_err(|e| CallError::new(codes::F301, format!("参数解析失败：{e}")))
}

/// 本机显示名（配对与设备登记用）。
pub fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "本机".into())
}

/// 后台扫描线程：批进度 → job 表 + 事件；结束写 job.done。
fn spawn_scan(
    state: Arc<RmState>,
    ctx: Context,
    job_id: String,
    location_id: String,
    root_path: String,
    full: bool,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let root = std::path::PathBuf::from(&root_path);
        if !root.is_dir() {
            let _ = state.with_conn(|conn| -> Result<(), rusqlite::Error> {
                repo::job::append_log(
                    conn,
                    &job_id,
                    &format!("根路径不存在：{root_path}"),
                    rm_catalog::now(),
                )?;
                repo::job::finish(conn, &job_id, "failed", rm_catalog::now())?;
                Ok(())
            });
            events::emit_job_done(
                &ctx,
                &job_id,
                "failed",
                serde_json::json!({"error": "root_missing"}),
            );
            return;
        }
        let ignores: Vec<String> = Vec::new();
        let mut last_emit = std::time::Instant::now();
        let scan_result = state.with_conn_mut(|conn| {
            rm_scan::scan_location(
                conn,
                rm_scan::ScanOptions {
                    location_id: &location_id,
                    root: &root,
                    full,
                    batch_size: 10_000,
                    ignores: &ignores,
                },
                &cancel,
                &mut |p| {
                    if last_emit.elapsed().as_millis() >= 200 {
                        let pct = if p.files_seen > 0 {
                            (p.files_hashed + p.files_skipped) as f64 / p.files_seen as f64 * 100.0
                        } else {
                            0.0
                        };
                        let _ = state.with_conn(|conn| {
                            repo::job::set_progress(
                                conn,
                                &job_id,
                                &serde_json::json!({
                                    "pct": pct, "processed": p.files_seen,
                                    "hashed": p.files_hashed, "skipped": p.files_skipped,
                                })
                                .to_string(),
                                rm_catalog::now(),
                            )
                        });
                        events::emit_job_progress(&ctx, &job_id, pct, p.files_seen);
                        last_emit = std::time::Instant::now();
                    }
                },
            )
        });
        match scan_result.and_then(|r| r) {
            Ok(outcome) => {
                let cancelled = cancel.load(Ordering::SeqCst);
                let state_name = if cancelled { "cancelled" } else { "done" };
                let summary = serde_json::json!({
                    "files": outcome.progress.files_seen,
                    "hashed": outcome.progress.files_hashed,
                    "skipped": outcome.progress.files_skipped,
                    "bytes": outcome.progress.bytes_hashed,
                    "contents_new": outcome.contents_new,
                    "placements_new": outcome.placements_new,
                    "placements_updated": outcome.placements_updated,
                    "errors": outcome.error_list.len(),
                });
                let _ = state.with_conn(|conn| -> Result<(), rusqlite::Error> {
                    repo::job::set_progress(
                        conn,
                        &job_id,
                        &summary.to_string(),
                        rm_catalog::now(),
                    )?;
                    repo::job::finish(conn, &job_id, state_name, rm_catalog::now())?;
                    for e in outcome.error_list.iter().take(50) {
                        repo::job::append_log(conn, &job_id, e, rm_catalog::now())?;
                    }
                    Ok(())
                });
                events::emit_job_done(&ctx, &job_id, state_name, summary);
            }
            Err(e) => {
                let msg = e.message.clone();
                let _ = state.with_conn(|conn| -> Result<(), rusqlite::Error> {
                    repo::job::append_log(conn, &job_id, &msg, rm_catalog::now())?;
                    repo::job::finish(conn, &job_id, "failed", rm_catalog::now())?;
                    Ok(())
                });
                events::emit_job_done(&ctx, &job_id, "failed", serde_json::json!({"error": msg}));
            }
        }
        state.cancel_flags.lock().unwrap().remove(&job_id);
    });
}
