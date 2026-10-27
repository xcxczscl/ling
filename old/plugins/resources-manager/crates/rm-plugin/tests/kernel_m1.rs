//! M1 插件装载集成：Kernel 安装/激活 resources-manager，调用核心方法（A1/A2 后端）。

use std::fs;
use std::path::{Path, PathBuf};

use ling_kernel::Kernel;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn build_plugin_dir() -> PathBuf {
    let root = workspace_root();
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "resources-manager"])
        .current_dir(&root)
        .status()
        .expect("build resources-manager");
    assert!(status.success());
    let lib = format!(
        "{}resources_manager{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let src = root.join("target/debug").join(&lib);
    assert!(src.is_file(), "cdylib 未找到：{}", src.display());
    let dir = root.join("target/debug/rmgr-plugin-install");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::copy(
        root.join("plugins/resources-manager/plugin.toml"),
        dir.join("plugin.toml"),
    )
    .unwrap();
    fs::copy(
        &src,
        dir.join(format!("resources-manager{}", std::env::consts::DLL_SUFFIX)),
    )
    .unwrap();
    dir
}

#[test]
fn kernel_activates_resources_manager_and_calls_methods() {
    let tmp = tempfile::tempdir().unwrap();
    let kernel = Kernel::new(tmp.path().join("ling")).unwrap();
    let plugin_dir = build_plugin_dir();
    kernel.install(&plugin_dir).unwrap();
    // 依赖齐备时 install 自动激活；否则显式 load。
    if !kernel
        .activated_plugins()
        .iter()
        .any(|p| p == "resources-manager")
    {
        kernel.load("resources-manager").unwrap();
    }

    // A1：方法已注册且可调。
    let stats = kernel
        .call(
            "resources-manager",
            "resources-manager.scan.stats",
            serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(stats["files"], 0);

    // 扫描一个临时目录。
    let files = tmp.path().join("files");
    fs::create_dir_all(&files).unwrap();
    fs::write(files.join("a.bin"), b"hello-kernel").unwrap();
    fs::write(files.join("b.bin"), b"hello-kernel").unwrap();
    let loc = kernel
        .call(
            "resources-manager",
            "resources-manager.location.add",
            serde_json::json!({"path": files.display().to_string(), "kind": "local"}),
        )
        .unwrap();
    let loc_id = loc["id"].as_str().unwrap().to_string();
    let scan = kernel
        .call(
            "resources-manager",
            "resources-manager.scan.start",
            serde_json::json!({"location_id": loc_id}),
        )
        .unwrap();
    assert!(scan["job_id"].as_str().is_some());
    // 扫描是后台线程；轮询 stats 直到 files>=2 或超时。
    let mut files_n = 0u64;
    for _ in 0..80 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let st = kernel
            .call(
                "resources-manager",
                "resources-manager.scan.stats",
                serde_json::json!({}),
            )
            .unwrap();
        files_n = st["files"].as_u64().unwrap_or(0);
        if files_n >= 2 {
            break;
        }
    }
    assert!(files_n >= 2, "扫描未入库：files={files_n}");
    let dups = kernel
        .call(
            "resources-manager",
            "resources-manager.dup.list",
            serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(dups["total"], 1);

    // UI 静态页方法。
    let page = kernel
        .call(
            "resources-manager",
            "resources-manager.ui.page",
            serde_json::json!({"path": "/resources/browse"}),
        )
        .unwrap();
    assert!(page["html"].as_str().unwrap().contains("资源浏览"));

    kernel.disable("resources-manager").unwrap();
}
