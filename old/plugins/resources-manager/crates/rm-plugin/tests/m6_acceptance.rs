//! M6 验收：平台适配 + 发行包资产存在性 + 打包脚本可执行。
//!
//! 运行时打包冒烟（release 二进制 scan → 重复组）走 `scripts/package.sh` 后
//! 手工/CI 执行，见 docs/03 §7.4 与 archive/M6 归档记录。

use std::path::Path;

/// rm-fs 遍历 + file_meta 在当前平台可用（inode 有值 / rel_path 规范化）。
#[test]
fn m6_fs_platform_meta_works() {
    let dir = std::env::temp_dir().join(format!("m6-fs-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/a.txt"), b"m6").unwrap();
    let mut seen = Vec::new();
    rm_fs::walk::walk(&dir, &[], &mut |fm| {
        seen.push(fm.clone());
        true
    });
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].rel_path, "sub/a.txt");
    assert_eq!(seen[0].size, 2);
    // unix 上 inode 必有；windows 上 file_index 必有；其余平台可为 None。
    #[cfg(any(unix, windows))]
    assert!(seen[0].inode.is_some());
}

/// Windows 长路径规范化：加前缀 / 还原 / UNC（仅在 windows 目标编译运行；
/// 其他平台只验证模块存在性不调用）。
#[cfg(windows)]
#[test]
fn m6_windows_long_path_roundtrip() {
    use rm_fs::platform::windows::{from_long_path, to_long_path};
    let short = Path::new(r"C:\data\a.txt");
    assert_eq!(to_long_path(short), short); // 短路径不加前缀
    let long = Path::new(&format!(r"C:\{}", "x".repeat(300)));
    let prefixed = to_long_path(long);
    assert!(prefixed.to_string_lossy().starts_with(r"\\?\"));
    assert_eq!(from_long_path(&prefixed), long);
}

/// 发行包资产齐全（源码树内；package.sh 会拷入 dist/release）。
#[test]
fn m6_packaging_assets_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let assets = [
        "scripts/package.sh",
        "packaging/systemd/resources-manager.service",
        "packaging/windows/install-service.ps1",
        "packaging/windows/uninstall-service.ps1",
        "packaging/INSTALL.txt",
    ];
    for a in assets {
        let p = root.join(a);
        assert!(p.is_file(), "缺少打包资产: {a}");
    }
    // package.sh 可执行（unix 权限位）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("scripts/package.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "package.sh 需要可执行位");
    }
}

/// systemd 单元最小字段齐全（防打包时拷错空文件）。
#[test]
fn m6_systemd_unit_has_required_fields() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("packaging/systemd/resources-manager.service")).unwrap();
    for key in ["[Unit]", "[Service]", "[Install]", "ExecStart=", "Restart=on-failure", "WantedBy="] {
        assert!(text.contains(key), "systemd 单元缺少字段: {key}");
    }
}
