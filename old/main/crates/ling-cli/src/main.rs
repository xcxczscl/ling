//! `ling` 命令行（`docs/04` §7）：`serve` 前台启动宿主 + 管理端口；
//! 其余子命令经管理端口（`127.0.0.1:<registry.json 端口>` + `session.json` 口令）
//! 连接正在运行的宿主。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ling_kernel::admin;
use ling_kernel::Kernel;
use ling_sdk::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = run(&args);
    std::process::exit(code);
}

fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("ling {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("--help") | Some("-h") | None => {
            print_help();
            0
        }
        Some("serve") => cmd_serve(&args[1..]),
        Some("logs") => match args.get(1) {
            Some(id) => cmd_request(json!({
                "method": "kernel.logs.read",
                "params": {"plugin": id, "limit": args.get(2).and_then(|l| l.parse().ok()).unwrap_or(50)}
            })),
            None => {
                eprintln!("用法：ling logs <编号> [条数]");
                2
            }
        },
        Some("plugins") => cmd_request(json!({"method": "kernel.plugins.list"})),
        Some("install") => match args.get(1) {
            Some(path) => {
                let abs = absolute(path);
                cmd_request(json!({
                    "method": "kernel.plugins.install",
                    "params": {"path": abs.display().to_string()}
                }))
            }
            None => {
                eprintln!("用法：ling install <目录或zip>");
                2
            }
        },
        Some("uninstall") => match args.get(1) {
            Some(id) => {
                let purge = args.iter().any(|a| a == "--purge");
                cmd_request(json!({
                    "method": "kernel.plugins.uninstall",
                    "params": {"id": id, "purge": purge}
                }))
            }
            None => {
                eprintln!("用法：ling uninstall <编号> [--purge]");
                2
            }
        },
        Some(op @ ("enable" | "disable" | "reload" | "restart")) => match args.get(1) {
            Some(id) => cmd_request(json!({
                "method": format!("kernel.plugins.{op}"),
                "params": {"id": id}
            })),
            None => {
                eprintln!("用法：ling {op} <编号>");
                2
            }
        },
        Some("shutdown") => cmd_request(json!({"method": "kernel.shutdown"})),
        Some(other) => {
            eprintln!("未知命令：{other}");
            print_help();
            2
        }
    }
}

fn print_help() {
    println!(
        "ling — 一切皆插件的宿主\n\
         \n\
         用法：\n\
         \x20 ling serve [--builtin-dir <路径>]\n\
         \x20                         前台启动宿主（首启自动安装内置三件套）\n\
         \x20 ling plugins               列出已安装插件及状态\n\
         \x20 ling install <目录|zip>    安装插件并触发调和\n\
         \x20 ling uninstall <编号>      卸载（--purge 连数据删除）\n\
         \x20 ling enable|disable <编号> 启用/停用\n\
         \x20 ling reload <编号>         热更新（新版本已在插件目录）\n\
         \x20 ling restart <编号>        重启插件\n\
         \x20 ling logs <编号> [条数]    读插件日志（需 core-log）\n\
         \x20 ling shutdown              优雅停止宿主\n\
         \x20 ling --version             输出版本号\n\
         \n\
         数据根目录：LING_HOME 覆盖，否则 $XDG_DATA_HOME/ling/（缺省 ~/.local/share/ling/）"
    );
}

fn ling_data_root() -> PathBuf {
    if let Some(home) = std::env::var_os("LING_HOME") {
        return PathBuf::from(home);
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("ling");
    }
    // Windows 优先用 %LOCALAPPDATA%\ling，其次 %USERPROFILE%\.local\share\ling
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("ling");
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join(".local/share/ling")
}

fn absolute(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

fn cmd_serve(args: &[String]) -> i32 {
    let data_root = ling_data_root();
    let kernel = match Kernel::new(&data_root) {
        Ok(k) => std::sync::Arc::new(k),
        Err(e) => {
            eprintln!("启动内核失败：{e}");
            return 1;
        }
    };
    // 管理端口先起（session.json 就绪后内置插件才能校验口令）。
    let server = match admin::start(kernel.clone(), 7900) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("启动管理端口失败：{e}");
            return 1;
        }
    };
    // 内置插件目录（docs/06 §2）：--builtin-dir 覆盖，否则从 cwd 向上发现。
    let builtin_dir = args
        .iter()
        .position(|a| a == "--builtin-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(discover_builtin_dir);
    if let Some(dir) = &builtin_dir {
        let catalog = ling_kernel::builtin::discover(dir);
        kernel.set_builtin_catalog(catalog);
    }
    // 首次启动（plugins/ 为空）自动安装三件套；启动前既有安装一并调和（01 §6）。
    if let Err(e) = kernel.ensure_default_trio() {
        eprintln!("安装内置三件套失败：{e}");
        return 1;
    }
    if let Err(e) = kernel.reconcile() {
        eprintln!("启动调和失败：{e}");
        return 1;
    }
    let ui_port: Option<u64> = std::fs::read_to_string(data_root.join("registry.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<ling_sdk::Value>(&t).ok())
        .and_then(|v| v["ui_port"].as_u64());
    println!(
        "ling 已启动：管理端口 127.0.0.1:{}（界面 {}，数据根 {}）",
        server.port,
        ui_port
            .map(|p| format!("127.0.0.1:{p}"))
            .unwrap_or_else(|| "未安装".into()),
        data_root.display()
    );
    let flag = server.shutdown_flag();
    while !flag.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("ling 已停止。");
    0
}

/// 从当前目录向上查找 `builtin-plugins/`（须含至少一个可解析条目）。
fn discover_builtin_dir() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("builtin-plugins");
        if candidate.is_dir() && !ling_kernel::builtin::discover(&candidate).is_empty() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn cmd_request(request: ling_sdk::Value) -> i32 {
    let data_root = ling_data_root();
    let registry: ling_sdk::Value = std::fs::read_to_string(data_root.join("registry.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(ling_sdk::Value::Null);
    let Some(port) = registry["management_port"].as_u64() else {
        eprintln!("读不到管理端口（registry.json），宿主是否在运行？（ling serve）");
        return 1;
    };
    let session: ling_sdk::Value = std::fs::read_to_string(data_root.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(ling_sdk::Value::Null);
    let Some(token) = session["token"].as_str() else {
        eprintln!("读不到会话口令（session.json），宿主是否在运行？");
        return 1;
    };

    let stream = match TcpStream::connect(("127.0.0.1", port as u16)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("连接 127.0.0.1:{port} 失败：{e}");
            return 1;
        }
    };
    let _ = stream.set_nodelay(true);
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("复制连接失败：{e}");
            return 1;
        }
    };
    if writeln!(writer, "{}", json!({"op": "auth", "token": token})).is_err() {
        eprintln!("发送认证失败");
        return 1;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("宿主未应答认证");
        return 1;
    }
    match serde_json::from_str::<ling_sdk::Value>(&line) {
        Ok(v) if v["ok"] == false => {
            eprintln!("认证被拒绝：{}", v["error"]["message"]);
            return 1;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("认证应答无法解析：{e}");
            return 1;
        }
    }

    let request = json!({"id": 1, "method": request["method"], "params": request["params"]});
    if writeln!(writer, "{request}").is_err() {
        eprintln!("发送请求失败");
        return 1;
    }
    line.clear();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("宿主未应答");
        return 1;
    }
    match serde_json::from_str::<ling_sdk::Value>(&line) {
        Ok(v) => {
            if v["ok"] == true {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v["result"]).unwrap_or_default()
                );
                0
            } else {
                eprintln!(
                    "{}: {}",
                    v["error"]["code"].as_str().unwrap_or("E_INTERNAL"),
                    v["error"]["message"].as_str().unwrap_or("未知错误")
                );
                1
            }
        }
        Err(e) => {
            eprintln!("应答无法解析：{e}");
            1
        }
    }
}
