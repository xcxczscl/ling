//! 自动发现 libmpv SDK 路径，把搜索路径传给 Cargo 让 libmpv2-sys 能找到 mpv.lib。
//!
//! 搜索顺序（找到即停）：
//!   1. 环境变量 `MPV_SDK_DIR`
//!   2. 工作区根目录下的 `run/mpv-sdk/`（本文件向上两级定位 workspace）
//!
//! 期望目录结构：
//!   run/mpv-sdk/lib/mpv.lib     ← MSVC 链接库
//!   run/mpv-sdk/bin/mpv-2.dll   ← 运行时 DLL（放到 dist 或 PATH）
//!   run/mpv-sdk/include/mpv/    ← 头文件（libmpv2-sys bindgen 用）

use std::path::PathBuf;

fn main() {
    if let Some(dir) = find_sdk() {
        // shinchiro dev 包把文件直接放根目录（mpv.lib、mpv-2.dll 都在根）
        // 有些包放 lib/ 子目录——两个都搜
        println!("cargo:rustc-link-search=native={}", dir.display());
        let lib_dir = dir.join("lib");
        if lib_dir.exists() {
            println!("cargo:rustc-link-search=native={}", lib_dir.display());
        }
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:warning=mpv-sdk found at {}", dir.display());
        return;
    }
    println!("cargo:warning=mpv-sdk NOT found. Set MPV_SDK_DIR env var or extract dev 7z to run/mpv-sdk/");
}

fn find_sdk() -> Option<PathBuf> {
    // 1. 环境变量（用户可能有多个版本，按需切换）
    if let Ok(v) = std::env::var("MPV_SDK_DIR") {
        let p = PathBuf::from(&v);
        if p.exists() {
            return Some(p);
        }
    }
    // 2. 工作区根目录下的 run/mpv-sdk/
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // player/plugins/player → plugins/player → player → 工作区根
    let workspace_root = here
        .parent()                        // plugins/player
        .and_then(|p| p.parent())        // player
        .and_then(|p| p.parent())        // workspace root
        .unwrap_or_else(|| here.as_path())
        .to_path_buf();
    let candidate = workspace_root.join("run").join("mpv-sdk");
    if candidate.exists() {
        return Some(candidate);
    }
    None
}
