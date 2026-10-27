//! M5 方法：AI 建议（list/apply/reject/revoke）+ 生成作业 + 隐私档位（`docs/03` §2.3 / §5.4）。
//!
//! 边界（`docs/01` §7.8）：AI 只写 `ai_suggestion`，绝不直接写业务表；
//! 低风险（tag/metadata）置信度 ≥ 0.85 自动采纳（source=ai，可 revoke）；
//! 高风险（connection/entry_create/entry_match/version_split）一律 pending。

use std::sync::Arc;

use ling_sdk::{CallError, Context, Value};

use rm_catalog::repo;
use rm_proto::codes;
use rm_proto::msg;

use crate::events;
use crate::methods::from_args;
use crate::state::{to_call, RmState};

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
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        Ok(())
    });
}

/// 注册 M5 方法。
pub fn register_m5(ctx: &Context, state: &Arc<RmState>) -> Result<(), ling_sdk::ContextError> {
    let reg =
        |name: &str, handler: Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>| {
            ctx.register_method(name, handler, Box::new(|| {}), None)
        };

    // ===== ai.suggest.list =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.ai.suggest.list",
            Box::new(move |args| {
                let req: msg::SuggestListReq = from_args(&args)?;
                if let Some(st) = &req.state {
                    if !repo::suggestion::state_known(st) {
                        return Err(CallError::new(
                            codes::F101,
                            format!("未知建议状态：{st}"),
                        ));
                    }
                }
                let f = repo::suggestion::ListFilter {
                    state: req.state.clone(),
                    source: req.source.clone(),
                    target_kind: req.target_kind.clone(),
                    target_id: req.target_id.clone(),
                    limit: req.limit.unwrap_or(50).min(500),
                    offset: req.offset.unwrap_or(0),
                };
                let (rows, total) = s.with_conn(|conn| {
                    let rows = repo::suggestion::list(conn, &f).map_err(|e| e.to_string())?;
                    let total = repo::suggestion::count(conn, &f).map_err(|e| e.to_string())?;
                    Ok::<_, String>((rows, total))
                }).map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|r| {
                        serde_json::json!({
                            "id": r.id, "target_kind": r.target_kind, "target_id": r.target_id,
                            "kind": r.kind, "payload": r.payload, "confidence": r.confidence,
                            "model": r.model, "state": r.state, "source": r.source,
                            "created": r.created, "applied_at": r.applied_at,
                        })
                    }).collect::<Vec<_>>(),
                    "total": total,
                }))
            }),
        )?;
    }

    // ===== ai.suggest.apply =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.ai.suggest.apply",
            Box::new(move |args| {
                let req: msg::SuggestApplyReq = from_args(&args)?;
                if req.suggestion_ids.is_empty() {
                    return Err(CallError::new(codes::F101, "apply 需要 suggestion_ids"));
                }
                let patch = req.patch.clone();
                let now = rm_catalog::now();
                let mut applied = 0u32;
                let mut errors: Vec<String> = Vec::new();
                for id in &req.suggestion_ids {
                    match apply_one(&s, id, patch.as_deref(), now) {
                        Ok(true) => applied += 1,
                        Ok(false) => errors.push(format!("{id}: 已是终态，跳过")),
                        Err(e) => errors.push(format!("{id}: {} {}", e.code, e.message)),
                    }
                }
                let _ = s.with_conn(|conn| {
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
                    Ok::<_, String>(())
                });
                if applied > 0 {
                    events::emit_catalog_changed(&ctx2, "suggestion");
                }
                Ok(serde_json::json!({
                    "applied": applied,
                    "errors": errors,
                }))
            }),
        )?;
    }

    // ===== ai.suggest.reject =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.ai.suggest.reject",
            Box::new(move |args| {
                let req: msg::SuggestRejectReq = from_args(&args)?;
                if req.suggestion_ids.is_empty() {
                    return Err(CallError::new(codes::F101, "reject 需要 suggestion_ids"));
                }
                let mut rejected = 0u32;
                let mut errors: Vec<String> = Vec::new();
                for id in &req.suggestion_ids {
                    let r = s.with_conn(|conn| {
                        let row = repo::suggestion::get(conn, id)
                            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?
                            .ok_or_else(|| rm_proto::BizError::new(
                                codes::F701, format!("建议不存在：{id}")))?;
                        if row.state != "pending" {
                            return Err(rm_proto::BizError::new(
                                codes::F702,
                                format!("建议 {id} 当前状态 {}，不允许 reject", row.state),
                            ));
                        }
                        repo::suggestion::set_state(conn, id, "rejected", rm_catalog::now())
                            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))
                    });
                    match r {
                        Ok(_) => rejected += 1,
                        Err(e) => errors.push(format!("{id}: {} {}", e.code, e.message)),
                    }
                }
                let _ = s.with_conn(|conn| {
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
                    Ok::<_, String>(())
                });
                if rejected > 0 {
                    events::emit_catalog_changed(&ctx2, "suggestion");
                }
                Ok(serde_json::json!({
                    "rejected": rejected,
                    "errors": errors,
                }))
            }),
        )?;
    }

    // ===== ai.suggest.revoke =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.ai.suggest.revoke",
            Box::new(move |args| {
                let req: msg::SuggestRevokeReq = from_args(&args)?;
                if req.source.trim().is_empty() {
                    return Err(CallError::new(codes::F101, "revoke 需要 source"));
                }
                let source = req.source.clone();
                // 1) 回滚已采纳的业务效果（按 source 删除贴附）。
                let revoked_tags = s.with_conn(|conn| {
                    let n1 = repo::tag::entry_tag_revoke_by_source(conn, &source)
                        .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
                    let n2 = repo::tag::version_tag_revoke_by_source(conn, &source)
                        .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
                    Ok::<_, rm_proto::BizError>(n1 + n2)
                }).map_err(to_call)?;
                // 2) pending → rejected。
                let rejected = s
                    .with_conn(|conn| {
                        repo::suggestion::reject_pending_by_source(conn, &source)
                            .map_err(|e| e.to_string())
                    })
                    .map_err(to_call)?;
                let _ = s.with_conn(|conn| {
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
                    Ok::<_, String>(())
                });
                events::emit_catalog_changed(&ctx2, "suggestion");
                Ok(serde_json::json!({
                    "revoked": rejected,
                    "rolled_back_tags": revoked_tags,
                }))
            }),
        )?;
    }

    // ===== ai.run（长任务：确定性提取 + MockProvider 建议 + 分级确认） =====
    {
        let s = Arc::clone(state);
        let ctx2 = ctx.clone();
        reg(
            "resources-manager.ai.run",
            Box::new(move |args| {
                let req: msg::AiRunReq = from_args(&args)?;
                let budget = req.budget.unwrap_or(rm_ai::cost::DEFAULT_MAX_CALLS);
                let job_id = now_job(&s, "ai", "ai");
                let state = Arc::clone(&s);
                let jid = job_id.clone();
                let ctx3 = ctx2.clone();
                let target_kind = req.target_kind.clone();
                let target_id = req.target_id.clone();
                std::thread::spawn(move || {
                    run_ai(&state, &ctx3, &jid, &target_kind, &target_id, budget);
                });
                Ok(serde_json::json!({ "job_id": job_id }))
            }),
        )?;
    }

    // ===== ai.privacy.set / list =====
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.ai.privacy.set",
            Box::new(move |args| {
                let req: msg::AiPrivacySetReq = from_args(&args)?;
                if !repo::suggestion::tier_known(&req.tier) {
                    return Err(CallError::new(
                        codes::F101,
                        format!("未知隐私档位：{}（local/cloud/hybrid）", req.tier),
                    ));
                }
                s.with_conn(|conn| {
                    repo::suggestion::privacy_set(conn, &req.path_prefix, &req.tier)
                        .map_err(|e| e.to_string())
                })
                .map_err(to_call)?;
                Ok(serde_json::json!({
                    "path_prefix": req.path_prefix, "tier": req.tier,
                }))
            }),
        )?;
    }
    {
        let s = Arc::clone(state);
        reg(
            "resources-manager.ai.privacy.list",
            Box::new(move |_| {
                let rows = s
                    .with_conn(repo::suggestion::privacy_list)
                    .map_err(to_call)?;
                Ok(serde_json::json!({
                    "rows": rows.into_iter().map(|(p, t)| {
                        serde_json::json!({"path_prefix": p, "tier": t})
                    }).collect::<Vec<_>>(),
                }))
            }),
        )?;
    }

    Ok(())
}

/// 单条 apply：pending → 执行业务效果 → accepted。返回是否真的 applied。
fn apply_one(
    state: &RmState,
    id: &str,
    patch: Option<&str>,
    now: i64,
) -> Result<bool, rm_proto::BizError> {
    let sugg = state
        .with_conn(|conn| {
            repo::suggestion::get(conn, id)
                .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?
                .ok_or_else(|| rm_proto::BizError::new(codes::F701, format!("建议不存在：{id}")))
        })?;
    if sugg.state != "pending" {
        return Ok(false); // 已是终态，跳过
    }
    // patch：替换 payload（修改后接受）。
    let payload_str = patch.unwrap_or(&sugg.payload);
    let payload: serde_json::Value = serde_json::from_str(payload_str)
        .map_err(|e| rm_proto::BizError::new(codes::F101, format!("payload 解析失败：{e}")))?;
    if !rm_ai::suggest::kind_known(&sugg.kind) {
        return Err(rm_proto::BizError::new(
            codes::F101,
            format!("未知建议种类：{}", sugg.kind),
        ));
    }

    // 执行业务效果（只有 apply 这一刻才碰业务表）。
    state.with_conn(|conn| {
        apply_effect(conn, &sugg, &payload, now)
            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
        repo::suggestion::set_state(conn, id, "accepted", now)
            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?;
        Ok::<_, rm_proto::BizError>(())
    })?;
    Ok(true)
}

/// 把一条建议落地到业务表（`docs/01` §7.8.2：只在 accept 时发生）。
fn apply_effect(
    conn: &rusqlite::Connection,
    sugg: &repo::suggestion::Suggestion,
    payload: &serde_json::Value,
    now: i64,
) -> Result<(), String> {
    match sugg.kind.as_str() {
        "tag" => {
            let tag_name = payload["tag_name"]
                .as_str()
                .ok_or("tag 建议缺少 tag_name")?;
            let domain = payload["domain"].as_str().unwrap_or("subject");
            let tag_id = rm_semantic::ensure_tag(conn, tag_name, domain)
                .map_err(|e| e.to_string())?;
            match sugg.target_kind.as_str() {
                "entry" => repo::tag::entry_tag_apply(
                    conn,
                    &sugg.target_id,
                    &tag_id,
                    &sugg.source,
                    sugg.confidence,
                    Some(&sugg.model),
                    now,
                )
                .map_err(|e| e.to_string())?,
                "version" => repo::tag::version_tag_apply(
                    conn,
                    &sugg.target_id,
                    &tag_id,
                    &sugg.source,
                    sugg.confidence,
                    Some(&sugg.model),
                    now,
                )
                .map_err(|e| e.to_string())?,
                other => return Err(format!("tag 建议不支持 target_kind={other}")),
            }
        }
        "connection" => {
            let from_kind = payload["from_kind"].as_str().ok_or("缺 from_kind")?;
            let from_id = payload["from_id"].as_str().ok_or("缺 from_id")?;
            let to_kind = payload["to_kind"].as_str().ok_or("缺 to_kind")?;
            let to_id = payload["to_id"].as_str().ok_or("缺 to_id")?;
            let ctype = payload["type"].as_str().ok_or("缺 type")?;
            if !rm_catalog::repo::connection::type_known(ctype) {
                return Err(format!("未知关系类型：{ctype}"));
            }
            let cid = rm_semantic::gen_id("cn");
            rm_catalog::repo::connection::add(
                conn,
                &cid,
                from_kind,
                from_id,
                to_kind,
                to_id,
                ctype,
                None,
                &sugg.source,
                sugg.confidence,
                now,
            )
            .map_err(|e| e.to_string())?;
        }
        "entry_create" => {
            let title = payload["title"].as_str().ok_or("缺 title")?;
            let kind = payload["kind"].as_str().unwrap_or("work");
            let eid = rm_semantic::gen_id("en");
            rm_catalog::repo::entry::create(conn, &eid, kind, title, None, now)
                .map_err(|e| e.to_string())?;
        }
        "entry_match" => {
            let entry_id = sugg.target_id.as_str();
            let source = payload["source"].as_str().ok_or("缺 source")?;
            let external_id = payload["external_id"].as_str().ok_or("缺 external_id")?;
            let url = payload["url"].as_str();
            rm_catalog::repo::entry::ext_upsert(
                conn,
                entry_id,
                source,
                external_id,
                url,
                now,
            )
            .map_err(|e| e.to_string())?;
        }
        "metadata" => {
            // metadata 建议：M5 仅登记 accepted 状态（结构化列回写留给后续版本）。
            // 幻觉防护：不碰业务表，仅确认。
        }
        "version_split" => {
            return Err("version_split 的落地逻辑 M5 未实现（保持 pending）".into());
        }
        other => return Err(format!("未知建议种类：{other}")),
    }
    Ok(())
}

/// ai.run 后台任务：提取 → 建议 → 分级确认入库。
fn run_ai(
    state: &RmState,
    ctx: &Context,
    job_id: &str,
    target_kind: &str,
    target_id: &str,
    budget_max: u32,
) {
    let result = (|| -> Result<String, CallError> {
        let mut budget = rm_ai::cost::Budget::new(budget_max);
        let mock = rm_ai::provider::MockProvider;
        let now = rm_catalog::now();

        // 目标上下文：M5 支持 entry / version / metadata(路径)。
        let (rel_path, existing_tags, real_target_kind, real_target_id) =
            state.with_conn(|conn| -> Result<(String, Vec<String>, String, String), rm_proto::BizError> {
                match target_kind {
                    "entry" => {
                        let e = repo::entry::get(conn, target_id)
                            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?
                            .ok_or_else(|| rm_proto::BizError::new(
                                codes::F706, format!("词条不存在：{target_id}")))?;
                        let tags = repo::tag::entry_tags(conn, &e.0)
                            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?
                            .into_iter()
                            .map(|t| t.1)
                            .collect();
                        Ok((e.2.clone(), tags, "entry".into(), e.0))
                    }
                    "version" => {
                        let title: String = conn
                            .query_row(
                                "SELECT title FROM version WHERE id=?1",
                                [target_id],
                                |r| r.get(0),
                            )
                            .map_err(|_| rm_proto::BizError::new(
                                codes::F706, format!("版本不存在：{target_id}")))?;
                        let tags = repo::tag::version_tag_list(conn, target_id)
                            .map_err(|e| rm_proto::BizError::new(codes::F301, e.to_string()))?
                            .into_iter()
                            .map(|t| t.1)
                            .collect();
                        Ok((title, tags, "version".into(), target_id.to_string()))
                    }
                    "metadata" => {
                        Ok((target_id.to_string(), vec![], "metadata".into(), target_id.to_string()))
                    }
                    other => Err(rm_proto::BizError::new(
                        codes::F101,
                        format!("ai.run 不支持 target_kind={other}（entry/version/metadata）"),
                    )),
                }
            })
            .map_err(to_call)?;

        // 隐私档位解析。
        let tier = state
            .with_conn(|conn| {
                repo::suggestion::privacy_resolve(conn, &rel_path)
                    .map_err(|e| e.to_string())
            })
            .map_err(to_call)?;

        // Provider 挑选（M5 全部走 Mock；真实 Ollama/cloud 接口冻结）。
        let provider = rm_ai::provider::pick_provider(&tier, &rel_path, &mock)
            .map_err(|e| CallError::new(&e.code, e.message))?;

        // 确定性提取。
        let extracted = rm_ai::extract::extract(&rel_path);

        // 指纹。
        let ctx_struct = rm_ai::provider::AnalyzeContext {
            target_kind: real_target_kind.clone(),
            target_id: real_target_id.clone(),
            rel_path: rel_path.clone(),
            extracted: extracted.clone(),
            existing_tags: existing_tags.clone(),
            candidate_titles: vec![extracted.title_guess.clone()],
        };
        let input_hash = rm_ai::hash_hex(&serde_json::to_string(&ctx_struct).unwrap_or_default());
        let prompt_hash = rm_ai::hash_hex(&format!(
            "v1|{}|{}|{}",
            provider.id(),
            real_target_kind,
            input_hash
        ));

        // 预算扣减（每次 analyze 算一次模型调用）。
        if !budget.try_spend() {
            return Err(CallError::new(codes::F703, "AI 预算耗尽"));
        }

        // 调 Provider（Mock 确定性、零成本）。
        let raw = provider
            .analyze(&ctx_struct)
            .map_err(|e| CallError::new(&e.code, e.message))?;

        // 入库 + 分级确认。
        let mut pending = 0u32;
        let mut auto_accepted = 0u32;
        for r in raw {
            if !rm_ai::suggest::kind_known(&r.kind) {
                continue;
            }
            let sg_id = rm_ai::gen_sg_id();
            let payload = r.payload.to_string();
            let source = if matches!(provider.id(), "mock") {
                "ai"
            } else {
                "ai"
            };
            state.with_conn(|conn| {
                repo::suggestion::insert(
                    conn,
                    &sg_id,
                    &real_target_kind,
                    &real_target_id,
                    &r.kind,
                    &payload,
                    r.confidence,
                    provider.id(),
                    &prompt_hash,
                    &input_hash,
                    source,
                    now,
                )
                .map_err(|e| e.to_string())
            }).map_err(to_call)?;

            if rm_ai::suggest::auto_acceptable(&r.kind, r.confidence) {
                // metadata 目标不做业务贴附（tag/connection 无落点），保持 pending 人工处理。
                let applicable = !(real_target_kind == "metadata"
                    && matches!(r.kind.as_str(), "tag" | "connection" | "entry_create" | "entry_match" | "version_split"));
                if applicable {
                    // 自动采纳：立即执行业务效果 + 标 accepted。
                    let payload_val: serde_json::Value =
                        serde_json::from_str(&payload).unwrap_or_default();
                    let applied = state.with_conn(|conn| {
                        let sugg = repo::suggestion::get(conn, &sg_id)
                            .map_err(|e| e.to_string())?
                            .ok_or("建议插入后丢失")?;
                        apply_effect(conn, &sugg, &payload_val, now)?;
                        repo::suggestion::set_state(conn, &sg_id, "accepted", now)
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(())
                    });
                    match applied {
                        Ok(()) => auto_accepted += 1,
                        Err(e) => {
                            // 自动采纳失败 → 降级为 pending（人工兜底）。
                            eprintln!("[rm-ai] 自动采纳失败 {sg_id}: {e}");
                            pending += 1;
                        }
                    }
                } else {
                    pending += 1;
                }
            } else {
                pending += 1;
            }
        }

        let _ = state.with_conn(|conn| {
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
            Ok::<_, String>(())
        });
        events::emit_ai_suggest_created(ctx, pending + auto_accepted);

        Ok(format!(
            "ai.run 完成：target={real_target_kind}/{real_target_id} tier={tier} \
             pending={pending} auto_accepted={auto_accepted} budget_used={}",
            budget.used
        ))
    })();

    finish_job(state, job_id, result);
}
