use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use ling_sdk::{codes, Plugin, Value};

use crate::error::KernelError;
use crate::manifest::{self, Manifest};
use crate::state::{PluginRecord, PluginState, Shared};

/// 装载通道（`docs/01` §3.1）：安装 = 拷贝进 `ling/plugins/<编号>/` 并校验清单；
/// 装载 = dlopen + 取符号 + 核对 id + activate。
pub fn install(shared: &Arc<Shared>, source: &Path) -> Result<String, KernelError> {
    let manifest = if source.is_dir() {
        install_from_dir(shared, source)?
    } else if source.is_file() {
        install_from_zip(shared, source)?
    } else {
        return Err(KernelError::NotFound(source.display().to_string()));
    };
    Ok(manifest.plugin.id)
}

fn ensure_not_installed(shared: &Arc<Shared>, id: &str) -> Result<(), KernelError> {
    let manifest = shared.plugin_dir(id).join("plugin.toml");
    if manifest.is_file() {
        return Err(KernelError::AlreadyInstalled(id.to_string()));
    }
    Ok(())
}

fn install_from_dir(shared: &Arc<Shared>, source: &Path) -> Result<Manifest, KernelError> {
    let manifest = manifest::read_manifest(source)?;
    ensure_not_installed(shared, &manifest.plugin.id)?;
    let target = shared.plugin_dir(&manifest.plugin.id);
    fs::create_dir_all(&target)
        .map_err(|e| KernelError::io("create plugins dir", e.to_string()))?;
    if let Err(e) = copy_tree(source, &target) {
        let _ = fs::remove_dir_all(&target);
        return Err(KernelError::io("copy plugin dir", e.to_string()));
    }
    Ok(manifest)
}

fn install_from_zip(shared: &Arc<Shared>, zip_path: &Path) -> Result<Manifest, KernelError> {
    let file = fs::File::open(zip_path).map_err(|e| KernelError::io("open zip", e.to_string()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| KernelError::io("read zip", e.to_string()))?;

    let mut manifest_text = None;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| KernelError::io("read zip entry", e.to_string()))?;
        if entry.enclosed_name().is_none() {
            return Err(KernelError::manifest(
                codes::E_MANIFEST_INVALID,
                "zip 含非法路径条目",
            ));
        }
        let entry_name = entry
            .name()
            .map_err(|e| KernelError::io("read zip entry", e.to_string()))?
            .into_owned();
        if entry_name == "plugin.toml" {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|e| KernelError::io("read plugin.toml", e.to_string()))?;
            manifest_text = Some(text);
        }
    }
    let manifest_text = manifest_text.ok_or_else(|| {
        KernelError::manifest(codes::E_MANIFEST_INVALID, "zip 根部缺少 plugin.toml")
    })?;
    drop(manifest_text);
    let staging_parent = shared.plugins_dir().join(".staging");
    fs::create_dir_all(&staging_parent)
        .map_err(|e| KernelError::io("create staging", e.to_string()))?;
    let staging = staging_parent.join(format!("zip-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| KernelError::io("create staging", e.to_string()))?;

    let extract = (|| -> Result<Manifest, KernelError> {
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| KernelError::io("read zip entry", e.to_string()))?;
            let rel = entry.enclosed_name().ok_or_else(|| {
                KernelError::manifest(codes::E_MANIFEST_INVALID, "zip 含非法路径条目")
            })?;
            if rel.as_os_str().is_empty() {
                continue;
            }
            let out = staging.join(&rel);
            if entry.is_dir() {
                fs::create_dir_all(&out)
                    .map_err(|e| KernelError::io("extract dir", e.to_string()))?;
            } else {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| KernelError::io("extract dir", e.to_string()))?;
                }
                let mut file = fs::File::create(&out)
                    .map_err(|e| KernelError::io("extract file", e.to_string()))?;
                std::io::copy(&mut entry, &mut file)
                    .map_err(|e| KernelError::io("extract file", e.to_string()))?;
            }
        }
        let manifest = manifest::read_manifest(&staging)?;
        ensure_not_installed(shared, &manifest.plugin.id)?;
        let target = shared.plugin_dir(&manifest.plugin.id);
        if target.is_dir() {
            copy_tree(&staging, &target)
                .map_err(|e| KernelError::io("merge plugin dir", e.to_string()))?;
            fs::remove_dir_all(&staging)
                .map_err(|e| KernelError::io("remove staging", e.to_string()))?;
        } else {
            fs::rename(&staging, &target)
                .map_err(|e| KernelError::io("move plugin into place", e.to_string()))?;
        }
        Ok(manifest)
    })();

    if extract.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    let _ = fs::remove_dir_all(&staging_parent);
    extract
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

pub fn load(
    shared: &Arc<Shared>,
    core: &Arc<crate::context::KernelCore>,
    id: &str,
) -> Result<(), KernelError> {
    let dir = shared.plugin_dir(id);
    if !dir.is_dir() {
        return Err(KernelError::NotFound(id.to_string()));
    }
    if !dir.join("plugin.toml").is_file() {
        return Err(KernelError::NotFound(id.to_string()));
    }
    {
        let plugins = shared.plugins.read().unwrap();
        if plugins.contains_key(id) {
            return Err(KernelError::AlreadyLoaded(id.to_string()));
        }
    }

    let manifest = manifest::read_manifest(&dir)?;
    if manifest.plugin.id != id {
        return Err(KernelError::manifest(
            codes::E_MANIFEST_INVALID,
            format!("目录名 {id} 与清单 id {} 不一致", manifest.plugin.id),
        ));
    }
    let manifest_text = fs::read_to_string(dir.join("plugin.toml"))
        .map_err(|e| KernelError::io("read plugin.toml", e.to_string()))?;
    if manifest.is_sidecar() {
        return crate::sidecar::start(shared, core, id, &dir, manifest, manifest_text);
    }

    let declared: BTreeSet<_> = manifest.declared_capabilities()?.into_iter().collect();
    let data_dir = dir.join("data");
    fs::create_dir_all(&data_dir).map_err(|e| KernelError::io("create data dir", e.to_string()))?;

    // `docs/05` §3.1 对策②：装载的是 `{id}.so` 的**私有副本**——磁盘原件保留在
    // `{id}.so` 路径（调和/启用可随时重载），用户替换新产物也不会覆盖仍在内存
    // 映射中的文件（避免 SIGBUS）。
    let source = dir.join(format!("{id}{}", std::env::consts::DLL_SUFFIX));
    if !source.is_file() {
        return Err(KernelError::load(format!(
            "动态库不存在：{}",
            source.display()
        )));
    }
    let private_name = format!(
        "{id}.{}{}",
        shared
            .lib_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        std::env::consts::DLL_SUFFIX
    );
    let private_path = dir.join(&private_name);
    fs::copy(&source, &private_path)
        .map_err(|e| KernelError::io("copy library to private name", e.to_string()))?;
    let library = match open_library(&private_path) {
        Ok(lib) => lib,
        Err(e) => {
            let _ = fs::remove_file(&private_path);
            return Err(e);
        }
    };
    let mut instance = create_instance(&library, id)?;

    crate::caps::set_declared(shared, id, declared.clone());
    shared.plugins.write().unwrap().insert(
        id.to_string(),
        PluginRecord {
            manifest: manifest.clone(),
            manifest_text,
            state: PluginState::Activating,
            library: Some(library),
            library_file: Some(private_name),
            instance: None,
            ledger: Vec::new(),
            next_seq: 1,
            deactivate_cbs: Vec::new(),
        },
    );

    let ctx = core.context_for(id, declared);
    let activate_result = instance.as_mut().activate(ctx);

    match activate_result {
        Ok(()) => {
            let mut plugins = shared.plugins.write().unwrap();
            let record = plugins.get_mut(id).expect("record exists after activate");
            record.state = PluginState::Activated;
            record.instance = Some(instance);
            drop(plugins);
            state_event(shared, id, "activated");
            Ok(())
        }
        Err(e) => {
            let (ledger, library_file) = {
                let mut plugins = shared.plugins.write().unwrap();
                let record = plugins.get_mut(id).expect("record exists after activate");
                (
                    crate::ledger::take_ledger(record),
                    record.library_file.take(),
                )
            };
            crate::ledger::run_reverse(ledger);
            drop(instance);
            let mut plugins = shared.plugins.write().unwrap();
            if let Some(record) = plugins.get_mut(id) {
                record.library = None;
            }
            plugins.remove(id);
            drop(plugins);
            if let Some(name) = library_file {
                let _ = fs::remove_file(dir.join(name));
            }
            crate::caps::remove(shared, id);
            Err(KernelError::load(format!("activate 失败：{}", e.message())))
        }
    }
}

fn open_library(path: &Path) -> Result<libloading::Library, KernelError> {
    if !path.is_file() {
        return Err(KernelError::load(format!(
            "动态库不存在：{}",
            path.display()
        )));
    }
    unsafe { libloading::Library::new(path) }
        .map_err(|e| KernelError::load(format!("动态库加载失败（{}）：{e}", path.display())))
}

#[allow(improper_ctypes_definitions)]
type CreateFn = unsafe extern "C" fn() -> *mut dyn Plugin;

fn create_instance(
    library: &libloading::Library,
    id: &str,
) -> Result<Box<dyn Plugin>, KernelError> {
    let symbol: libloading::Symbol<CreateFn> = unsafe { library.get(b"ling_plugin_create") }
        .map_err(|_| KernelError::load("不是合法的 ling 插件：缺少 ling_plugin_create 符号"))?;
    let create: CreateFn = *symbol;
    let raw = unsafe { create() };
    if raw.is_null() {
        return Err(KernelError::load("ling_plugin_create 返回空指针"));
    }
    let instance = unsafe { Box::from_raw(raw) };
    if instance.id() != id {
        let actual = instance.id().to_string();
        drop(instance);
        return Err(KernelError::load(format!(
            "动态库 id {actual} 与清单 id {id} 不一致"
        )));
    }
    Ok(instance)
}

pub fn state_event(shared: &Arc<Shared>, id: &str, state: &str) {
    shared.dispatch(
        "kernel.plugin.state",
        Value::Object(
            [
                ("plugin".to_string(), Value::String(id.to_string())),
                ("state".to_string(), Value::String(state.to_string())),
                ("reason".to_string(), Value::String(String::new())),
            ]
            .into_iter()
            .collect(),
        ),
    );
}

pub fn installed_ids(shared: &Shared) -> Vec<String> {
    let mut ids = Vec::new();
    if let Ok(entries) = fs::read_dir(shared.plugins_dir()) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with('.') {
                        continue;
                    }
                    if entry.path().join("plugin.toml").is_file() {
                        ids.push(name.to_string());
                    }
                }
            }
        }
    }
    ids.sort();
    ids
}

/// 卸载的文件语义（`docs/01` §3.4 步骤 4–5、`docs/04` §1）：
/// 非 purge 移除安装工件（plugin.toml、动态库、侧车等），保留 `data/` 用户数据；
/// purge 连 `data/` 一并删除。目录在两种模式下都可再次安装。
pub fn remove_install_files(shared: &Shared, id: &str, purge: bool) -> Result<(), KernelError> {
    let dir = shared.plugin_dir(id);
    if purge {
        if !dir.exists() {
            return Ok(());
        }
        return fs::remove_dir_all(&dir)
            .map_err(|e| KernelError::io("purge plugin dir", e.to_string()));
    }
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in
        fs::read_dir(&dir).map_err(|e| KernelError::io("read plugin dir", e.to_string()))?
    {
        let entry = entry.map_err(|e| KernelError::io("read plugin dir", e.to_string()))?;
        let name = entry.file_name();
        if name == "data" && entry.path().is_dir() {
            continue;
        }
        let path = entry.path();
        let removed = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.map_err(|e| KernelError::io("remove install artifact", e.to_string()))?;
    }
    Ok(())
}

/// 热更新备份（`docs/05` §2）：备份"当前运行版本"的工件——装载中插件用
/// 记录里的清单原文与私有库文件（磁盘上的同名文件可能已被新版覆盖）；
/// 未装载插件退化为整目录（非 `data/`）快照。
pub struct ReloadBackup {
    pub dir: std::path::PathBuf,
}

pub fn backup_install_artifacts(
    shared: &Shared,
    id: &str,
    running_manifest_text: Option<&str>,
    running_library_file: Option<&str>,
) -> Result<ReloadBackup, KernelError> {
    let dir = shared.plugin_dir(id);
    static BACKUP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let backup = std::env::temp_dir().join(format!(
        "ling-reload-{}-{}-{id}",
        std::process::id(),
        BACKUP_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&backup);
    fs::create_dir_all(&backup)
        .map_err(|e| KernelError::io("create reload backup", e.to_string()))?;
    let copy_one = |src_name: &str, dst_name: &str| -> Result<(), KernelError> {
        let src = dir.join(src_name);
        if src.is_file() {
            fs::copy(&src, backup.join(dst_name))
                .map(|_| ())
                .map_err(|e| KernelError::io("copy reload backup", e.to_string()))?;
        }
        Ok(())
    };
    match running_manifest_text {
        Some(text) => fs::write(backup.join("plugin.toml"), text)
            .map_err(|e| KernelError::io("backup manifest", e.to_string()))?,
        None => copy_one("plugin.toml", "plugin.toml")?,
    }
    let lib_name = format!("{id}{}", std::env::consts::DLL_SUFFIX);
    match running_library_file {
        Some(private) => copy_one(private, &lib_name)?,
        None => copy_one(&lib_name, &lib_name)?,
    }
    for entry in
        fs::read_dir(&dir).map_err(|e| KernelError::io("read plugin dir", e.to_string()))?
    {
        let entry = entry.map_err(|e| KernelError::io("read plugin dir", e.to_string()))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if (name_str == "data" && entry.path().is_dir())
            || name_str == "plugin.toml"
            || name_str == lib_name
            || Some(name_str.as_str()) == running_library_file
        {
            continue;
        }
        let src = entry.path();
        let dst = backup.join(&name);
        let copied = if src.is_dir() {
            copy_tree(&src, &dst)
        } else {
            fs::copy(&src, &dst).map(|_| ())
        };
        copied.map_err(|e| KernelError::io("copy reload backup", e.to_string()))?;
    }
    Ok(ReloadBackup { dir: backup })
}

/// 热更新回滚（`docs/05` §2 步骤 5）：清掉当前安装工件（保留 `data/`），
/// 从备份还原旧版工件（`plugin.toml` + `{id}.so`）。
pub fn restore_install_artifacts(
    shared: &Shared,
    id: &str,
    backup: &ReloadBackup,
) -> Result<(), KernelError> {
    remove_install_files(shared, id, false)?;
    let dir = shared.plugin_dir(id);
    fs::create_dir_all(&dir).map_err(|e| KernelError::io("recreate plugin dir", e.to_string()))?;
    copy_tree(&backup.dir, &dir)
        .map_err(|e| KernelError::io("restore reload backup", e.to_string()))
}

pub fn discard_backup(backup: &ReloadBackup) {
    let _ = fs::remove_dir_all(&backup.dir);
}
