//! P5 沙箱隔离构建（`docs/01` §5.8 默认全隔离 + 权限清单式授权）。
//!
//! **平台说明**：
//! - Linux:   完整 Landlock + seccomp + setsid + rlimit + pre_exec（见下方实现）。
//! - 非 Linux（Windows/macOS/Android 等）: 无 OS 级沙箱机制，
//!   `prepare()` 返回 `fs="degraded"` / `network="granted"` 如实上报，
//!   `install()` 退化为 no-op，`kill_process_group()` 用 taskkill/Process.kill。
//!
//! 跨平台稳定面 = `SandboxOpts` / `Isolation` / `Prepared` 结构体 + 四个公开函数签名。

use std::path::{Path, PathBuf};
use std::process::Command;

use ling_sdk::Value;

use crate::error::PlayerError;

// ===== 沙箱权限清单（§5.8 权限表）—— 跨平台 =====

/// `sandbox_opts` 解析结果（默认 = 全隔离，三项授权皆为关）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxOpts {
    /// 允许读写的目录（§5.8「允许访问指定目录」，如游戏存档）。
    pub allow_dirs: Vec<PathBuf>,
    /// 允许联网（false = Linux 上 seccomp 拦网络；非 Linux 上本字段无作用）。
    pub allow_network: bool,
    /// 允许后台运行（player 退出不杀；§5.8 标注不推荐）。
    pub allow_background: bool,
}

impl Default for SandboxOpts {
    fn default() -> Self {
        Self {
            allow_dirs: Vec::new(),
            allow_network: false,
            allow_background: false,
        }
    }
}

/// 系统关键目录：不允许经 `allow_dirs` 整体授权（P302）。
/// （子路径如 `/root/saves` 合法——只拒「恰好等于」这些根。）
#[cfg(unix)]
const DENIED_ALLOW_ROOTS: &[&str] = &[
    "/",
    "/etc",
    "/proc",
    "/sys",
    "/dev",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/boot",
    "/var",
    "/root",
    "/home",
];

#[cfg(not(unix))]
const DENIED_ALLOW_ROOTS: &[&str] = &[
    // Windows: 系统根盘（用户一般用 C:\Program Files 这类路径显式授权，
    // 不会「恰好等于」某个系统根——保守留空即可，非 Unix 沙箱本来就是降级态）。
];

impl SandboxOpts {
    /// 从 `player.run` 入参解析 `sandbox_opts`（缺省/null → 默认全隔离）。
    ///
    /// 校验（错误映射）：
    /// - 非对象/字段类型错 → **P100**；
    /// - allow_dirs 非绝对路径/不存在/非目录/恰为系统根 → **P302**。
    pub fn from_args(args: &Value) -> Result<Self, PlayerError> {
        let Some(raw) = args.get("sandbox_opts") else {
            return Ok(Self::default());
        };
        if raw.is_null() {
            return Ok(Self::default());
        }
        let obj = raw
            .as_object()
            .ok_or_else(|| PlayerError::bad_param("sandbox_opts 须为对象"))?;

        let mut allow_dirs = Vec::new();
        if let Some(dirs) = obj.get("allow_dirs") {
            let arr = dirs
                .as_array()
                .ok_or_else(|| PlayerError::bad_param("sandbox_opts.allow_dirs 须为数组"))?;
            for d in arr {
                let s = d
                    .as_str()
                    .ok_or_else(|| PlayerError::bad_param("sandbox_opts.allow_dirs 须为字符串数组"))?;
                allow_dirs.push(validate_allow_dir(s)?);
            }
        }
        let allow_network = match obj.get("allow_network") {
            None | Some(Value::Null) => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| PlayerError::bad_param("sandbox_opts.allow_network 须为布尔"))?,
        };
        let allow_background = match obj.get("allow_background") {
            None | Some(Value::Null) => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| PlayerError::bad_param("sandbox_opts.allow_background 须为布尔"))?,
        };
        Ok(Self {
            allow_dirs,
            allow_network,
            allow_background,
        })
    }
}

/// allow_dirs 校验：绝对路径 + 存在 + 是目录 + 非系统关键根（canonicalize 后比对）。
fn validate_allow_dir(s: &str) -> Result<PathBuf, PlayerError> {
    let p = Path::new(s);
    if !p.is_absolute() {
        return Err(PlayerError::sandbox_path_denied(format!(
            "allow_dirs 须为绝对路径：{s}"
        )));
    }
    let canon = std::fs::canonicalize(p).map_err(|_| {
        PlayerError::sandbox_path_denied(format!("allow_dirs 不存在或不可达：{s}"))
    })?;
    if !canon.is_dir() {
        return Err(PlayerError::sandbox_path_denied(format!(
            "allow_dirs 不是目录：{s}"
        )));
    }
    let canon_str = canon.to_string_lossy();
    if DENIED_ALLOW_ROOTS.contains(&canon_str.as_ref()) {
        return Err(PlayerError::sandbox_path_denied(format!(
            "系统关键目录不可整体授权：{canon_str}"
        )));
    }
    Ok(canon)
}

// ===== 隔离报告（player.run 响应 isola 实装增补，附录 B 登记）—— 跨平台 =====

/// 各隔离项实装状态（如实上报；降级不隐藏）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isolation {
    /// `"landlock"` | `"degraded"`（内核不支持 Landlock 时降级，附录 B 登记）。
    pub fs: &'static str,
    /// `"seccomp"` | `"granted"`（allow_network 授权） | `"degraded"`（架构不支持）。
    pub network: &'static str,
}

impl Isolation {
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "fs": self.fs,
            "network": self.network,
            "no_new_privs": self.fs != "degraded",
            "session": if self.fs != "degraded" { "setsid" } else { "none" },
        })
    }
}

// ===== Prepared 结构体——跨平台 =====

#[cfg(unix)]
use unix_impl::{Prepared, SockFilter};

#[cfg(not(unix))]
#[derive(Debug, Clone)]
pub struct Prepared {
    isolation: Isolation,
}

#[cfg(not(unix))]
impl Prepared {
    pub fn isolation(&self) -> Isolation {
        self.isolation.clone()
    }
    pub fn close_ruleset(&mut self) {
        // Windows 无 fd，no-op
    }
}

// ===== 公开 API —— 跨平台签名；内部实现按平台分发 =====

/// 构建隔离件（父进程）。
/// - Unix:  构建 Landlock 规则集 + seccomp 过滤器（可报 P301/P302）。
/// - 非 Unix: 无 OS 级沙箱，直接返回 `fs="degraded"` / `network="granted"`。
pub fn prepare(
    sandbox_dir: &Path,
    program: &Path,
    opts: &SandboxOpts,
) -> Result<Prepared, PlayerError> {
    #[cfg(unix)]
    {
        return unix_impl::prepare(sandbox_dir, program, opts);
    }
    #[cfg(not(unix))]
    {
        let _ = (sandbox_dir, program, opts);
        Ok(Prepared {
            isolation: Isolation {
                fs: "degraded",
                network: if opts.allow_network { "granted" } else { "degraded" },
            },
        })
    }
}

/// 把隔离件装进 `Command`。
/// - Unix:  通过 `pre_exec` 在子进程 exec 前套 Landlock/seccomp/setsid/rlimit。
/// - 非 Unix: no-op，返回 Ok(())。
pub fn install(prepared: &mut Prepared, cmd: &mut Command) -> Result<(), PlayerError> {
    #[cfg(unix)]
    {
        return unix_impl::install(prepared, cmd);
    }
    #[cfg(not(unix))]
    {
        let _ = (prepared, cmd);
        Ok(())
    }
}

/// 探测进程（组）是否存活。
/// - Unix: `kill(-pid, 0)`。
/// - 非 Unix: 用 tasklist 探测。
pub fn process_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        return unix_impl::process_alive(pid);
    }
    #[cfg(not(unix))]
    {
        return windows_impl::process_alive(pid);
    }
}

/// 杀进程（组）。
/// - Unix: `kill(-pid, SIGKILL)` + `kill(pid, SIGKILL)`。
/// - 非 Unix: taskkill /PID <pid> /T /F。
pub fn kill_process_group(pid: i32) {
    #[cfg(unix)]
    {
        unix_impl::kill_process_group(pid);
    }
    #[cfg(not(unix))]
    {
        windows_impl::kill_process_group(pid);
    }
}

// ===========================================================================
// Unix 专属实现（Landlock / seccomp / setsid / pre_exec）
// ===========================================================================

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use std::ffi::CString;

    // ---- Linux ABI 常量 ----

    const SYS_LANDLOCK_CREATE_RULESET: i64 = 444;
    const SYS_LANDLOCK_ADD_RULE: i64 = 445;
    const SYS_LANDLOCK_RESTRICT_SELF: i64 = 446;

    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
    const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
    const LANDLOCK_ABI1_MASK: u64 = 0x1FFF;

    const RO_X: u64 = 0b01101;
    const DEV_RW: u64 = 0b00110;
    const NON_DIR_MASK: u64 = 0b0111;

    const PR_SET_NO_NEW_PRIVS: i32 = 38;
    const PR_SET_SECCOMP: i32 = 22;
    const SECCOMP_MODE_FILTER: u64 = 2;

    const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const EPERM: u32 = 1;

    extern "C" {
        fn setsid() -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
        fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
        fn syscall(num: i64, ...) -> i64;
        fn prctl(option: i32, ...) -> i32;
        fn open(path: *const i8, flags: i32, ...) -> i32;
        fn close(fd: i32) -> i32;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RLimit { cur: u64, max: u64 }

    const RLIMIT_CORE: i32 = 4;
    const RLIMIT_NOFILE: i32 = 7;

    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;
    const SC_NR: u32 = 0;
    const SC_ARCH: u32 = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SockFilter {
        pub code: u16, pub jt: u8, pub jf: u8, pub k: u32,
    }

    #[repr(C)]
    struct SockFprog { len: u16, filter: *const SockFilter }

    #[cfg(target_arch = "x86_64")]
    fn audit_arch() -> Option<u32> { Some(0xC000_003E) }
    #[cfg(target_arch = "aarch64")]
    fn audit_arch() -> Option<u32> { Some(0xC000_00B7) }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn audit_arch() -> Option<u32> { None }

    #[cfg(target_arch = "x86_64")]
    const DENIED_SYSCALLS: &[u32] = &[41, 42, 53, 43, 288, 425, 426, 427];
    #[cfg(target_arch = "aarch64")]
    const DENIED_SYSCALLS: &[u32] = &[198, 203, 199, 202, 242, 425, 426, 427];
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const DENIED_SYSCALLS: &[u32] = &[];

    // ---- Prepared ----

    pub struct Prepared {
        pub isolation: Isolation,
        pub ruleset_fd: Option<i32>,
        pub seccomp: Option<Vec<SockFilter>>,
    }

    impl Prepared {
        pub fn isolation(&self) -> Isolation { self.isolation.clone() }
        pub fn close_ruleset(&mut self) {
            if let Some(fd) = self.ruleset_fd.take() {
                unsafe { let _ = close(fd); }
            }
        }
    }

    // ---- prepare ----

    pub fn prepare(
        sandbox_dir: &Path,
        program: &Path,
        opts: &SandboxOpts,
    ) -> Result<Prepared, PlayerError> {
        let abi = query_landlock_abi();
        let (ruleset_fd, fs_mode) = match abi {
            None => (None, "degraded"),
            Some(_) => {
                let fd = build_ruleset(sandbox_dir, program, opts)?;
                (Some(fd), "landlock")
            }
        };
        let seccomp = if opts.allow_network {
            None
        } else {
            match audit_arch() {
                Some(arch) => Some(build_deny_network_filter(arch)),
                None => None,
            }
        };
        let network_mode = if opts.allow_network {
            "granted"
        } else if seccomp.is_some() {
            "seccomp"
        } else {
            "degraded"
        };
        Ok(Prepared {
            isolation: Isolation { fs: fs_mode, network: network_mode },
            ruleset_fd,
            seccomp,
        })
    }

    fn query_landlock_abi() -> Option<u32> {
        let ret = unsafe {
            syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                std::ptr::null::<u8>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION as i64,
            )
        };
        if ret < 0 { None } else { Some(ret as u32) }
    }

    fn build_ruleset(
        sandbox_dir: &Path,
        program: &Path,
        opts: &SandboxOpts,
    ) -> Result<i32, PlayerError> {
        let handled: u64 = LANDLOCK_ABI1_MASK;
        let fd = unsafe {
            syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &handled as *const u64,
                std::mem::size_of::<u64>(),
                0i64,
            )
        };
        if fd < 0 {
            return Err(PlayerError::sandbox_start_failed(format!(
                "landlock_create_ruleset 失败：{}", last_errno_msg()
            )));
        }
        let fd = fd as i32;

        add_path_rule(fd, sandbox_dir, LANDLOCK_ABI1_MASK)?;
        if let Some(parent) = program.parent() {
            if parent != Path::new("") && parent != Path::new("/") {
                add_path_rule(fd, parent, RO_X)?;
            }
        }
        add_path_rule(fd, program, RO_X)?;
        for root in ["/bin", "/sbin", "/lib", "/lib64", "/usr", "/etc"] {
            let p = Path::new(root);
            if p.exists() { add_path_rule(fd, p, RO_X)?; }
        }
        for p in ["/proc", "/sys"] {
            if Path::new(p).exists() { add_path_rule(fd, Path::new(p), RO_X)?; }
        }
        for dev in ["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random", "/dev/tty"] {
            if Path::new(dev).exists() { add_path_rule(fd, Path::new(dev), DEV_RW)?; }
        }
        for d in &opts.allow_dirs {
            add_path_rule(fd, d, LANDLOCK_ABI1_MASK)?;
        }
        Ok(fd)
    }

    fn add_path_rule(ruleset_fd: i32, path: &Path, allowed: u64) -> Result<(), PlayerError> {
        let c_path = cstring_of_path(path)?;
        const O_PATH: i32 = 0o10000000;
        const O_CLOEXEC: i32 = 0o2000000;
        let path_fd = unsafe { open(c_path.as_ptr(), O_PATH | O_CLOEXEC) };
        if path_fd < 0 {
            return Err(PlayerError::sandbox_start_failed(format!(
                "打开规则路径失败 {}：{}", path.display(), last_errno_msg()
            )));
        }
        let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
        let allowed = if is_dir { allowed } else { allowed & NON_DIR_MASK };
        if allowed == 0 {
            unsafe { let _ = close(path_fd); }
            return Err(PlayerError::sandbox_start_failed(format!(
                "规则路径无可授权位 {}", path.display()
            )));
        }
        #[repr(C)]
        struct PathBeneathAttr { allowed_access: u64, parent_fd: i32 }
        let attr = PathBeneathAttr { allowed_access: allowed, parent_fd: path_fd };
        let ret = unsafe {
            syscall(
                SYS_LANDLOCK_ADD_RULE,
                ruleset_fd as i64,
                LANDLOCK_RULE_PATH_BENEATH as i64,
                &attr as *const PathBeneathAttr,
                0i64,
            )
        };
        unsafe { let _ = close(path_fd); }
        if ret < 0 {
            return Err(PlayerError::sandbox_start_failed(format!(
                "landlock_add_rule 失败 {}：{}", path.display(), last_errno_msg()
            )));
        }
        Ok(())
    }

    fn build_deny_network_filter(arch: u32) -> Vec<SockFilter> {
        let mut f = Vec::with_capacity(4 + 2 * DENIED_SYSCALLS.len());
        f.push(SockFilter { code: BPF_LD | BPF_W | BPF_ABS, jt: 0, jf: 0, k: SC_ARCH });
        f.push(SockFilter { code: BPF_JMP | BPF_JEQ | BPF_K, jt: 1, jf: 0, k: arch });
        f.push(SockFilter { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: SECCOMP_RET_KILL_PROCESS });
        f.push(SockFilter { code: BPF_LD | BPF_W | BPF_ABS, jt: 0, jf: 0, k: SC_NR });
        for nr in DENIED_SYSCALLS {
            f.push(SockFilter { code: BPF_JMP | BPF_JEQ | BPF_K, jt: 0, jf: 1, k: *nr });
            f.push(SockFilter { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: SECCOMP_RET_ERRNO | EPERM });
        }
        f.push(SockFilter { code: BPF_RET | BPF_K, jt: 0, jf: 0, k: SECCOMP_RET_ALLOW });
        f
    }

    // ---- install ----

    pub fn install(prepared: &mut Prepared, cmd: &mut Command) -> Result<(), PlayerError> {
        use std::os::unix::process::CommandExt;
        let ruleset_fd = prepared.ruleset_fd;
        let seccomp = prepared.seccomp.clone();
        unsafe {
            cmd.pre_exec(move || -> std::io::Result<()> {
                if setsid() < 0 { return Err(std::io::Error::last_os_error()); }
                if prctl(PR_SET_NO_NEW_PRIVS, 1u64, 0u64, 0u64, 0u64) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(fd) = ruleset_fd {
                    if syscall(SYS_LANDLOCK_RESTRICT_SELF, fd as i64, 0i64) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if let Some(filter) = &seccomp {
                    let prog = SockFprog {
                        len: filter.len() as u16,
                        filter: filter.as_ptr(),
                    };
                    if prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog as *const SockFprog as u64, 0u64, 0u64) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                let core = RLimit { cur: 0, max: 0 };
                let _ = setrlimit(RLIMIT_CORE, &core);
                let nofile = RLimit { cur: 1024, max: 1024 };
                let _ = setrlimit(RLIMIT_NOFILE, &nofile);
                Ok(())
            });
        }
        Ok(())
    }

    pub fn process_alive(pid: i32) -> bool {
        unsafe { kill(-pid, 0) == 0 }
    }

    pub fn kill_process_group(pid: i32) {
        const SIGKILL: i32 = 9;
        unsafe {
            let _ = kill(-pid, SIGKILL);
            let _ = kill(pid, SIGKILL);
        }
    }

    // ---- helpers ----

    fn cstring_of_path(p: &Path) -> Result<CString, PlayerError> {
        let s = p.to_str().ok_or_else(|| {
            PlayerError::sandbox_start_failed(format!("路径含非法 UTF-8：{}", p.display()))
        })?;
        CString::new(s).map_err(|_| {
            PlayerError::sandbox_start_failed(format!("路径含内嵌 NUL：{s}"))
        })
    }

    fn last_errno_msg() -> String {
        std::io::Error::last_os_error().to_string()
    }
}

// ===========================================================================
// Windows stub 实现
// ===========================================================================

#[cfg(windows)]
mod windows_impl {
    /// 探测进程是否存活（Windows 无 kill(-pid) 等价物，用 tasklist）。
    pub fn process_alive(pid: i32) -> bool {
        use std::process::Command;
        let out = Command::new("tasklist.exe")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")),
            Err(_) => false,
        }
    }

    /// 杀进程树（taskkill /T 杀子进程；/F 强制）。
    pub fn kill_process_group(pid: i32) {
        use std::process::Command;
        let _ = Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

// 非 Unix 非 Windows（macOS/Android 等）也给 stub，避免链接缺符号
#[cfg(not(any(unix, windows)))]
mod windows_impl {
    pub fn process_alive(_pid: i32) -> bool { false }
    pub fn kill_process_group(_pid: i32) {}
}

// ===========================================================================
// 单元测试（仅 Unix，因为依赖 pre_exec / setsid / Landlock 等系统调用）
// ===========================================================================

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn t1_opts_default_is_full_isolation() {
        let o = SandboxOpts::from_args(&serde_json::json!({})).unwrap();
        assert_eq!(o, SandboxOpts::default(), "缺省 sandbox_opts = 全隔离");
        assert!(o.allow_dirs.is_empty());
        assert!(!o.allow_network, "缺省禁止联网（§5.8 网络行）");
        assert!(!o.allow_background, "缺省退出即杀（§5.8 生命周期行）");
        let o2 = SandboxOpts::from_args(&serde_json::json!({"sandbox_opts": null})).unwrap();
        assert_eq!(o2, SandboxOpts::default());
    }

    #[test]
    fn t2_opts_parse_and_type_checks() {
        let o = SandboxOpts::from_args(&serde_json::json!({
            "sandbox_opts": {"allow_network": true, "allow_background": true}
        })).unwrap();
        assert!(o.allow_network && o.allow_background);
        let e = SandboxOpts::from_args(&serde_json::json!({"sandbox_opts": []})).unwrap_err();
        assert_eq!(e.code, "P100");
    }

    #[test]
    fn t3_allow_dirs_p302_validation() {
        let e = SandboxOpts::from_args(&serde_json::json!({
            "sandbox_opts": {"allow_dirs": ["relative/path"]}
        })).unwrap_err();
        assert_eq!(e.code, "P302", "相对路径：{}", e.message);

        let e = SandboxOpts::from_args(&serde_json::json!({
            "sandbox_opts": {"allow_dirs": ["/definitely/not/exists/p5"]}
        })).unwrap_err();
        assert_eq!(e.code, "P302");

        for root in ["/", "/etc", "/proc", "/dev", "/usr", "/lib"] {
            let e = SandboxOpts::from_args(&serde_json::json!({
                "sandbox_opts": {"allow_dirs": [root]}
            })).unwrap_err();
            assert_eq!(e.code, "P302", "{root} 应拒绝：{}", e.message);
        }
        let o = SandboxOpts::from_args(&serde_json::json!({
            "sandbox_opts": {"allow_dirs": ["/tmp"]}
        })).unwrap();
        assert_eq!(o.allow_dirs.len(), 1);
        assert_eq!(o.allow_dirs[0], PathBuf::from("/tmp"));
    }

    #[test]
    fn t5_prepare_reports_isolation_honestly() {
        let sandbox = std::env::temp_dir().join("p5-sbx-unit");
        std::fs::create_dir_all(&sandbox).unwrap();
        let program = Path::new("/bin/sh");

        let mut p = prepare(&sandbox, program, &SandboxOpts::default()).unwrap();
        let iso = p.isolation();
        assert_eq!(iso.network, "seccomp", "默认装网络过滤");
        assert!(iso.fs == "landlock" || iso.fs == "degraded", "fs 如实上报");
        p.close_ruleset();

        let mut p2 = prepare(&sandbox, program,
            &SandboxOpts { allow_network: true, ..Default::default() }).unwrap();
        assert_eq!(p2.isolation().network, "granted");
        p2.close_ruleset();
    }

    #[test]
    fn t6_spawn_sandboxed_sh() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Stdio;

        let tmp = std::env::temp_dir().join(format!("p5-sbx-spawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let sandbox = tmp.join("home");
        let prog_dir = tmp.join("prog");
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::create_dir_all(&prog_dir).unwrap();
        let prog = prog_dir.join("sh-ok");
        std::fs::write(&prog, "#!/bin/sh\necho hi > out.txt\nexit 3\n").unwrap();
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut p = prepare(&sandbox, &prog, &SandboxOpts::default()).unwrap();
        let mut cmd = Command::new(&prog);
        cmd.current_dir(&sandbox).env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::null()).stderr(Stdio::null());
        install(&mut p, &mut cmd).unwrap();
        let mut child = cmd.spawn().unwrap_or_else(|e| panic!("沙箱 spawn 失败：{e}"));
        p.close_ruleset();
        let st = child.wait().unwrap();
        assert_eq!(st.code(), Some(3), "退出码原样带回");
        let out = std::fs::read_to_string(sandbox.join("out.txt")).unwrap();
        assert_eq!(out.trim(), "hi", "沙箱内写文件成功");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
