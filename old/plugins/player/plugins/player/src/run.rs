//! P5 运行场景（`docs/01` §5.2 责任 2–4 + §5.8 沙箱与生命周期）：
//! `player.run` 拿真实路径 → 沙箱内起进程 → 监控退出码广播事件；
//! `player.run.list`/`player.run.stop` 管理运行表（原表未列，附录 B 登记）。
//!
//! 生命周期（§5.8 生命周期行「player 退出即杀」）：spawn 后子进程 `setsid`
//! 自成会话/进程组（`sandbox::install`），监控线程 100ms `try_wait` 收尸并
//! `player.run.exited`；停用/停机时 `kill_non_background` 杀掉全部
//! `state=running && !allow_background` 的进程组（`allow_background` 授权项
//! 语义为不杀，标注不推荐）。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ling_sdk::{Context, Value};

use crate::error::{from_rm_err, PlayerError};
use crate::sandbox::{self, SandboxOpts};

// ===== 运行表 =====

/// 一次运行的登记行（`player.run.list` 外发）。
#[derive(Debug, Clone)]
pub struct RunRow {
    pub run_id: String,
    pub placement_id: i64,
    pub content_id: Option<String>,
    pub asset_type: String,
    pub path: String,
    pub pid: i32,
    /// `"running"` | `"exited"`（监控线程收尸后翻转）。
    pub state: &'static str,
    pub exit_code: Option<i32>,
    pub allow_background: bool,
    pub isolation: Value,
    pub sandbox_dir: String,
    pub started_ms: u64,
}

impl RunRow {
    fn to_value(&self) -> Value {
        serde_json::json!({
            "run_id": self.run_id,
            "placement_id": self.placement_id,
            "content_id": self.content_id,
            "asset_type": self.asset_type,
            "path": self.path,
            "pid": self.pid,
            "state": self.state,
            "exit_code": self.exit_code,
            "allow_background": self.allow_background,
            "isolation": self.isolation,
            "sandbox_dir": self.sandbox_dir,
            "started_ms": self.started_ms,
        })
    }
}

/// 运行表（方法处理与监控线程共享；停用时据此杀进程组）。
/// P7：`inputs` 持每次运行的 stdin 句柄——`player.remote.input`（§5.15）经
/// 管道把输入事件 JSONL 投递给沙箱进程；进程退出/停机由监控线程清句柄。
#[derive(Default)]
pub struct RunRegistry {
    rows: Mutex<Vec<RunRow>>,
    inputs: Mutex<HashMap<String, ChildStdin>>,
}

impl RunRegistry {
    fn insert(&self, row: RunRow) {
        self.rows.lock().unwrap().push(row);
    }

    fn set_exited(&self, run_id: &str, exit_code: i32) {
        if let Some(r) = self.rows.lock().unwrap().iter_mut().find(|r| r.run_id == run_id) {
            r.state = "exited";
            r.exit_code = Some(exit_code);
        }
        // 进程消亡 → 输入管道关（后续 input → P100「运行不存在或已退出」）。
        self.clear_input(run_id);
    }

    fn get(&self, run_id: &str) -> Option<RunRow> {
        self.rows.lock().unwrap().iter().find(|r| r.run_id == run_id).cloned()
    }

    fn list_values(&self) -> Vec<Value> {
        self.rows.lock().unwrap().iter().map(RunRow::to_value).collect()
    }

    /// 仍在运行的 (pid, allow_background)（停用即杀的筛选面）。
    fn running(&self) -> Vec<(i32, bool)> {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.state == "running")
            .map(|r| (r.pid, r.allow_background))
            .collect()
    }

    fn set_input(&self, run_id: &str, stdin: ChildStdin) {
        self.inputs.lock().unwrap().insert(run_id.to_string(), stdin);
    }

    fn clear_input(&self, run_id: &str) {
        self.inputs.lock().unwrap().remove(run_id);
    }
}

// ===== player.run =====

/// `player.run`：解析目标 → 沙箱启动进程 → 登记 + 监控。
///
/// 错误映射：非 app/game → **P103**；`sandbox_opts` 类型错 → **P100**、
/// allow_dirs 非法/系统根 → **P302**；rm 路径交付失败 → F501/F503 翻译
/// （P101/P102）；文件不存在 → **P101**；隔离构建/启动失败 → **P301**。
pub fn run(ctx: &Context, runs: &Arc<RunRegistry>, args: Value) -> Result<Value, PlayerError> {
    let target = crate::resolve_target(ctx, &args)?;
    if !matches!(target.asset_type.as_str(), "app" | "game") {
        return Err(PlayerError::unsupported_asset(format!(
            "运行仅支持 app/game，该放置为：{}（rel_path {}）",
            target.asset_type, target.rel_path
        )));
    }
    let opts = SandboxOpts::from_args(&args)?;

    // 真实路径（rm 03 §2.6.4；§5.2 责任 1）。
    let out = ctx
        .call(
            "resources-manager",
            "resources-manager.get_local_path",
            serde_json::json!({ "placement_id": target.placement_id }),
        )
        .map_err(|e| from_rm_err(&e))?;
    let path = PathBuf::from(
        out["path"]
            .as_str()
            .ok_or_else(|| PlayerError::content_unavailable("get_local_path 未返回 path"))?,
    );
    if !path.is_file() {
        return Err(PlayerError::content_unavailable(format!(
            "文件不存在：{}",
            path.display()
        )));
    }
    ensure_exec(&path);

    // 沙箱目录（§5.8 文件系统行：独立目录 `<data>/sandbox/<run_id>/`）。
    // `<data>` 与 player.db 同源（data_root，PLAYER_DATA_DIR 可覆盖，附录 B #19）。
    let run_id = new_run_id();
    let sandbox_dir = crate::data_root(ctx).join("sandbox").join(&run_id);
    let tmp_dir = sandbox_dir.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| {
        PlayerError::sandbox_start_failed(format!("建沙箱目录失败：{}", e))
    })?;

    let mut prepared = sandbox::prepare(&sandbox_dir, &path, &opts)?;
    let isolation = prepared.isolation().to_value();

    // 输出落沙箱内文件（父进程开好句柄，子进程继承——绕开路径再校验）。
    let out_file = fs::File::create(sandbox_dir.join("stdout.log")).map_err(|e| {
        PlayerError::sandbox_start_failed(format!("建 stdout.log 失败：{}", e))
    })?;
    let err_file = fs::File::create(sandbox_dir.join("stderr.log")).map_err(|e| {
        PlayerError::sandbox_start_failed(format!("建 stderr.log 失败：{}", e))
    })?;

    let mut cmd = Command::new(&path);
    cmd.current_dir(&sandbox_dir);
    cmd.env_clear();
    cmd.env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    cmd.env("HOME", &sandbox_dir);
    cmd.env("TMPDIR", &tmp_dir);
    cmd.env("LANG", "C");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::from(out_file));
    cmd.stderr(Stdio::from(err_file));
    sandbox::install(&mut prepared, &mut cmd)?;
    let mut child = cmd.spawn().map_err(|e| {
        prepared.close_ruleset();
        PlayerError::sandbox_start_failed(format!("沙箱启动失败：{e}"))
    })?;
    prepared.close_ruleset();

    let pid = child.id() as i32;
    runs.insert(RunRow {
        run_id: run_id.clone(),
        placement_id: target.placement_id,
        content_id: target.content_id,
        asset_type: target.asset_type,
        path: path.to_string_lossy().into_owned(),
        pid,
        state: "running",
        exit_code: None,
        allow_background: opts.allow_background,
        isolation: isolation.clone(),
        sandbox_dir: sandbox_dir.to_string_lossy().into_owned(),
        started_ms: now_ms(),
    });
    // P7：stdin 管道句柄入表（§5.15 输入事件投递面；退出时监控线程 set_exited 清）。
    if let Some(stdin) = child.stdin.take() {
        runs.set_input(&run_id, stdin);
    }
    spawn_monitor(ctx.clone(), Arc::clone(runs), run_id.clone(), child);

    Ok(serde_json::json!({
        "run_id": run_id,
        "pid": pid,
        "path": path.to_string_lossy(),
        "sandbox_dir": sandbox_dir.to_string_lossy(),
        "isolation": isolation,
    }))
}

/// 监控线程：100ms try_wait → 翻状态 + 广播 `player.run.exited`（§5.12 事件表）。
/// 停机后 emit 失败无害（`let _ =`）；线程随进程退出自然终结。
fn spawn_monitor(ctx: Context, runs: Arc<RunRegistry>, run_id: String, mut child: Child) {
    std::thread::spawn(move || loop {
        match child.try_wait() {
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Ok(Some(status)) => {
                let exit_code = status.code().unwrap_or_else(|| {
                    // 被信号杀（Unix 才有这个概念；Windows 上 code() 已覆盖所有情况）。
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        return status.signal().map(|s| 128 + s).unwrap_or(-1);
                    }
                    #[cfg(not(unix))]
                    {
                        -1
                    }
                });
                runs.set_exited(&run_id, exit_code);
                let _ = ctx.emit(
                    "player.run.exited",
                    serde_json::json!({ "run_id": run_id, "exit_code": exit_code }),
                );
                break;
            }
            Err(_) => {
                runs.set_exited(&run_id, -1);
                let _ = ctx.emit(
                    "player.run.exited",
                    serde_json::json!({ "run_id": run_id, "exit_code": -1 }),
                );
                break;
            }
        }
    });
}

// ===== player.run.list / player.run.stop =====

/// `player.run.list`：全部运行行（插入序，先启动的在前）。
pub fn list(runs: &RunRegistry) -> Value {
    serde_json::json!({ "rows": runs.list_values() })
}

/// `player.run.stop`：杀进程组（显式停，含 allow_background 的）。
/// 未知 run_id → **P100**；已退出 → `stopped:false` 不报错。
pub fn stop(runs: &RunRegistry, args: &Value) -> Result<Value, PlayerError> {
    let run_id = crate::str_field(args, "run_id")?;
    let Some(row) = runs.get(&run_id) else {
        return Err(PlayerError::bad_param(format!("未知 run_id：{run_id}")));
    };
    if row.state == "running" {
        sandbox::kill_process_group(row.pid);
    }
    Ok(serde_json::json!({ "run_id": run_id, "stopped": row.state == "running" }))
}

// ===== P7：控制平面消费面（remote 运行行/输入/日志；附录 B 登记） =====

/// 单行取值（`GET /remote/run/{id}` 与发起侧 `player.remote.status`）。
/// 未知 run_id → **P100**。
pub fn get_value(runs: &RunRegistry, run_id: &str) -> Result<Value, PlayerError> {
    runs.get(run_id)
        .map(|r| r.to_value())
        .ok_or_else(|| PlayerError::bad_param(format!("未知 run_id：{run_id}")))
}

/// §5.15 输入事件投递（**仅 run 会话命名空间**——视频会话 id 不在运行表，
/// 天然拒绝，普通投屏不接受输入的安全面由该命名空间承载）。
///
/// transport（P7 首版登记）：事件逐条序列化为**一行 JSON**（JSONL）写入
/// 沙箱进程 stdin——四类事件统一格式，便于对端后续注入 GUI/终端时解析；
/// text 不裸写（协议行一致优先）。未知/已退出 run → **P100**；管道断开
/// （进程刚退）→ **P100** 并带原因。
pub fn write_input(runs: &RunRegistry, run_id: &str, event: &Value) -> Result<(), PlayerError> {
    let Some(row) = runs.get(run_id) else {
        return Err(PlayerError::bad_param(format!("未知 run_id：{run_id}")));
    };
    let mut line = serde_json::to_vec(event)
        .map_err(|e| PlayerError::bad_param(format!("事件序列化失败：{e}")))?;
    line.push(b'\n');
    let mut inputs = runs.inputs.lock().unwrap();
    let Some(stdin) = inputs.get_mut(run_id) else {
        return Err(PlayerError::bad_param(format!(
            "运行不存在或已退出（state={}）：{run_id}",
            row.state
        )));
    };
    stdin
        .write_all(&line)
        .and_then(|_| stdin.flush())
        .map_err(|e| {
            PlayerError::bad_param(format!("输入管道写入失败（进程可能已退出）：{e}"))
        })
}

/// 沙箱 stdout/stderr 日志尾（`GET /remote/run/{id}/log`——画面回传
/// headless 环境下的降级观测面，真窗口捕获留窗口环境，附录 B 登记）。
/// `stream` 仅 `stdout`/`stderr`；未知 run → **P100**、流名错 → **P100**；
/// 日志尚未产生（文件不存在）→ 空串不报错。
pub fn log_tail(runs: &RunRegistry, run_id: &str, stream: &str) -> Result<Value, PlayerError> {
    if stream != "stdout" && stream != "stderr" {
        return Err(PlayerError::bad_param(format!(
            "stream 须为 stdout/stderr，收到：{stream}"
        )));
    }
    let row = runs
        .get(run_id)
        .ok_or_else(|| PlayerError::bad_param(format!("未知 run_id：{run_id}")))?;
    let path = Path::new(&row.sandbox_dir).join(format!("{stream}.log"));
    let data = fs::read(&path).unwrap_or_default();
    const TAIL: usize = 8 * 1024;
    let start = data.len().saturating_sub(TAIL);
    let tail = String::from_utf8_lossy(&data[start..]).into_owned();
    Ok(serde_json::json!({
        "run_id": run_id,
        "stream": stream,
        "data": tail,
        "bytes": data.len(),
    }))
}

/// 停用/停机即杀（§5.8 生命周期行）：只杀 `!allow_background` 的运行行。
/// 幂等：已退出的组 kill 得 ESRCH 忽略。
pub fn kill_non_background(runs: &RunRegistry) {
    for (pid, allow_background) in runs.running() {
        if !allow_background {
            sandbox::kill_process_group(pid);
        }
    }
}

// ===== 工具 =====

/// 12 字节随机 → `run-<hex>`（getrandom；失败退时间戳，够本进程内不撞）。
fn new_run_id() -> String {
    let mut b = [0u8; 12];
    if getrandom::getrandom(&mut b).is_err() {
        b[..8].copy_from_slice(&now_ms().to_be_bytes());
    }
    let mut s = String::with_capacity(4 + b.len() * 2);
    s.push_str("run-");
    for x in b {
        s.push_str(&format!("{:02x}", x));
    }
    s
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 无执行位则 best-effort 补上（shell 脚本类夹具；失败留给 spawn 报 P301）。
/// 仅 Unix 有可执行位概念；Windows 上 .exe/.bat 天然可执行，函数退化为 no-op。
#[cfg(unix)]
fn ensure_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(md) = fs::metadata(path) {
        if md.mode() & 0o111 == 0 {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(md.mode() | 0o111));
        }
    }
}
#[cfg(not(unix))]
fn ensure_exec(_path: &Path) {}
