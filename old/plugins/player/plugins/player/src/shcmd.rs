//! 外部命令执行（带超时）——`docs/01` §6.1 附注：P1 起以子进程调用
//! poppler（pdftoppm/pdfinfo）与 FFmpeg（ffprobe，P4 ffmpeg），插件在
//! plugin.toml 声明 `shell` 能力（附录 B 登记，P1）。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 运行 `args[0] args[1..]`，捕获输出；超时杀进程，返回 `Err`。
///
/// stdin 不接（避免子进程等输入挂死）；stdout/stderr 管道读取。
pub fn run(args: &[&str], timeout: Duration) -> Result<std::process::Output, String> {
    let mut child = Command::new(args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", args[0]))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{} 超时 {:?}", args[0], timeout));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait {}: {e}", args[0]));
            }
        }
    }

    child
        .wait_with_output()
        .map_err(|e| format!("wait_with_output {}: {e}", args[0]))
}

/// 成功判据：exit 0；否则附带 stderr 尾部做错误消息。
pub fn run_checked(args: &[&str], timeout: Duration) -> Result<String, String> {
    let out = run(args, timeout)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{} 退出 {:?}：{}",
            args[0],
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_ok_and_fail_output() {
        let ok = run_checked(&["sh", "-c", "printf hi"], Duration::from_secs(5)).unwrap();
        assert_eq!(ok, "hi");
        let err = run_checked(&["sh", "-c", "echo boom >&2; exit 3"], Duration::from_secs(5))
            .unwrap_err();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn t2_timeout_kills() {
        let err = run(&["sleep", "30"], Duration::from_millis(100)).unwrap_err();
        assert!(err.contains("超时"), "{err}");
    }
}
