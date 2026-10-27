//! `player-lite` 精简接收器（P3；`docs/01` §5.4 两套构建同一代码库、§5.13 投屏协议）。
//!
//! ```sh
//! player-lite [--port 7915] [--name 客厅盒子] [--id lite-xxxx]
//! # --port 0 → OS 分配端口，stdout 回显 `PORT=<n>`（e2e 用）
//! ```

use player::lite::{run, LiteOpts};

fn main() {
    let mut opts = LiteOpts::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut next = |name: &str| args.next().unwrap_or_else(|| panic!("--{name} 缺值"));
        match arg.as_str() {
            "--port" => {
                opts.http_port = next("port").parse().expect("--port 须为 0..65535")
            }
            "--name" => opts.name = next("name"),
            "--id" => opts.id = next("id"),
            "--hb" => opts.hb_targets.push(next("hb")),
            "--help" | "-h" => {
                println!(
                    "player-lite [--port 7915] [--name <名称>] [--id <id>] [--hb host:port]"
                );
                return;
            }
            other => {
                eprintln!("未知参数：{other}");
                std::process::exit(2);
            }
        }
    }
    if let Err(e) = run(opts) {
        eprintln!("player-lite 启动失败：{e}");
        std::process::exit(1);
    }
}
