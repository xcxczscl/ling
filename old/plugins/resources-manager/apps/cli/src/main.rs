//! `rmgr`：resources-manager 业务命令行（`docs/03` §3）。
//!
//! M1：`--standalone` 直连目录库（需 LING_HOME 或 `--db`）；网关模式 M2 接
//! core-ui。退出码：0 成功 / 1 一般错误 / 2 参数错误 / 3 被安全规则阻止。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod client;

use rm_ai::provider::Provider;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    }
    let json = args.iter().any(|a| a == "--json");
    let standalone = args.iter().any(|a| a == "--standalone");
    let db = flag_value(&args, "--db");
    let ling_home = flag_value(&args, "--ling-home")
        .or_else(|| std::env::var("LING_HOME").ok().map(PathBuf::from));

    // 去掉全局开关后的命令词；flag 值在 strip_flag_values 里成对删除。
    let rest: Vec<String> = args
        .iter()
        .filter(|a| !matches!(a.as_str(), "--json" | "--standalone"))
        .cloned()
        .collect();
    let rest = strip_flag_values(&rest);

    if rest.is_empty() {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    }

    // 网关模式：M1 先要求 --standalone（网关接通在集成测试里走 kernel）。
    if !standalone && db.is_none() {
        // 尝试 standalone 缺省路径推导。
    }

    let db_path = resolve_db(db, ling_home.as_deref());
    // M1：仅 standalone 直连目录库；网关模式（core-ui /rpc）随 M2 集成开放。
    let outcome = run_standalone(&rest, &db_path, json);

    match outcome {
        Ok(code) => code,
        Err(code) => code,
    }
}

const USAGE: &str = "\
rmgr — resources-manager 业务命令行

用法:
  rmgr <命令> [参数] [--standalone] [--db <catalog.db>] [--json]

命令（M1+M2+M3+M4）:
  location add <路径> [--label 名]     添加位置
  location list                        列位置
  location set-state <id> <状态>       改位置状态
  device list                          列设备
  device pair <code> --addr <ip:port>  真握手配对（M3）
  device forget <id>                   忘记对端
  scan <location_id> [--full]          扫描
  scan stats                           扫描统计
  dup [--min-size N]                   列重复组
  dup link --content <id> [--keep N]   硬链接合并
  dup delete --placement 1,2 [--force] 安全删除（被拦截退出码 3）
  entry list [--kind K] [--q 前缀]     列词条
  entry create --title T [--kind K]    新建词条
  entry show <entry_id>                词条详情
  tag list [--domain D]                列标签
  tag apply --entry E --tag T          贴标签
  classify apply <rules.json>          导入并执行规则
  job list / job log <id>              作业
  verify --location <id> [--full]      校验副本
  connection list / add / del          关系
  fetch <blake3> --addr <ip:port> [--out 路径]   按需取回（M3，同步等待）
  transfer list / cancel <job_id>      传输任务
  evict --placement 1,2                驱逐（删本体留记录）
  policy set --path-prefix P --mode M  按需策略
  policy list                          策略列表
  sync status / export / import <file> / conflicts / resolve
  sync [--peer <id>]                   记录同步（有 addr 走局域网）
  backup --location <id> --target <目录> [--password pw]   段式备份（M4）
  backup list                          备份快照列表
  backup verify --target <目录> [--full] [--password pw]   校验备份
  backup restore --target <目录> --snapshot <id> --out <目录>  恢复
  snapshot list [--location <id>]      快照列表
  compress sample --placement 1,2      试压报告
  compress run --placement 1 --mode auto|always|off  执行压缩
  pool list                            池列表
  pool create --location <id> --rel-path <相对路径> [--compression auto]
  pool ingest --placement 1,2 [--pool <id>]          归纳入池
  pool move --from <pool> [--to <pool>]              跨池迁移
  pool remove --pool <id>                            移除空池
  ai suggest list [--state pending] [--source ai]    AI 建议列表
  ai suggest apply --ids id1,id2 [--patch json]      接受建议
  ai suggest reject --ids id1,id2                    拒绝建议
  ai suggest revoke --source ai                      按来源批量撤销
  ai run --target-kind entry --target-id <id>        生成 AI 建议（作业）
  ai privacy set --path-prefix /敏感 --tier local    设置隐私档位
  ai privacy list                                    隐私档位列表
";

fn flag_value(args: &[String], flag: &str) -> Option<PathBuf> {
    let prefixed = format!("{flag}=");
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).map(PathBuf::from))
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix(&prefixed).map(PathBuf::from))
        })
}

fn strip_flag_values(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        // 只剥离全局 flag；子命令 flag 留给 cmd_* 自己解析。
        if a == "--db" || a == "--ling-home" {
            i += 2;
            continue;
        }
        if a.starts_with("--db=") || a.starts_with("--ling-home=") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

fn opt_value(args: &[String], flag: &str) -> Option<String> {
    let prefixed = format!("{flag}=");
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix(&prefixed).map(|s| s.to_string()))
        })
}

fn resolve_db(db: Option<PathBuf>, ling_home: Option<&std::path::Path>) -> PathBuf {
    if let Some(d) = db {
        return d;
    }
    if let Some(home) = ling_home {
        return home
            .join("plugins")
            .join("resources-manager")
            .join("data")
            .join("catalog.db");
    }
    PathBuf::from("catalog.db")
}

fn run_standalone(
    rest: &[String],
    db_path: &std::path::Path,
    json: bool,
) -> Result<ExitCode, ExitCode> {
    use rm_proto::codes;
    if !db_path.exists() && !matches!(rest[0].as_str(), "location" | "device") {
        // location add 可以创建新库；其他命令要求库存在。
        if rest[0] != "location" {
            eprintln!(
                "目录库不存在：{}（先 location add 或 --db）",
                db_path.display()
            );
            return Err(ExitCode::from(1));
        }
    }
    let mut conn = match rm_catalog::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("打开目录库失败：{e}");
            return Err(ExitCode::from(1));
        }
    };
    let _ = codes::F101;

    let out = match rest[0].as_str() {
        "location" => cmd_location(&mut conn, &rest[1..], json),
        "device" => cmd_device(&conn, &rest[1..], json),
        "scan" => cmd_scan(&mut conn, rest, db_path, json),
        "dup" => cmd_dup(&mut conn, &rest[1..], json),
        "entry" => cmd_entry(&conn, &rest[1..], json),
        "tag" => cmd_tag(&conn, &rest[1..], json),
        "classify" => cmd_classify(&conn, &rest[1..], json),
        "job" => cmd_job(&conn, &rest[1..], json),
        "verify" => cmd_verify(&mut conn, &rest[1..], json),
        "connection" => cmd_connection(&conn, &rest[1..], json),
        "sync" => cmd_sync(&mut conn, db_path, &rest[1..], json),
        "fetch" => cmd_fetch(&mut conn, db_path, &rest[1..], json),
        "transfer" => cmd_transfer(&conn, &rest[1..], json),
        "evict" => cmd_evict(&mut conn, &rest[1..], json),
        "policy" => cmd_policy(&mut conn, &rest[1..], json),
        "backup" => cmd_backup(&mut conn, &rest[1..], json),
        "snapshot" => cmd_snapshot(&conn, &rest[1..], json),
        "compress" => cmd_compress(&mut conn, &rest[1..], json),
        "pool" => cmd_pool(&mut conn, &rest[1..], json),
        "ai" => cmd_ai(&mut conn, &rest[1..], json),
        other => {
            eprintln!("未知命令：{other}\n{USAGE}");
            return Err(ExitCode::from(2));
        }
    };
    match out {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(client::CmdError::Blocked(msg)) => {
            eprintln!("被安全规则阻止：{msg}");
            Ok(ExitCode::from(3))
        }
        Err(client::CmdError::Usage(msg)) => {
            eprintln!("参数错误：{msg}");
            Err(ExitCode::from(2))
        }
        Err(client::CmdError::Biz(msg)) => {
            eprintln!("{msg}");
            Err(ExitCode::from(1))
        }
    }
}

fn emit(json: bool, text: String, value: serde_json::Value) {
    if json {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        println!("{text}");
    }
}

fn cmd_location(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("add") => {
            let path = rest
                .get(1)
                .ok_or(client::CmdError::Usage("缺少路径".into()))?;
            let label = opt_value(rest, "--label");
            let id = rm_semantic::gen_id("lc");
            let device_id = "dv_local";
            let fingerprint = format!(
                "sha256:{}",
                blake3_hash(std::env::temp_dir().to_string_lossy().as_bytes())
            );
            rm_catalog::repo::placement::device_upsert(
                conn,
                device_id,
                "本机",
                std::env::consts::OS,
                &fingerprint,
                rm_catalog::now(),
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            rm_catalog::repo::placement::location_add(
                conn,
                &id,
                device_id,
                path,
                "local",
                label.as_deref(),
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("位置已添加：{id}"),
                serde_json::json!({"id": id}),
            );
            Ok(())
        }
        Some("list") => {
            let rows = rm_catalog::repo::placement::location_list(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|(id, path, kind, state, label)| {
                            serde_json::json!({"id": id, "root_path": path, "kind": kind,
                                               "state": state, "label": label})
                        }).collect::<Vec<_>>()
                    }),
                );
            } else {
                for (id, path, kind, state, label) in rows {
                    println!(
                        "{id}\t{path}\t{kind}\t{state}\t{}",
                        label.unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        Some("set-state") => {
            let id = rest
                .get(1)
                .ok_or(client::CmdError::Usage("缺少位置 id".into()))?;
            let state = rest
                .get(2)
                .ok_or(client::CmdError::Usage("缺少状态".into()))?;
            if rm_proto::LocationState::parse(state).is_none() {
                return Err(client::CmdError::Usage(format!("未知状态：{state}")));
            }
            let n = rm_catalog::repo::placement::location_set_state(conn, id, state)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if n == 0 {
                return Err(client::CmdError::Biz(format!("位置不存在：{id}")));
            }
            emit(
                json,
                format!("已更新：{id} → {state}"),
                serde_json::json!({}),
            );
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "location 需要 add/list/set-state".into(),
        )),
    }
}

fn blake3_hash(data: &[u8]) -> String {
    // 避免 cli 直接依赖 blake3：用简单位置 id 派生（仅 fingerprint 用）。
    // 真指纹在插件侧算。这里用长度+首字节的 hex 占位即可满足唯一约束（路径含随机 temp）。
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn cmd_device(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") => {
            let rows = rm_catalog::repo::placement::device_list(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|(id, name, platform, state, last_seen)| {
                            serde_json::json!({"id": id, "name": name, "platform": platform,
                                               "state": state, "last_seen": last_seen})
                        }).collect::<Vec<_>>()
                    }),
                );
            } else {
                for (id, name, platform, state, _ls) in rows {
                    println!("{id}\t{name}\t{platform}\t{state}");
                }
            }
            Ok(())
        }
        Some("pair") => {
            let code = rest
                .get(1)
                .ok_or(client::CmdError::Usage("device pair 需要配对码".into()))?;
            let addr = opt_value(rest, "--addr")
                .ok_or(client::CmdError::Usage("device pair 需要 --addr <ip:port>".into()))?;
            // 真握手（M3）：需要 identity.key（data_dir 与 catalog.db 同目录）。
            let data_dir = conn
                .query_row("PRAGMA database_list", [], |r| {
                    let _name: String = r.get(1)?;
                    let file: String = r.get(2)?;
                    Ok(std::path::PathBuf::from(file))
                })
                .map(|p| {
                    p.parent()
                        .map(|d| d.to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                })
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let identity = rm_transfer::Identity::load_or_generate(&data_dir)
                .map_err(|e| client::CmdError::Biz(e.message))?;
            let my_dev = rm_semantic::local_device(conn);
            let peer = rm_transfer::pairing::pair(
                &addr,
                code,
                &identity,
                &my_dev,
                "rmgr",
                std::env::consts::OS,
                None,
            )
            .map_err(|e| client::CmdError::Biz(format!("{}: {}", e.code, e.message)))?;
            let now = rm_catalog::now();
            rm_catalog::repo::peer::upsert(
                conn,
                &peer.device_id,
                Some(&addr),
                &peer.fingerprint,
                Some(&peer.pubkey_hex),
                now,
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            rm_catalog::repo::placement::device_upsert(
                conn,
                &peer.device_id,
                &peer.name,
                &peer.platform,
                &peer.fingerprint,
                now,
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("已配对：{} ({})", peer.device_id, peer.fingerprint),
                serde_json::json!({
                    "device_id": peer.device_id,
                    "fingerprint": peer.fingerprint,
                    "addr": addr,
                }),
            );
            Ok(())
        }
        Some("code") => {
            // 配对码由 LAN 传输面（插件激活时启动）签发；standalone 无监听端时提示。
            eprintln!(
                "提示：配对码由 LAN 传输面签发（插件激活时自动监听 7912）。\n\
                 网关模式请调用 resources-manager.device.pairing_code；\n\
                 standalone 请先以 ling 运行插件，或在对端执行 pairing_code 后再 device pair。"
            );
            Err(client::CmdError::Usage(
                "standalone 下无法签发配对码（无 LAN 监听）".into(),
            ))
        }
        Some("forget") => {
            let id = rest
                .get(1)
                .ok_or(client::CmdError::Usage("device forget 需要 id".into()))?;
            rm_catalog::repo::peer::forget(conn, id)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(json, format!("已忘记：{id}"), serde_json::json!({}));
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "device 需要 list/pair/forget".into(),
        )),
    }
}

fn cmd_connection(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") => {
            let from_kind = opt_value(rest, "--from-kind");
            let from_id = opt_value(rest, "--from-id");
            let to_kind = opt_value(rest, "--to-kind");
            let to_id = opt_value(rest, "--to-id");
            let from = match (from_kind.as_deref(), from_id.as_deref()) {
                (Some(k), Some(i)) => Some((k, i)),
                _ => None,
            };
            let to = match (to_kind.as_deref(), to_id.as_deref()) {
                (Some(k), Some(i)) => Some((k, i)),
                _ => None,
            };
            let (rows, total) = rm_semantic::connection_list(conn, from, to, 500, 0)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "total": total,
                        "rows": rows.iter().map(|(id, fk, fi, tk, ti, ty, ord, created)| {
                            serde_json::json!({
                                "id": id, "from_kind": fk, "from_id": fi,
                                "to_kind": tk, "to_id": ti, "type": ty,
                                "ordinal": ord, "created": created,
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("关系 {total}：");
                for (id, fk, fi, tk, ti, ty, ord, _) in rows {
                    println!("{id}\t{fk}:{fi} -[{ty}]-> {tk}:{ti}\tord={:?}", ord);
                }
            }
            Ok(())
        }
        Some("add") => {
            let from_kind = opt_value(rest, "--from-kind").ok_or(client::CmdError::Usage(
                "connection add 需要 --from-kind".into(),
            ))?;
            let from_id = opt_value(rest, "--from-id").ok_or(client::CmdError::Usage(
                "connection add 需要 --from-id".into(),
            ))?;
            let to_kind = opt_value(rest, "--to-kind").ok_or(client::CmdError::Usage(
                "connection add 需要 --to-kind".into(),
            ))?;
            let to_id = opt_value(rest, "--to-id").ok_or(client::CmdError::Usage(
                "connection add 需要 --to-id".into(),
            ))?;
            let ctype = opt_value(rest, "--type")
                .ok_or(client::CmdError::Usage("connection add 需要 --type".into()))?;
            let ordinal = opt_value(rest, "--ordinal").and_then(|v| v.parse().ok());
            let id = rm_semantic::connection_add(
                conn, &from_kind, &from_id, &to_kind, &to_id, &ctype, ordinal,
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("已建立关系：{id}"),
                serde_json::json!({"connection_id": id}),
            );
            Ok(())
        }
        Some("del") => {
            let id = rest
                .get(1)
                .ok_or(client::CmdError::Usage("connection del 需要 id".into()))?;
            rm_semantic::connection_del(conn, id)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(json, format!("已删除关系：{id}"), serde_json::json!({}));
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "connection 需要 list/add/del".into(),
        )),
    }
}

fn cmd_sync(
    conn: &mut rusqlite::Connection,
    db_path: &std::path::Path,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("status") => {
            let peers = rm_catalog::repo::peer::list(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let known = peers.iter().map(|p| p.3).min().unwrap_or(0);
            let pending = rm_catalog::repo::op::pending_for_peer(conn, known)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let conflicts =
                rm_sync::pending_count(conn).map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let last = peers.iter().map(|p| p.4).max().unwrap_or(0);
            emit(
                json,
                format!(
                    "对端 {}，待同步 op {pending}，待决冲突 {conflicts}，上次同步 {last}",
                    peers.len()
                ),
                serde_json::json!({
                    "peers": peers.iter().map(|(id, addr, fp, ks, ls)| {
                        serde_json::json!({"device_id": id, "addr": addr, "fingerprint": fp,
                                           "known_seq": ks, "last_sync": ls})
                    }).collect::<Vec<_>>(),
                    "pending_ops": pending,
                    "conflict_count": conflicts,
                    "last_sync": last,
                }),
            );
            Ok(())
        }
        Some("export") => {
            let path = rest.get(1).ok_or(client::CmdError::Usage(
                "sync export 需要输出路径（对端 import 用）".into(),
            ))?;
            let peer = opt_value(rest, "--peer");
            let after = match peer.as_deref() {
                Some(pid) => rm_catalog::repo::peer::get(conn, pid)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .map(|p| p.3)
                    .unwrap_or(0),
                None => 0,
            };
            let ops = rm_sync::export_bundle(conn, after, 1000)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let text = serde_json::to_string_pretty(&ops)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            std::fs::write(path, text)
                .map_err(|e| client::CmdError::Biz(format!("写文件失败：{e}")))?;
            emit(
                json,
                format!("已导出 {} 条 op → {path}", ops.len()),
                serde_json::json!({"count": ops.len(), "path": path}),
            );
            Ok(())
        }
        Some("import") => {
            let path = rest
                .get(1)
                .ok_or(client::CmdError::Usage("sync import 需要文件路径".into()))?;
            let text = std::fs::read_to_string(path)
                .map_err(|e| client::CmdError::Biz(format!("读文件失败：{e}")))?;
            let ops: Vec<rm_sync::BundleOp> = serde_json::from_str(&text)
                .map_err(|e| client::CmdError::Biz(format!("解析失败：{e}")))?;
            let self_dev = rm_semantic::local_device(conn);
            let peer = ops
                .first()
                .map(|o| o.device_id.clone())
                .unwrap_or_else(|| "unknown".into());
            let report = rm_sync::apply_ops(conn, &self_dev, &peer, &ops)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let max_seq = ops.iter().map(|o| o.seq).max().unwrap_or(0);
            if max_seq > 0 {
                let _ =
                    rm_catalog::repo::peer::set_progress(conn, &peer, max_seq, rm_catalog::now());
            }
            emit(
                json,
                format!(
                    "导入完成：应用 {}，跳过 {}，冲突 {}",
                    report.applied, report.skipped, report.conflicts
                ),
                serde_json::json!({
                    "applied": report.applied, "skipped": report.skipped,
                    "conflicts": report.conflicts, "errors": report.errors,
                }),
            );
            Ok(())
        }
        Some("conflicts") => {
            let state = opt_value(rest, "--state");
            let (rows, total) = rm_sync::list_pending(conn, state.as_deref(), 500, 0)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "total": total,
                        "rows": rows.iter().map(|r| {
                            serde_json::json!({
                                "id": r.0, "object_kind": r.1, "object_id": r.2, "field": r.3,
                                "my_value": r.4, "their_value": r.5,
                                "my_device": r.6, "their_device": r.7,
                                "conflict_kind": r.10, "state": r.11,
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("冲突 {total}：");
                for r in rows {
                    println!(
                        "{}\t{}:{} field={} kind={} state={}",
                        r.0, r.1, r.2, r.3, r.10, r.11
                    );
                }
            }
            Ok(())
        }
        Some("resolve") => {
            let id = rest.get(1).ok_or(client::CmdError::Usage(
                "sync resolve 需要 conflict_id".into(),
            ))?;
            let choice = opt_value(rest, "--choice")
                .ok_or(client::CmdError::Usage("sync resolve 需要 --choice".into()))?;
            let device = rm_semantic::local_device(conn);
            rm_sync::resolve_conflict(conn, &device, id, &choice, None)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("已裁决：{id} → {choice}"),
                serde_json::json!({"conflict_id": id, "choice": choice}),
            );
            Ok(())
        }
        _ => {
            // 裸 `rmgr sync [--peer id]`：有 addr 走局域网 /op/*（M3）；无 addr 提示文件桥。
            let peer = opt_value(rest, "--peer");
            let peers = rm_catalog::repo::peer::list(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let target = match peer.as_deref() {
                Some(pid) => peers
                    .iter()
                    .find(|p| p.0 == pid)
                    .cloned()
                    .ok_or_else(|| client::CmdError::Biz(format!("对端不存在：{pid}")))?,
                None => peers
                    .into_iter()
                    .find(|p| p.1.as_deref().is_some_and(|a| !a.is_empty()))
                    .ok_or(client::CmdError::Biz("无带 addr 的对端".into()))?,
            };
            let (peer_id, addr, _, known, _) = target;
            let Some(addr) = addr.filter(|a| !a.is_empty()) else {
                emit(
                    json,
                    "对端无 LAN addr：请用 sync export / sync import 文件桥".into(),
                    serde_json::json!({"bridge": "export_import", "peer_id": peer_id}),
                );
                return Ok(());
            };
            let data_dir = data_dir_of(db_path);
            let identity = rm_transfer::Identity::load_or_generate(&data_dir)
                .map_err(|e| client::CmdError::Biz(e.message))?;
            let my_dev = rm_semantic::local_device(conn);
            let identity = std::sync::Arc::new(identity);
            let remote = rm_transfer::op_client::pull_ops(&addr, &identity, &my_dev, known, 1000)
                .map_err(|e| client::CmdError::Biz(format!("{}: {}", e.code, e.message)))?;
            let peer_dev = remote
                .first()
                .and_then(|o| o["device_id"].as_str())
                .unwrap_or(&peer_id)
                .to_string();
            let bundle: Vec<rm_sync::BundleOp> = remote
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            let max_remote = bundle.iter().map(|o| o.seq).max().unwrap_or(0);
            let report = rm_sync::apply_ops(conn, &my_dev, &peer_dev, &bundle)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let local_ops = rm_sync::export_bundle(conn, known, 1000)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let local_vals: Vec<serde_json::Value> = local_ops
                .iter()
                .map(|o| serde_json::to_value(o).unwrap_or_default())
                .collect();
            let import_report =
                rm_transfer::op_client::push_ops(&addr, &identity, &my_dev, &local_vals)
                    .map_err(|e| client::CmdError::Biz(format!("{}: {}", e.code, e.message)))?;
            let max_local = local_ops.iter().map(|o| o.seq).max().unwrap_or(0);
            let progress = max_remote.max(max_local).max(known);
            let _ = rm_catalog::repo::peer::set_progress(
                conn,
                &peer_id,
                progress,
                rm_catalog::now(),
            );
            let applied = report.applied + import_report.applied;
            emit(
                json,
                format!(
                    "局域网同步完成：应用 {applied}，推送 {}",
                    local_vals.len()
                ),
                serde_json::json!({
                    "transport": "lan",
                    "peer_id": peer_id,
                    "applied": applied,
                    "skipped": report.skipped + import_report.skipped,
                    "conflicts": report.conflicts + import_report.conflicts,
                    "pushed": local_vals.len(),
                }),
            );
            Ok(())
        }
    }
}

fn cmd_scan(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    _db_path: &std::path::Path,
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.get(1).map(String::as_str) {
        Some("stats") => {
            let (files, bytes, contents) = rm_catalog::repo::placement::scan_totals(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let (dup_groups, dup_bytes) = rm_catalog::repo::placement::dup_totals(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("文件 {files}，字节 {bytes}，内容 {contents}，重复组 {dup_groups}（可回收 {dup_bytes} 字节）"),
                serde_json::json!({
                    "files": files, "bytes": bytes, "contents": contents,
                    "dup_groups": dup_groups, "dup_bytes": dup_bytes,
                }),
            );
            Ok(())
        }
        _ => {
            // rmgr scan <location_id>
            let loc_id = rest
                .get(1)
                .ok_or(client::CmdError::Usage("scan 需要位置 id".into()))?;
            let full = rest.iter().any(|a| a == "--full");
            let (root,) = {
                let loc = rm_catalog::repo::placement::location_get(conn, loc_id)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .ok_or_else(|| client::CmdError::Biz(format!("位置不存在：{loc_id}")))?;
                (loc.1.clone(),)
            };
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let outcome = rm_scan::scan_location(
                conn,
                rm_scan::ScanOptions {
                    location_id: loc_id,
                    root: std::path::Path::new(&root),
                    full,
                    batch_size: 10_000,
                    ignores: &[],
                },
                &cancel,
                &mut |p| {
                    if !json {
                        eprintln!(
                            "已见 {}，已哈希 {}，跳过 {}，{} 字节",
                            p.files_seen, p.files_hashed, p.files_skipped, p.bytes_hashed
                        );
                    }
                },
            )
            .map_err(|e| client::CmdError::Biz(e.to_message()))?;
            emit(
                json,
                format!(
                    "扫描完成：文件 {}，新内容 {}，新放置 {}，更新 {}，错误 {}",
                    outcome.progress.files_seen,
                    outcome.contents_new,
                    outcome.placements_new,
                    outcome.placements_updated,
                    outcome.error_list.len()
                ),
                serde_json::json!({
                    "files": outcome.progress.files_seen,
                    "hashed": outcome.progress.files_hashed,
                    "skipped": outcome.progress.files_skipped,
                    "bytes": outcome.progress.bytes_hashed,
                    "contents_new": outcome.contents_new,
                    "placements_new": outcome.placements_new,
                    "placements_updated": outcome.placements_updated,
                    "errors": outcome.error_list,
                }),
            );
            Ok(())
        }
    }
}

fn cmd_dup(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        None | Some("list") => {
            let min_size = opt_value(rest, "--min-size").and_then(|v| v.parse().ok());
            let (groups, total) = rm_dedup::dup_groups(conn, min_size, 500, 0)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "total": total,
                        "groups": groups.iter().map(|g| {
                            serde_json::json!({
                                "content_id": g.content_id, "size": g.size,
                                "copy_count": g.copy_count,
                                "placements": g.placements.iter().map(|p| {
                                    serde_json::json!({"placement_id": p.placement_id,
                                                       "rel_path": p.rel_path,
                                                       "state": p.state})
                                }).collect::<Vec<_>>(),
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("重复组 {total}：");
                for g in &groups {
                    println!("{}  {} 字节 × {}", g.content_id, g.size, g.copy_count);
                    for p in &g.placements {
                        println!("  [{}] {} ({})", p.placement_id, p.rel_path, p.state);
                    }
                }
            }
            Ok(())
        }
        Some("link") => {
            let content = opt_value(rest, "--content")
                .ok_or(client::CmdError::Usage("dup link 需要 --content".into()))?;
            let keep = opt_value(rest, "--keep").and_then(|v| v.parse().ok());
            let (linked, freed) = rm_dedup::hardlink_merge(conn, &content, keep)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("硬链接合并 {linked} 份，释放约 {freed} 字节"),
                serde_json::json!({"linked": linked, "freed_bytes": freed}),
            );
            Ok(())
        }
        Some("delete") => {
            let placement_arg = opt_value(rest, "--placement").ok_or(client::CmdError::Usage(
                "dup delete 需要 --placement".into(),
            ))?;
            let ids: Vec<i64> = placement_arg
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if ids.is_empty() {
                return Err(client::CmdError::Usage(
                    "--placement 需要逗号分隔的 id".into(),
                ));
            }
            let force = rest.iter().any(|a| a == "--force");
            let plan = rm_dedup::plan_delete(conn, &ids, force)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            match plan.verdict {
                rm_dedup::DeleteVerdict::Blocked { reason, detail } => {
                    // 退出码 3（`docs/03` §3.2）。
                    Err(client::CmdError::Blocked(format!("{reason}: {detail}")))
                }
                rm_dedup::DeleteVerdict::Allowed => {
                    let roots: std::collections::BTreeMap<String, String> =
                        rm_catalog::repo::placement::location_list(conn)
                            .map_err(|e| client::CmdError::Biz(e.to_string()))?
                            .into_iter()
                            .map(|(id, path, _, _, _)| (id, path))
                            .collect();
                    let deleted =
                        rm_dedup::execute_delete(conn, &plan, &|lid| roots.get(lid).cloned())
                            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                    emit(
                        json,
                        format!("已删除 {deleted} 份放置"),
                        serde_json::json!({"deleted": deleted}),
                    );
                    Ok(())
                }
            }
        }
        _ => Err(client::CmdError::Usage("dup 需要 list/link/delete".into())),
    }
}

fn cmd_entry(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") => {
            let kind = opt_value(rest, "--kind");
            let q = opt_value(rest, "--q");
            let (rows, total) =
                rm_catalog::repo::entry::list(conn, kind.as_deref(), q.as_deref(), 500, 0)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "total": total,
                        "rows": rows.iter().map(|(id, kind, title, sub, _, _)| {
                            serde_json::json!({"id": id, "kind": kind, "title": title,
                                               "subtitle": sub})
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("词条 {total}：");
                for (id, kind, title, _sub, _, _) in rows {
                    println!("{id}\t{kind}\t{title}");
                }
            }
            Ok(())
        }
        Some("create") => {
            let title = opt_value(rest, "--title")
                .ok_or(client::CmdError::Usage("entry create 需要 --title".into()))?;
            let kind = opt_value(rest, "--kind").unwrap_or_else(|| "work".into());
            let id = rm_semantic::create_entry(conn, &title, &kind, None)
                .map_err(|e| client::CmdError::Biz(e.to_message()))?;
            emit(json, format!("已创建：{id}"), serde_json::json!({"id": id}));
            Ok(())
        }
        Some("show") => {
            let id = rest
                .get(1)
                .ok_or(client::CmdError::Usage("entry show 需要 id".into()))?;
            let detail = rm_semantic::entry_detail(conn, id)
                .map_err(|e| client::CmdError::Biz(e.to_message()))?;
            if json {
                emit(true, String::new(), detail);
            } else {
                println!("{}", serde_json::to_string_pretty(&detail).unwrap());
            }
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "entry 需要 list/create/show".into(),
        )),
    }
}

fn cmd_tag(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") => {
            let domain = opt_value(rest, "--domain");
            rm_catalog::repo::tag::ensure_builtin_domains(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let rows = rm_catalog::repo::tag::tag_list(conn, domain.as_deref())
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|(id, parent, domain, name)| {
                            serde_json::json!({"id": id, "parent_id": parent,
                                               "domain": domain, "name": name})
                        }).collect::<Vec<_>>()
                    }),
                );
            } else {
                for (id, _parent, domain, name) in rows {
                    println!("{id}\t{domain}\t{name}");
                }
            }
            Ok(())
        }
        Some("apply") => {
            let entry = opt_value(rest, "--entry")
                .ok_or(client::CmdError::Usage("tag apply 需要 --entry".into()))?;
            let tag = opt_value(rest, "--tag")
                .ok_or(client::CmdError::Usage("tag apply 需要 --tag".into()))?;
            rm_semantic::apply_tag(conn, "entry", &entry, &tag)
                .map_err(|e| client::CmdError::Biz(e.to_message()))?;
            emit(json, "已贴标签".into(), serde_json::json!({}));
            Ok(())
        }
        _ => Err(client::CmdError::Usage("tag 需要 list/apply".into())),
    }
}

fn cmd_classify(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("apply") => {
            let path = rest.get(1).ok_or(client::CmdError::Usage(
                "classify apply 需要规则文件".into(),
            ))?;
            let text = std::fs::read_to_string(path)
                .map_err(|e| client::CmdError::Biz(format!("读规则文件失败：{e}")))?;
            let rules: Vec<rm_proto::msg::ClassifyRule> = serde_json::from_str(&text)
                .map_err(|e| client::CmdError::Biz(format!("规则 JSON 解析失败：{e}")))?;
            let imported = rm_classify::import_rules(conn, &rules)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let run = rm_classify::run_rules(conn, None)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!(
                    "导入 {} 条规则；扫描 {} 放置，命中 {}，贴标 {}",
                    imported.imported, run.placements_scanned, run.rules_fired, run.tags_applied
                ),
                serde_json::json!({
                    "imported": imported.imported,
                    "placements_scanned": run.placements_scanned,
                    "rules_fired": run.rules_fired,
                    "tags_applied": run.tags_applied,
                }),
            );
            Ok(())
        }
        _ => Err(client::CmdError::Usage("classify 需要 apply".into())),
    }
}

fn cmd_job(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") => {
            let rows = rm_catalog::repo::job::list(conn, None, 100)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|(id, kind, state, progress, _, _)| {
                            serde_json::json!({"id": id, "kind": kind, "state": state,
                                               "progress": serde_json::from_str::<serde_json::Value>(progress).unwrap_or_default()})
                        }).collect::<Vec<_>>()
                    }),
                );
            } else {
                for (id, kind, state, _, _, _) in rows {
                    println!("{id}\t{kind}\t{state}");
                }
            }
            Ok(())
        }
        Some("log") => {
            let id = rest
                .get(1)
                .ok_or(client::CmdError::Usage("job log 需要 id".into()))?;
            let log = rm_catalog::repo::job::log_of(conn, id)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({"lines": log.lines().collect::<Vec<_>>()}),
                );
            } else {
                println!("{log}");
            }
            Ok(())
        }
        _ => Err(client::CmdError::Usage("job 需要 list/log".into())),
    }
}

fn cmd_verify(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    // M1：全量重算选中位置的哈希并核对；发现不一致置 missing。
    let location = opt_value(rest, "--location")
        .ok_or(client::CmdError::Usage("verify 需要 --location".into()))?;
    let full = rest.iter().any(|a| a == "--full");
    let loc = rm_catalog::repo::placement::location_get(conn, &location)
        .map_err(|e| client::CmdError::Biz(e.to_string()))?
        .ok_or_else(|| client::CmdError::Biz(format!("位置不存在：{location}")))?;
    let root = loc.1.clone();
    let placements: Vec<(i64, String, String, i64)> = {
        let mut st = conn
            .prepare("SELECT id, rel_path, content_id, size FROM placement WHERE location_id=?1 AND state='online'")
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
        let mapped = st
            .query_map(rusqlite::params![location], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| client::CmdError::Biz(e.to_string()))?
    };
    let sample: Vec<_> = if full {
        placements
    } else {
        // 抽样 1%（至少 1 条）。
        let step = (placements.len() / 100).max(1);
        placements.into_iter().step_by(step).collect()
    };
    let now = rm_catalog::now();
    let mut checked = 0u64;
    let mut mismatched = 0u64;
    for (pid, rel, content_id, _size) in sample {
        let path = std::path::Path::new(&root).join(&rel);
        match rm_scan::hash_tier::full_hash(&path) {
            Ok(h) if h == content_id => {
                rm_catalog::repo::placement::placement_set_last_verified(conn, pid, now)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                checked += 1;
            }
            Ok(_) => {
                mismatched += 1;
                conn.execute(
                    "UPDATE placement SET state='missing' WHERE id=?1",
                    rusqlite::params![pid],
                )
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            }
            Err(_) => {
                mismatched += 1;
                conn.execute(
                    "UPDATE placement SET state='missing' WHERE id=?1",
                    rusqlite::params![pid],
                )
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            }
        }
    }
    // verified_level 回写：全量一致 → full；抽样 → partial；有不一致 → none。
    let level = if mismatched > 0 {
        "none"
    } else if full {
        "full"
    } else {
        "partial"
    };
    let cids: Vec<String> = {
        let mut st = conn
            .prepare("SELECT DISTINCT content_id FROM placement WHERE location_id=?1")
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
        let mapped = st
            .query_map(rusqlite::params![location], |r| r.get(0))
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| client::CmdError::Biz(e.to_string()))?
    };
    for cid in cids {
        rm_catalog::repo::content::set_verified_level(conn, &cid, level)
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
    }
    emit(
        json,
        format!(
            "校验完成：通过 {checked}，不一致 {}，级别 {level}",
            mismatched
        ),
        serde_json::json!({"checked": checked, "mismatched": mismatched, "verified_level": level}),
    );
    Ok(())
}

// ===== M3：按需取回 / 传输 / 驱逐 / 策略 =====

fn data_dir_of(db_path: &std::path::Path) -> PathBuf {
    db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn cmd_fetch(
    conn: &mut rusqlite::Connection,
    db_path: &std::path::Path,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    let hash = rest
        .first()
        .ok_or(client::CmdError::Usage("fetch 需要 <blake3>".into()))?;
    let addr = opt_value(rest, "--addr").ok_or(client::CmdError::Usage(
        "fetch 需要 --addr <ip:port>（对端 LAN 面）".into(),
    ))?;
    let out = opt_value(rest, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir_of(db_path).join("incoming").join(hash));
    let data_dir = data_dir_of(db_path);
    let (size, _, _) = rm_catalog::repo::content::get(conn, hash)
        .map_err(|e| client::CmdError::Biz(e.to_string()))?
        .ok_or(client::CmdError::Biz(
            "内容记录不存在（先同步记录）".into(),
        ))?;
    // 策略守卫：按目标路径求值。
    let target_s = out.display().to_string();
    let mode = rm_catalog::repo::policy::eval_for_placement(conn, 0, &target_s, hash)
        .map_err(|e| client::CmdError::Biz(e.to_string()))?;
    if !matches!(mode.as_str(), "mirror" | "evictable") {
        return Err(client::CmdError::Blocked(format!(
            "按需策略 {mode} 禁止从对端拉回"
        )));
    }
    let identity = rm_transfer::Identity::load_or_generate(&data_dir)
        .map_err(|e| client::CmdError::Biz(e.message))?;
    let my_dev = rm_semantic::local_device(conn);
    let peer_id = rm_semantic::local_device(conn); // placeholder；以 addr 为 peer 键
    let peer_key = opt_value(rest, "--peer").unwrap_or_else(|| addr.clone());
    let (job_id, offset, _) = rm_catalog::repo::transfer::upsert(
        conn,
        &peer_key,
        hash,
        &target_s,
        rm_catalog::now(),
    )
    .map_err(|e| client::CmdError::Biz(e.to_string()))?;
    let _ = peer_id;
    let task = rm_transfer::client::FetchTask {
        addr,
        identity: std::sync::Arc::new(identity),
        device_id: my_dev,
        content_blake3: hash.clone(),
        target_path: out.clone(),
        total: size.max(0) as u64,
        start_offset: offset.max(0) as u64,
        on_progress: std::sync::Arc::new(|_| {}),
    };
    let now = rm_catalog::now();
    rm_catalog::repo::transfer::set_state(conn, &job_id, "running", None, now)
        .map_err(|e| client::CmdError::Biz(e.to_string()))?;
    match rm_transfer::client::fetch(&task) {
        Ok(_) => {
            let now = rm_catalog::now();
            let _ = rm_catalog::repo::transfer::set_progress(
                conn,
                &job_id,
                size,
                size,
                now,
            );
            rm_catalog::repo::transfer::set_state(conn, &job_id, "done", None, now)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("取回完成：{} → {job_id}", out.display()),
                serde_json::json!({"job_id": job_id, "path": out.display().to_string(), "size": size}),
            );
            Ok(())
        }
        Err(e) => {
            let msg = format!("{}: {}", e.code, e.message);
            rm_catalog::repo::transfer::set_state(
                conn,
                &job_id,
                "failed",
                Some(&msg),
                rm_catalog::now(),
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            Err(client::CmdError::Biz(msg))
        }
    }
}

fn cmd_transfer(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") | None => {
            let job = opt_value(rest, "--job");
            let (rows, total) = rm_catalog::repo::transfer::list(
                conn,
                job.as_deref(),
                500,
                0,
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "total": total,
                        "rows": rows.iter().map(|(id, peer, hash, path, off, tot, st, err, upd)| {
                            serde_json::json!({
                                "id": id, "peer_device_id": peer, "content_blake3": hash,
                                "target_path": path, "offset": off, "total": tot,
                                "state": st, "error": err, "updated": upd,
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("传输任务 {total}：");
                for (id, peer, hash, path, off, tot, st, err, _) in rows {
                    println!("{id}\t{st}\t{off}/{tot}\t{peer}\t{hash}\t{path}\t{}", err.unwrap_or_default());
                }
            }
            Ok(())
        }
        Some("cancel") => {
            let id = rest
                .get(1)
                .ok_or(client::CmdError::Usage("transfer cancel 需要 job_id".into()))?;
            rm_catalog::repo::transfer::set_state(
                conn,
                id,
                "cancelled",
                None,
                rm_catalog::now(),
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(json, format!("已取消：{id}"), serde_json::json!({"job_id": id}));
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "transfer 需要 list / cancel".into(),
        )),
    }
}

fn cmd_evict(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    let spec = opt_value(rest, "--placement").ok_or(client::CmdError::Usage(
        "evict 需要 --placement <id,id>".into(),
    ))?;
    let ids: Vec<i64> = spec
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if ids.is_empty() {
        return Err(client::CmdError::Usage("placement id 列表为空".into()));
    }
    let mut evicted = 0u64;
    let mut blocked = Vec::new();
    for id in ids {
        let Some(p) = rm_catalog::repo::placement::placement_get(conn, id)
            .map_err(|e| client::CmdError::Biz(e.to_string()))?
        else {
            blocked.push(serde_json::json!({"placement_id": id, "reason": "不存在"}));
            continue;
        };
        let (loc_id, rel, content) = (p.1.clone(), p.2.clone(), p.3.clone());
        let Some((_, root, _, loc_state, _)) = rm_catalog::repo::placement::location_get(conn, &loc_id)
            .map_err(|e| client::CmdError::Biz(e.to_string()))?
        else {
            blocked.push(serde_json::json!({"placement_id": id, "reason": "位置不存在"}));
            continue;
        };
        let abs = format!("{}/{}", root.trim_end_matches('/'), rel);
        let mode = rm_catalog::repo::policy::eval_for_placement(conn, id, &abs, &content)
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
        if mode != "evictable" {
            blocked.push(serde_json::json!({"placement_id": id, "reason": format!("策略 {mode} 不允许驱逐")}));
            continue;
        }
        if loc_state == "cold" {
            blocked.push(serde_json::json!({"placement_id": id, "reason": "冷位置"}));
            continue;
        }
        let others = rm_catalog::repo::placement::placements_of_content(conn, &content)
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
        if !others.iter().any(|(oid, _, _, st, _, _, _)| *oid != id && st == "online") {
            blocked.push(serde_json::json!({"placement_id": id, "reason": "无其他在线副本"}));
            continue;
        }
        if abs.starts_with('/') {
            let _ = std::fs::remove_file(&abs);
        }
        rm_catalog::repo::placement::placement_delete(conn, id)
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
        evicted += 1;
    }
    if evicted > 0 && blocked.is_empty() {
        emit(
            json,
            format!("已驱逐 {evicted} 个放置"),
            serde_json::json!({"evicted": evicted}),
        );
        Ok(())
    } else if evicted > 0 {
        emit(
            json,
            format!("已驱逐 {evicted}，拦截 {}", blocked.len()),
            serde_json::json!({"evicted": evicted, "blocked": blocked}),
        );
        Ok(())
    } else {
        Err(client::CmdError::Blocked(format!(
            "全部被拦截：{blocked:?}"
        )))
    }
}

fn cmd_policy(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("set") => {
            let mode = opt_value(rest, "--mode")
                .ok_or(client::CmdError::Usage("policy set 需要 --mode".into()))?;
            if rm_catalog::repo::policy::parse_mode(&mode).is_none() {
                return Err(client::CmdError::Usage(format!("非法策略档：{mode}")));
            }
            let (kind, id) = if let Some(p) = opt_value(rest, "--path-prefix") {
                ("path_prefix", p)
            } else if let Some(t) = opt_value(rest, "--tag") {
                ("tag", t)
            } else {
                return Err(client::CmdError::Usage(
                    "policy set 需要 --path-prefix 或 --tag".into(),
                ));
            };
            let pid = rm_catalog::repo::policy::set(
                conn,
                kind,
                &id,
                &mode,
                opt_value(rest, "--priority")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                rm_catalog::now(),
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("策略已写：{pid} {kind}:{id} → {mode}"),
                serde_json::json!({"policy_id": pid, "mode": mode}),
            );
            Ok(())
        }
        Some("list") | None => {
            let rows = rm_catalog::repo::policy::list(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|(id, sk, sid, mode, prio)| {
                            serde_json::json!({
                                "id": id, "scope_kind": sk, "scope_id": sid,
                                "mode": mode, "priority": prio,
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("策略 {}：", rows.len());
                for (id, sk, sid, mode, prio) in rows {
                    println!("{id}\t{sk}:{sid}\t{mode}\tprio={prio}");
                }
            }
            Ok(())
        }
        _ => Err(client::CmdError::Usage("policy 需要 set / list".into())),
    }
}

// ===== M4：备份 / 快照 / 压缩 / 池 =====

fn cmd_backup(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") => {
            let rows = rm_catalog::repo::snapshot::list(conn, None, 1000, 0)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "total": rows.1,
                        "rows": rows.0.iter().map(|(id, loc, target, created, mh, sj)| {
                            serde_json::json!({
                                "id": id, "location_id": loc, "target": target,
                                "created": created, "manifest_hash": mh, "stats_json": sj,
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("备份快照 {}：", rows.1);
                for (id, loc, target, created, mh, sj) in rows.0 {
                    println!("{id}\t{loc}\t{target}\t{created}\t{mh}\t{sj}");
                }
            }
            Ok(())
        }
        Some("verify") => {
            let target = opt_value(rest, "--target")
                .ok_or(client::CmdError::Usage("backup verify 需要 --target".into()))?;
            let full = rest.iter().any(|a| a == "--full");
            let password = opt_value(rest, "--password");
            let report = rm_backup::backup_verify(
                Path::new(&target),
                full,
                password.as_deref(),
                &mut |_, _| {},
            )
            .map_err(|e| client::CmdError::Biz(format!("{}: {}", e.code, e.message)))?;
            emit(
                json,
                format!(
                    "校验完成：检查 {}，正常 {}",
                    report.checked, report.ok
                ),
                serde_json::json!({
                    "checked": report.checked, "ok": report.ok,
                    "mismatch": report.mismatch, "missing": report.missing,
                }),
            );
            Ok(())
        }
        Some("restore") => {
            let target = opt_value(rest, "--target")
                .ok_or(client::CmdError::Usage("backup restore 需要 --target".into()))?;
            let snapshot = opt_value(rest, "--snapshot")
                .ok_or(client::CmdError::Usage("backup restore 需要 --snapshot".into()))?;
            let out = opt_value(rest, "--out")
                .ok_or(client::CmdError::Usage("backup restore 需要 --out".into()))?;
            let password = opt_value(rest, "--password");
            let only: Option<Vec<String>> = opt_value(rest, "--contents").map(|s| {
                s.split(',')
                    .filter(|x| !x.trim().is_empty())
                    .map(|x| x.trim().to_string())
                    .collect()
            });
            let stats = rm_backup::backup_restore(
                Path::new(&target),
                Path::new(&out),
                only.as_deref(),
                password.as_deref(),
                &mut |_, _| {},
            )
            .map_err(|e| client::CmdError::Biz(format!("{}: {}", e.code, e.message)))?;
            let _ = snapshot;
            emit(
                json,
                format!("恢复完成：{} 件 → {out}", stats.files),
                serde_json::json!({"files": stats.files, "bytes": stats.bytes, "out": out}),
            );
            Ok(())
        }
        Some(_) | None => {
            // 同步备份（standalone 直接跑；插件侧走作业）。
            let loc = opt_value(rest, "--location")
                .ok_or(client::CmdError::Usage("backup 需要 --location".into()))?;
            let target = opt_value(rest, "--target")
                .ok_or(client::CmdError::Usage("backup 需要 --target".into()))?;
            let password = opt_value(rest, "--password");
            let device = rm_semantic::local_device(conn);
            let items = {
                let (_, root, _, _) = {
                    let l = rm_catalog::repo::placement::location_get(conn, &loc)
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?
                        .ok_or(client::CmdError::Biz(format!("位置不存在：{loc}")))?;
                    (l.0, l.1, l.2, l.3)
                };
                let rows = rm_catalog::repo::pool::placements_of_location(conn, &loc)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                let mut items = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for (_, cid, size, rel, _, pstate) in rows {
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
                items
            };
            let stats = rm_backup::backup_run(
                Path::new(&target),
                &device,
                &loc,
                &items,
                password.as_deref(),
                rm_catalog::now(),
                &mut |_, _| {},
            )
            .map_err(|e| client::CmdError::Biz(format!("{}: {}", e.code, e.message)))?;
            let stats_json = serde_json::json!({
                "files": stats.files, "bytes": stats.bytes,
                "new_contents": stats.new_contents, "segs": stats.segs,
            })
            .to_string();
            rm_catalog::repo::snapshot::insert(
                conn,
                &stats.snapshot_id,
                &loc,
                &target,
                rm_catalog::now(),
                &stats.manifest_hash,
                &stats_json,
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!(
                    "备份完成：{} 新内容 / {} 文件；snapshot={}",
                    stats.new_contents, stats.files, stats.snapshot_id
                ),
                serde_json::json!({
                    "snapshot_id": stats.snapshot_id,
                    "files": stats.files, "bytes": stats.bytes,
                    "new_contents": stats.new_contents, "segs": stats.segs,
                    "skipped": stats.skipped,
                    "manifest_hash": stats.manifest_hash,
                }),
            );
            Ok(())
        }
    }
}

fn cmd_snapshot(
    conn: &rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") | None => {
            let loc = opt_value(rest, "--location");
            let limit: u32 = opt_value(rest, "--limit")
                .and_then(|s| s.parse().ok())
                .unwrap_or(100);
            let offset: u32 = opt_value(rest, "--offset")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let rows = rm_catalog::repo::snapshot::list(conn, loc.as_deref(), limit, offset)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "total": rows.1,
                        "rows": rows.0.iter().map(|(id, loc, target, created, mh, sj)| {
                            serde_json::json!({
                                "id": id, "location_id": loc, "target": target,
                                "created": created, "manifest_hash": mh, "stats_json": sj,
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("快照 {}：", rows.1);
                for (id, loc, target, created, mh, sj) in rows.0 {
                    println!("{id}\t{loc}\t{target}\t{created}\t{mh}\t{sj}");
                }
            }
            Ok(())
        }
        _ => Err(client::CmdError::Usage("snapshot 需要 list".into())),
    }
}

fn cmd_compress(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("sample") => {
            let spec = opt_value(rest, "--placement").ok_or(client::CmdError::Usage(
                "compress sample 需要 --placement".into(),
            ))?;
            let ids: Vec<i64> = spec
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            let mut out = Vec::new();
            for pid in ids {
                let Some(p) = rm_catalog::repo::pool::placement_path_info(conn, pid)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                else {
                    continue;
                };
                let (loc, rel, cid, _, _, pstate) = p;
                if pstate != "online" {
                    continue;
                }
                let (_, root, _, _, _) = rm_catalog::repo::placement::location_get(conn, &loc)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .map(|l| (l.0, l.1, l.2, l.3, l.4))
                    .ok_or(client::CmdError::Biz("位置不存在".into()))?;
                let abs = format!("{}/{}", root.trim_end_matches('/'), rel);
                match rm_store::compress::sample_file(Path::new(&abs)) {
                    Ok(r) => out.push(serde_json::json!({
                        "placement_id": pid, "blake3": cid,
                        "sample_in": r.sample_in, "sample_out": r.sample_out,
                        "worth": r.worth, "ratio": r.ratio,
                    })),
                    Err(e) => out.push(serde_json::json!({
                        "placement_id": pid, "blake3": cid, "error": e.message,
                    })),
                }
            }
            if json {
                emit(true, String::new(), serde_json::json!({"rows": out}));
            } else {
                println!("试压报告：");
                for r in &out {
                    println!("{r}");
                }
            }
            Ok(())
        }
        Some("run") => {
            let spec = opt_value(rest, "--placement").ok_or(client::CmdError::Usage(
                "compress run 需要 --placement".into(),
            ))?;
            let mode = opt_value(rest, "--mode").unwrap_or_else(|| "auto".into());
            if !matches!(mode.as_str(), "auto" | "always" | "off") {
                return Err(client::CmdError::Usage(format!(
                    "非法 mode：{mode}"
                )));
            }
            let ids: Vec<i64> = spec
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            let mut ok = 0u64;
            let mut skip = 0u64;
            for pid in ids {
                let Some(p) = rm_catalog::repo::pool::placement_path_info(conn, pid)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                else {
                    continue;
                };
                let (loc, rel, cid, _, _, pstate) = p;
                if pstate != "online" {
                    skip += 1;
                    continue;
                }
                let enc: String = conn
                    .query_row("SELECT enc FROM content WHERE blake3=?1", [&cid], |r| {
                        r.get(0)
                    })
                    .unwrap_or_else(|_| "plain".into());
                let (_, root, _, _, _) = rm_catalog::repo::placement::location_get(conn, &loc)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .map(|l| (l.0, l.1, l.2, l.3, l.4))
                    .ok_or(client::CmdError::Biz("位置不存在".into()))?;
                let abs = format!("{}/{}", root.trim_end_matches('/'), rel);
                let path = PathBuf::from(&abs);
                match mode.as_str() {
                    "off" => {
                        if enc == "zstd" {
                            let tmp = path.with_extension("plain");
                            rm_store::compress::decompress_file(&path, &tmp)
                                .map_err(|e| client::CmdError::Biz(e.message))?;
                            let _ = std::fs::rename(&tmp, &path);
                            conn.execute(
                                "UPDATE content SET enc='plain', enc_size=NULL WHERE blake3=?1",
                                [&cid],
                            )
                            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                            ok += 1;
                        } else {
                            skip += 1;
                        }
                    }
                    "always" | "auto" => {
                        if enc == "zstd" {
                            skip += 1;
                            continue;
                        }
                        if mode == "auto" {
                            let sample = rm_store::compress::sample_file(&path)
                                .map_err(|e| client::CmdError::Biz(e.message))?;
                            if !sample.worth {
                                skip += 1;
                                continue;
                            }
                        }
                        let tmp = path.with_extension("zst");
                        let n = rm_store::compress::compress_file(&path, &tmp)
                            .map_err(|e| client::CmdError::Biz(e.message))?;
                        let _ = std::fs::rename(&tmp, &path);
                        conn.execute(
                            "UPDATE content SET enc='zstd', enc_size=?2 WHERE blake3=?1",
                            rusqlite::params![cid, n],
                        )
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                        ok += 1;
                    }
                    _ => unreachable!(),
                }
            }
            emit(
                json,
                format!("压缩完成：{ok} 变更，{skip} 跳过"),
                serde_json::json!({"changed": ok, "skipped": skip}),
            );
            Ok(())
        }
        _ => Err(client::CmdError::Usage("compress 需要 sample / run".into())),
    }
}

fn cmd_pool(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") | None => {
            let rows = rm_catalog::repo::pool::list(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|(id, loc, rel, cap, comp, lv, st)| {
                            serde_json::json!({
                                "id": id, "location_id": loc, "rel_path": rel,
                                "capacity_hint": cap, "compression": comp,
                                "last_verified": lv, "state": st,
                            })
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("池 {}：", rows.len());
                for (id, loc, rel, cap, comp, lv, st) in rows {
                    println!("{id}\t{loc}\t{rel}\tcap={cap:?}\t{comp}\tlv={lv:?}\t{st}");
                }
            }
            Ok(())
        }
        Some("create") => {
            let loc = opt_value(rest, "--location")
                .ok_or(client::CmdError::Usage("pool create 需要 --location".into()))?;
            let rel = opt_value(rest, "--rel-path")
                .ok_or(client::CmdError::Usage("pool create 需要 --rel-path".into()))?;
            let comp = opt_value(rest, "--compression").unwrap_or_else(|| "auto".into());
            let id = opt_value(rest, "--id").unwrap_or_else(|| format!("pl_{loc}"));
            let cap: Option<i64> = opt_value(rest, "--capacity").and_then(|s| s.parse().ok());
            rm_catalog::repo::pool::create(
                conn,
                &id,
                &loc,
                &rel,
                cap,
                &comp,
                rm_catalog::now(),
            )
            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("池已建：{id} @ {loc}/{rel}"),
                serde_json::json!({"pool_id": id, "location_id": loc, "rel_path": rel}),
            );
            Ok(())
        }
        Some("ingest") => {
            let spec = opt_value(rest, "--placement").ok_or(client::CmdError::Usage(
                "pool ingest 需要 --placement".into(),
            ))?;
            let pool = opt_value(rest, "--pool");
            let ids: Vec<i64> = spec
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            let pool_row = match pool.as_deref() {
                Some(pid) => rm_catalog::repo::pool::get(conn, pid)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .ok_or(client::CmdError::Biz(format!("池不存在：{pid}")))?,
                None => rm_catalog::repo::pool::pick_online(conn)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .ok_or(client::CmdError::Biz("无可用池".into()))?,
            };
            let (pool_id, pool_loc, pool_rel, _, _, _, pool_state) = pool_row;
            if pool_state != "online" {
                return Err(client::CmdError::Blocked("目标池离线".into()));
            }
            let mut ok = 0u64;
            let mut fail = 0u64;
            for pid in ids {
                let Some((loc, rel, cid, _, mode, pstate)) =
                    rm_catalog::repo::pool::placement_path_info(conn, pid)
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?
                else {
                    fail += 1;
                    continue;
                };
                if mode == "managed" {
                    continue;
                }
                if pstate != "online" || loc != pool_loc {
                    fail += 1;
                    continue;
                }
                let (_, root, _, _, _) = rm_catalog::repo::placement::location_get(conn, &loc)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .map(|l| (l.0, l.1, l.2, l.3, l.4))
                    .ok_or(client::CmdError::Biz("位置不存在".into()))?;
                let src = Path::new(&root).join(&rel);
                let content_rel = rm_store::pool::pool_rel_for(&cid, false);
                let dst = rm_store::pool::pool_abs(&root, &pool_rel, &content_rel);
                let mtime = std::fs::metadata(&src)
                    .ok()
                    .and_then(|m| {
                        m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()
                    })
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                match rm_store::pool::migrate_file(&src, &dst, &cid) {
                    Ok((_, n)) => {
                        let _ = std::fs::remove_file(&src);
                        rm_catalog::repo::placement::placement_delete(conn, pid)
                            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                        rm_catalog::repo::pool::placement_add_managed(
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
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                        ok += 1;
                    }
                    Err(e) => {
                        fail += 1;
                        eprintln!("pid={pid}: {}", e.message);
                    }
                }
            }
            emit(
                json,
                format!("归纳完成：{ok} 成功，{fail} 失败"),
                serde_json::json!({"ok": ok, "fail": fail, "pool_id": pool_id}),
            );
            if fail > 0 && ok == 0 {
                Err(client::CmdError::Biz(format!("全部失败（{fail}）")))
            } else {
                Ok(())
            }
        }
        Some("move") => {
            let from = opt_value(rest, "--from")
                .ok_or(client::CmdError::Usage("pool move 需要 --from".into()))?;
            let to = opt_value(rest, "--to");
            let from_row = rm_catalog::repo::pool::get(conn, &from)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?
                .ok_or(client::CmdError::Biz(format!("池不存在：{from}")))?;
            let to_row = match to.as_deref() {
                Some(tid) => rm_catalog::repo::pool::get(conn, tid)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .ok_or(client::CmdError::Biz(format!("池不存在：{tid}")))?,
                None => rm_catalog::repo::pool::pick_online(conn)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .ok_or(client::CmdError::Biz("无可用目标池".into()))?,
            };
            let (from_loc, from_rel) = (from_row.1.clone(), from_row.2.clone());
            let (to_id, to_loc, to_rel) = (to_row.0.clone(), to_row.1.clone(), to_row.2.clone());
            let placements = rm_catalog::repo::pool::placements_in(conn, &from)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let mut ok = 0u64;
            let mut fail = 0u64;
            for (pid, cid, _, rel, pstate) in placements {
                if pstate != "online" {
                    fail += 1;
                    continue;
                }
                let (_, from_root, _, _, _) =
                    rm_catalog::repo::placement::location_get(conn, &from_loc)
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?
                        .map(|l| (l.0, l.1, l.2, l.3, l.4))
                        .ok_or(client::CmdError::Biz("位置不存在".into()))?;
                let (_, to_root, _, _, _) =
                    rm_catalog::repo::placement::location_get(conn, &to_loc)
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?
                        .map(|l| (l.0, l.1, l.2, l.3, l.4))
                        .ok_or(client::CmdError::Biz("位置不存在".into()))?;
                let src = rm_store::pool::pool_abs(&from_root, &from_rel, &rel);
                let content_rel = rm_store::pool::pool_rel_for(&cid, false);
                let dst = rm_store::pool::pool_abs(&to_root, &to_rel, &content_rel);
                match rm_store::pool::migrate_file(&src, &dst, &cid) {
                    Ok(_) => {
                        let _ = std::fs::remove_file(&src);
                        rm_catalog::repo::pool::set_placement_managed(conn, pid, &to_id, &content_rel)
                            .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                        ok += 1;
                    }
                    Err(_) => fail += 1,
                }
            }
            emit(
                json,
                format!("跨池迁移完成：{ok} 成功，{fail} 失败"),
                serde_json::json!({"ok": ok, "fail": fail}),
            );
            Ok(())
        }
        Some("remove") => {
            let pool = opt_value(rest, "--pool")
                .ok_or(client::CmdError::Usage("pool remove 需要 --pool".into()))?;
            let n = rm_catalog::repo::pool::count_placements(conn, &pool)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if n > 0 {
                return Err(client::CmdError::Blocked(format!(
                    "池非空（{n} 条放置），拒绝移除"
                )));
            }
            conn.execute("DELETE FROM pool WHERE id=?1", [&pool])
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(json, format!("池已移除：{pool}"), serde_json::json!({}));
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "pool 需要 list / create / ingest / move / remove".into(),
        )),
    }
}

// BizError 的 to_message 在 rm_proto 里是 Display；补一个便捷 trait。
trait ToMessage {
    fn to_message(&self) -> String;
}
impl ToMessage for rm_proto::BizError {
    fn to_message(&self) -> String {
        format!("{}: {}", self.code, self.message)
    }
}

// ===== M5：AI 建议 / 隐私 =====

fn cmd_ai(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("suggest") => cmd_ai_suggest(conn, &rest[1..], json),
        Some("privacy") => cmd_ai_privacy(conn, &rest[1..], json),
        Some("run") => {
            // standalone 模式下 ai.run 需要插件宿主（作业线程在插件里）；
            // 这里退化为"同步跑一次 Mock 建议"，方便脚本与验收。
            let kind = opt_value(rest, "--target-kind")
                .ok_or(client::CmdError::Usage("ai run 需要 --target-kind".into()))?;
            let target = opt_value(rest, "--target-id")
                .ok_or(client::CmdError::Usage("ai run 需要 --target-id".into()))?;
            let budget: u32 = opt_value(rest, "--budget")
                .and_then(|s| s.parse().ok())
                .unwrap_or(rm_ai::cost::DEFAULT_MAX_CALLS);
            let mut budget = rm_ai::cost::Budget::new(budget);
            let mock = rm_ai::provider::MockProvider;
            let now = rm_catalog::now();

            let (rel_path, existing_tags, real_kind, real_id) = match kind.as_str() {
                "entry" => {
                    let e = rm_catalog::repo::entry::get(conn, &target)
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?
                        .ok_or_else(|| client::CmdError::Biz(format!("词条不存在：{target}")))?;
                    let tags = rm_catalog::repo::tag::entry_tags(conn, &e.0)
                        .map_err(|e| client::CmdError::Biz(e.to_string()))?
                        .into_iter()
                        .map(|t| t.1)
                        .collect();
                    (e.2.clone(), tags, "entry".to_string(), e.0)
                }
                "metadata" => (target.clone(), vec![], "metadata".to_string(), target.clone()),
                other => {
                    return Err(client::CmdError::Usage(format!(
                        "ai run --target-kind 只支持 entry / metadata（当前 {other}）"
                    )))
                }
            };
            let tier = rm_catalog::repo::suggestion::privacy_resolve(conn, &rel_path)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let extracted = rm_ai::extract::extract(&rel_path);
            let ctx_struct = rm_ai::provider::AnalyzeContext {
                target_kind: real_kind.clone(),
                target_id: real_id.clone(),
                rel_path: rel_path.clone(),
                extracted,
                existing_tags,
                candidate_titles: vec![rel_path.clone()],
            };
            let input_hash = rm_ai::hash_hex(&serde_json::to_string(&ctx_struct).unwrap_or_default());
            let prompt_hash = rm_ai::hash_hex(&format!("v1|mock|{real_kind}|{input_hash}"));
            if !budget.try_spend() {
                return Err(client::CmdError::Biz("AI 预算耗尽".into()));
            }
            let raw = mock
                .analyze(&ctx_struct)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let mut pending = 0u32;
            let mut auto_accepted = 0u32;
            for r in raw {
                if !rm_ai::suggest::kind_known(&r.kind) {
                    continue;
                }
                let sg_id = rm_ai::gen_sg_id();
                let payload = r.payload.to_string();
                rm_catalog::repo::suggestion::insert(
                    conn,
                    &sg_id,
                    &real_kind,
                    &real_id,
                    &r.kind,
                    &payload,
                    r.confidence,
                    mock.id(),
                    &prompt_hash,
                    &input_hash,
                    "ai",
                    now,
                )
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                if rm_ai::suggest::auto_acceptable(&r.kind, r.confidence) {
                    let payload_val: serde_json::Value =
                        serde_json::from_str(&payload).unwrap_or_default();
                    let r = apply_effect_cli(conn, &sg_id, &r.kind, &payload_val, now);
                    match r {
                        Ok(()) => {
                            rm_catalog::repo::suggestion::set_state(conn, &sg_id, "accepted", now)
                                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                            auto_accepted += 1;
                        }
                        Err(e) => {
                            eprintln!("自动采纳失败 {sg_id}: {e}；保留 pending");
                            pending += 1;
                        }
                    }
                } else {
                    pending += 1;
                }
            }
            emit(
                json,
                format!("ai.run 完成：pending={pending} auto_accepted={auto_accepted} tier={tier}"),
                serde_json::json!({
                    "pending": pending, "auto_accepted": auto_accepted,
                    "tier": tier, "budget_used": budget.used,
                }),
            );
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "ai 需要 suggest / run / privacy".into(),
        )),
    }
}

/// CLI 版 apply_effect（与 methods_m5 共享语义；standalone 直连库）。
fn apply_effect_cli(
    conn: &mut rusqlite::Connection,
    sg_id: &str,
    kind: &str,
    payload: &serde_json::Value,
    now: i64,
) -> Result<(), String> {
    let sugg = rm_catalog::repo::suggestion::get(conn, sg_id)
        .map_err(|e| e.to_string())?
        .ok_or("建议不存在")?;
    match kind {
        "tag" => {
            let tag_name = payload["tag_name"].as_str().ok_or("缺 tag_name")?;
            let domain = payload["domain"].as_str().unwrap_or("subject");
            let tag_id = rm_semantic::ensure_tag(conn, tag_name, domain)
                .map_err(|e| e.to_string())?;
            match sugg.target_kind.as_str() {
                "entry" => rm_catalog::repo::tag::entry_tag_apply(
                    conn,
                    &sugg.target_id,
                    &tag_id,
                    &sugg.source,
                    sugg.confidence,
                    Some(&sugg.model),
                    now,
                )
                .map_err(|e| e.to_string())?,
                "version" => rm_catalog::repo::tag::version_tag_apply(
                    conn,
                    &sugg.target_id,
                    &tag_id,
                    &sugg.source,
                    sugg.confidence,
                    Some(&sugg.model),
                    now,
                )
                .map_err(|e| e.to_string())?,
                other => return Err(format!("tag 不支持 target_kind={other}")),
            }
        }
        "metadata" => {}
        other => return Err(format!("CLI apply 暂只支持 tag/metadata（{other}）")),
    }
    Ok(())
}

fn cmd_ai_suggest(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("list") | None => {
            let state = opt_value(rest, "--state");
            let source = opt_value(rest, "--source");
            let target_kind = opt_value(rest, "--target-kind");
            let target_id = opt_value(rest, "--target-id");
            if let Some(st) = &state {
                if !rm_catalog::repo::suggestion::state_known(st) {
                    return Err(client::CmdError::Biz(format!("未知状态：{st}")));
                }
            }
            let f = rm_catalog::repo::suggestion::ListFilter {
                state,
                source,
                target_kind,
                target_id,
                limit: 50,
                offset: 0,
            };
            let rows = rm_catalog::repo::suggestion::list(conn, &f)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let total = rm_catalog::repo::suggestion::count(conn, &f)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|r| serde_json::json!({
                            "id": r.id, "target_kind": r.target_kind, "target_id": r.target_id,
                            "kind": r.kind, "payload": r.payload, "confidence": r.confidence,
                            "model": r.model, "state": r.state, "source": r.source,
                            "created": r.created, "applied_at": r.applied_at,
                        })).collect::<Vec<_>>(),
                        "total": total,
                    }),
                );
            } else {
                println!("建议 {total} 条（显示 {}）：", rows.len());
                for r in rows {
                    println!(
                        "{id}\t{state}\t{kind}\t{conf:.2}\t{src}\t{tk}/{tid}\t{payload}",
                        id = r.id,
                        state = r.state,
                        kind = r.kind,
                        conf = r.confidence,
                        src = r.source,
                        tk = r.target_kind,
                        tid = r.target_id,
                        payload = r.payload,
                    );
                }
            }
            Ok(())
        }
        Some("apply") => {
            let ids = opt_value(rest, "--ids")
                .ok_or(client::CmdError::Usage("apply 需要 --ids id1,id2".into()))?;
            let patch = opt_value(rest, "--patch");
            let now = rm_catalog::now();
            let mut applied = 0u32;
            let mut errors = Vec::new();
            for id in ids.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                match apply_one_cli(conn, id, patch.as_deref(), now) {
                    Ok(true) => applied += 1,
                    Ok(false) => errors.push(format!("{id}: 已是终态")),
                    Err(e) => errors.push(format!("{id}: {e}")),
                }
            }
            emit(
                json,
                format!("已接受 {applied} 条"),
                serde_json::json!({"applied": applied, "errors": errors}),
            );
            Ok(())
        }
        Some("reject") => {
            let ids = opt_value(rest, "--ids")
                .ok_or(client::CmdError::Usage("reject 需要 --ids id1,id2".into()))?;
            let now = rm_catalog::now();
            let mut rejected = 0u32;
            let mut errors = Vec::new();
            for id in ids.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let r = rm_catalog::repo::suggestion::get(conn, id)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?
                    .ok_or_else(|| client::CmdError::Biz(format!("建议不存在：{id}")))?;
                if r.state != "pending" {
                    errors.push(format!("{id}: 状态 {}", r.state));
                    continue;
                }
                rm_catalog::repo::suggestion::set_state(conn, id, "rejected", now)
                    .map_err(|e| client::CmdError::Biz(e.to_string()))?;
                rejected += 1;
            }
            emit(
                json,
                format!("已拒绝 {rejected} 条"),
                serde_json::json!({"rejected": rejected, "errors": errors}),
            );
            Ok(())
        }
        Some("revoke") => {
            let source = opt_value(rest, "--source")
                .ok_or(client::CmdError::Usage("revoke 需要 --source".into()))?;
            let n1 = rm_catalog::repo::tag::entry_tag_revoke_by_source(conn, &source)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let n2 = rm_catalog::repo::tag::version_tag_revoke_by_source(conn, &source)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            let rejected = rm_catalog::repo::suggestion::reject_pending_by_source(conn, &source)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("已撤销 source={source}：{rejected} 条建议、{n} 条贴附回滚", n = n1 + n2),
                serde_json::json!({
                    "revoked": rejected, "rolled_back_tags": n1 + n2, "source": source,
                }),
            );
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "ai suggest 需要 list / apply / reject / revoke".into(),
        )),
    }
}

fn apply_one_cli(
    conn: &mut rusqlite::Connection,
    id: &str,
    patch: Option<&str>,
    now: i64,
) -> Result<bool, String> {
    let sugg = rm_catalog::repo::suggestion::get(conn, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("建议不存在：{id}"))?;
    if sugg.state != "pending" {
        return Ok(false);
    }
    let payload_str = patch.unwrap_or(&sugg.payload);
    let payload: serde_json::Value =
        serde_json::from_str(payload_str).map_err(|e| format!("payload 解析失败：{e}"))?;
    apply_effect_cli(conn, id, &sugg.kind, &payload, now)?;
    rm_catalog::repo::suggestion::set_state(conn, id, "accepted", now)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

fn cmd_ai_privacy(
    conn: &mut rusqlite::Connection,
    rest: &[String],
    json: bool,
) -> Result<(), client::CmdError> {
    match rest.first().map(String::as_str) {
        Some("set") => {
            let prefix = opt_value(rest, "--path-prefix")
                .ok_or(client::CmdError::Usage("privacy set 需要 --path-prefix".into()))?;
            let tier = opt_value(rest, "--tier")
                .ok_or(client::CmdError::Usage("privacy set 需要 --tier".into()))?;
            if !rm_catalog::repo::suggestion::tier_known(&tier) {
                return Err(client::CmdError::Biz(format!(
                    "未知档位：{tier}（local/cloud/hybrid）"
                )));
            }
            rm_catalog::repo::suggestion::privacy_set(conn, &prefix, &tier)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            emit(
                json,
                format!("隐私档位已设：{prefix} → {tier}"),
                serde_json::json!({"path_prefix": prefix, "tier": tier}),
            );
            Ok(())
        }
        Some("list") | None => {
            let rows = rm_catalog::repo::suggestion::privacy_list(conn)
                .map_err(|e| client::CmdError::Biz(e.to_string()))?;
            if json {
                emit(
                    true,
                    String::new(),
                    serde_json::json!({
                        "rows": rows.iter().map(|(p, t)| {
                            serde_json::json!({"path_prefix": p, "tier": t})
                        }).collect::<Vec<_>>(),
                    }),
                );
            } else {
                println!("隐私档位 {} 条：", rows.len());
                for (p, t) in rows {
                    println!("{p}\t{t}");
                }
            }
            Ok(())
        }
        _ => Err(client::CmdError::Usage(
            "ai privacy 需要 set / list".into(),
        )),
    }
}
