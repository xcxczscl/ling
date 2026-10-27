use std::fs;
use std::path::{Path, PathBuf};

use ling_kernel::Kernel;

fn cdylib_path(id: &str) -> PathBuf {
    ensure_cdylibs();
    let exe = std::env::current_exe().expect("test binary path");
    let profile = exe
        .parent()
        .and_then(|d| d.parent())
        .expect("target/<profile>/deps layout");
    let lib = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        id.replace('-', "_"),
        std::env::consts::DLL_SUFFIX
    );
    let path = profile.join(lib);
    assert!(
        path.is_file(),
        "示例插件动态库未构建：{}（尝试自动构建失败）",
        path.display()
    );
    path
}

pub(crate) fn ensure_cdylibs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let status = std::process::Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "demo-a",
                "-p",
                "demo-b",
                "-p",
                "core-ui",
                "-p",
                "core-settings",
                "-p",
                "core-plugin-manager",
                "-p",
                "core-job",
                "-p",
                "core-log",
            ])
            .current_dir(&workspace)
            .status()
            .expect("运行 cargo build 构建示例与内置插件");
        assert!(status.success(), "构建示例/内置插件失败");
    });
}

fn plugin_source(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(id)
}

pub(crate) struct Fixture {
    pub dir: PathBuf,
    _tmp: tempfile::TempDir,
}

pub(crate) fn fixture(id: &str) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::copy(
        plugin_source(id).join("plugin.toml"),
        dir.join("plugin.toml"),
    )
    .unwrap();
    fs::copy(
        cdylib_path(id),
        dir.join(format!("{id}{}", std::env::consts::DLL_SUFFIX)),
    )
    .unwrap();
    Fixture { dir, _tmp: tmp }
}

/// 同 id 夹具，但把清单里的版本号改写为指定值（依赖版本匹配测试用）。
#[allow(dead_code)]
pub(crate) fn fixture_with_version(id: &str, version: &str) -> Fixture {
    let fix = fixture(id);
    let text = fs::read_to_string(fix.dir.join("plugin.toml")).unwrap();
    let patched = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("version =") {
                format!("version = \"{version}\"")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(fix.dir.join("plugin.toml"), patched).unwrap();
    fix
}

/// 仅清单的夹具目录（无动态库；用于"永远不激活"的依赖图场景）。
#[allow(dead_code)]
pub(crate) fn manifest_only_fixture(id: &str, requires: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join(id);
    fs::create_dir_all(&dir).unwrap();
    let requires_toml = if requires.is_empty() {
        String::new()
    } else {
        let list = requires
            .iter()
            .map(|r| format!("\"{r}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("\n[requires]\nplugins = [{list}]\n")
    };
    let manifest =
        format!("[plugin]\nid = \"{id}\"\nversion = \"0.1.0\"\napi = \"1\"\n{requires_toml}");
    fs::write(dir.join("plugin.toml"), manifest).unwrap();
    tmp
}

#[allow(dead_code)]
pub(crate) fn fresh_kernel() -> (tempfile::TempDir, Kernel) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let kernel = Kernel::new(tmp.path().join("ling")).expect("kernel");
    (tmp, kernel)
}
