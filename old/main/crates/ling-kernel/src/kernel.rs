use std::path::{Path, PathBuf};
use std::sync::Arc;

use ling_sdk::{CallError, Capability, Value};

use crate::context::KernelCore;
use crate::deps::ReconcileReport;
use crate::error::KernelError;
use crate::manifest;
use crate::state::{PluginState, Shared, Violation};
use crate::{broker, bus, caps, deps, ledger, loader, registry};

/// ling 内核（宿主嵌入入口）。四个服务 + 机制见 `docs/01` §3.2。
pub struct Kernel {
    shared: Arc<Shared>,
    core: Arc<KernelCore>,
    last_reconcile: std::sync::Mutex<Option<ReconcileReport>>,
    /// 发行包内置插件目录（`docs/06` §2；由宿主入口扫描后注入）。
    builtin_catalog: std::sync::RwLock<Vec<crate::builtin::BuiltinEntry>>,
}

impl Kernel {
    /// 创建内核并准备数据根目录（`ling/` 级路径约定见 docs-common/01 §2.3）。
    /// 宿主级登记（`ling/registry.json`：停用名单、能力撤销黑名单）随内核载入。
    pub fn new(data_root: impl Into<PathBuf>) -> Result<Self, KernelError> {
        let shared = Arc::new(Shared::new(data_root.into()));
        std::fs::create_dir_all(shared.plugins_dir())
            .map_err(|e| KernelError::io("create plugins dir", e.to_string()))?;
        let stored = registry::read(&shared.data_root);
        *shared.disabled.write().unwrap() = stored.disabled;
        for (plugin, caps) in stored.revoked {
            let set = caps
                .iter()
                .filter_map(|s| Capability::parse(s))
                .collect::<std::collections::BTreeSet<_>>();
            shared.revoked.write().unwrap().insert(plugin, set);
        }
        let core = Arc::new(KernelCore {
            shared: Arc::clone(&shared),
        });
        Ok(Self {
            shared,
            core,
            last_reconcile: std::sync::Mutex::new(None),
            builtin_catalog: std::sync::RwLock::new(Vec::new()),
        })
    }

    pub fn data_root(&self) -> &Path {
        &self.shared.data_root
    }

    /// 注入发行包内置插件目录（宿主入口扫描 `builtin-plugins/` 后调用，`docs/06` §2）。
    pub fn set_builtin_catalog(&self, entries: Vec<crate::builtin::BuiltinEntry>) {
        *self.builtin_catalog.write().unwrap() = entries;
    }

    /// "可安装"的内置插件（目录中存在、尚未安装的），供插件管理页展示（`docs/06` §3）。
    pub fn builtin_available(&self) -> Vec<ling_sdk::Value> {
        let catalog = self.builtin_catalog.read().unwrap();
        let installed: std::collections::BTreeSet<_> =
            loader::installed_ids(&self.shared).into_iter().collect();
        catalog
            .iter()
            .filter(|e| !installed.contains(&e.id))
            .map(crate::builtin::BuiltinEntry::to_json)
            .collect()
    }

    /// 按需安装一个内置插件（与普通插件同一通道：暂存 → `install` → 调和）。
    pub fn install_builtin(&self, id: &str) -> Result<String, KernelError> {
        let entry = {
            let catalog = self.builtin_catalog.read().unwrap();
            catalog
                .iter()
                .find(|e| e.id == id)
                .cloned()
                .ok_or_else(|| KernelError::NotFound(format!("内置插件不在目录中：{id}")))?
        };
        let staging = crate::builtin::stage(&self.shared, &entry)?;
        let installed = self.install(&staging);
        let _ = std::fs::remove_dir_all(&staging);
        installed
    }

    /// 首次启动（`plugins/` 为空）自动安装三件套（`docs/06` §3）。
    /// 已有安装时跳过。返回本次安装的插件编号（可能为空）。
    pub fn ensure_default_trio(&self) -> Result<Vec<String>, KernelError> {
        if !crate::builtin::is_first_start(&self.shared) {
            return Ok(Vec::new());
        }
        let mut installed = Vec::new();
        for id in crate::builtin::DEFAULT_TRIO {
            installed.push(self.install_builtin(id)?);
        }
        Ok(installed)
    }

    /// 安装：接受插件目录或 zip 包，校验清单后拷贝进 `ling/plugins/<编号>/`
    /// （`docs/04` §1）。随后配置调和：依赖齐备即自动激活。环 → 移除刚拷贝
    /// 的工件并报 `E_DEPENDENCY_CYCLE`；激活失败不否决安装（报告留存）。
    pub fn install(&self, source: &Path) -> Result<String, KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        let id = loader::install(&self.shared, source)?;
        if let Err(e) = deps::check_no_cycle(&self.shared) {
            let _ = loader::remove_install_files(&self.shared, &id, true);
            return Err(e);
        }
        let _ = self.reconcile_inner();
        Ok(id)
    }

    /// 装载单个已安装插件：dlopen → 核对 id → activate → 广播状态事件。
    /// 原语操作：不校验依赖（L3 起常规路径走安装/调和自动激活）。
    pub fn load(&self, id: &str) -> Result<(), KernelError> {
        loader::load(&self.shared, &self.core, id)
    }

    /// 装载全部已安装但未装载的插件（原语；顺序不受依赖约束）。
    pub fn load_all(&self) -> Result<Vec<String>, KernelError> {
        let mut loaded = Vec::new();
        for id in loader::installed_ids(&self.shared) {
            let is_loaded = {
                let plugins = self.shared.plugins.read().unwrap();
                plugins.contains_key(&id)
            };
            if !is_loaded {
                self.load(&id)?;
                loaded.push(id);
            }
        }
        Ok(loaded)
    }

    /// 跨插件调用（服务①路由 + 服务③门禁）。
    pub fn call(&self, plugin: &str, method: &str, args: Value) -> Result<Value, CallError> {
        broker::call(&self.shared, plugin, method, args)
    }

    /// 内核侧广播（`kernel.*` 主题；插件广播走 `ctx.emit`）。
    pub fn emit(&self, topic: &str, payload: Value) {
        self.shared.dispatch(topic, payload);
    }

    /// 宿主侧订阅（如 `kernel.plan.step` 观测）。不计入任何插件账本。
    pub fn subscribe(&self, pattern: &str, callback: ling_sdk::EventCallback) -> u64 {
        bus::kernel_subscribe(&self.shared, pattern, bus::wrap_plugin_callback(callback))
    }

    pub fn unsubscribe(&self, subscription: u64) {
        bus::kernel_unsubscribe(&self.shared, subscription);
    }

    /// 用户撤销黑名单：移除某插件的生效能力（调用时门禁据此拒绝）。
    /// 黑名单宿主级持久化（`ling/registry.json`）。
    pub fn revoke_capability(&self, plugin: &str, cap: Capability) -> Result<(), KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        caps::revoke(&self.shared, plugin, cap)
            .map_err(|m| KernelError::CapabilityState { message: m })
    }

    /// 恢复被撤销的能力。
    pub fn grant_capability(&self, plugin: &str, cap: Capability) -> Result<(), KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        caps::grant(&self.shared, plugin, cap)
            .map_err(|m| KernelError::CapabilityState { message: m })
    }

    /// 启用插件并调和（`docs/03` §2.2：停用名单移除 → 满足即自动激活）。
    pub fn enable(&self, id: &str) -> Result<(), KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        if !self.shared.plugin_dir(id).join("plugin.toml").is_file() {
            return Err(KernelError::NotFound(id.to_string()));
        }
        self.shared.disabled.write().unwrap().remove(id);
        registry::persist_shared(&self.shared)
            .map_err(|e| KernelError::io("write registry.json", e.to_string()))?;
        self.reconcile_inner()?;
        Ok(())
    }

    /// 停用插件并调和：自身及其激活中的传递依赖方一并停用，工件保留。
    pub fn disable(&self, id: &str) -> Result<(), KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        if !self.shared.plugin_dir(id).join("plugin.toml").is_file() {
            return Err(KernelError::NotFound(id.to_string()));
        }
        self.shared.disabled.write().unwrap().insert(id.to_string());
        registry::persist_shared(&self.shared)
            .map_err(|e| KernelError::io("write registry.json", e.to_string()))?;
        self.reconcile_inner()?;
        Ok(())
    }

    /// 热更新（`docs/05` §2）：校验新清单 → 备份工件 → 停用下游与自身 →
    /// 装载新版 → 恢复调和（重新激活下游）。新版装载失败 → 还原旧工件、
    /// 重新装载旧版、恢复下游，返回 `E_RELOAD_FAILED`（旧版继续服务）。
    pub fn reload(&self, id: &str) -> Result<ReloadReport, KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        let dir = self.shared.plugin_dir(id);
        if !dir.join("plugin.toml").is_file() {
            return Err(KernelError::NotFound(id.to_string()));
        }
        let new_manifest = manifest::read_manifest(&dir)?;
        if new_manifest.plugin.id != id {
            return Err(KernelError::manifest(
                ling_sdk::codes::E_MANIFEST_INVALID,
                format!("目录名 {id} 与清单 id {} 不一致", new_manifest.plugin.id),
            ));
        }

        let running = {
            let plugins = self.shared.plugins.read().unwrap();
            plugins
                .get(id)
                .map(|r| (r.manifest_text.clone(), r.library_file.clone()))
        };
        let backup = loader::backup_install_artifacts(
            &self.shared,
            id,
            running.as_ref().map(|(t, _)| t.as_str()),
            running.as_ref().and_then(|(_, f)| f.as_deref()),
        )?;
        let downstream = deps::active_dependents(&self.shared, id)?;
        for dependent in &downstream {
            if stop_plugin(&self.shared, dependent).is_ok() {
                loader::state_event(&self.shared, dependent, "deactivated");
            }
        }
        let was_loaded = self.shared.plugins.read().unwrap().contains_key(id);
        if was_loaded && stop_plugin(&self.shared, id).is_ok() {
            loader::state_event(&self.shared, id, "deactivated");
        }

        if let Err(new_err) = loader::load(&self.shared, &self.core, id) {
            let restored = loader::restore_install_artifacts(&self.shared, id, &backup);
            loader::discard_backup(&backup);
            let rollback = match restored {
                Ok(()) => loader::load(&self.shared, &self.core, id),
                Err(e) => Err(e),
            };
            return match rollback {
                Ok(()) => {
                    let _ = self.reconcile_inner();
                    Err(KernelError::reload(format!(
                        "{new_err}；已回滚旧版并恢复下游"
                    )))
                }
                Err(rb) => Err(KernelError::reload(format!("{new_err}；回滚失败：{rb}"))),
            };
        }
        loader::discard_backup(&backup);
        self.reconcile_inner()?;
        Ok(ReloadReport {
            plugin: id.to_string(),
            downstream,
            version: new_manifest.plugin.version,
        })
    }

    /// 重启插件（崩溃恢复路径）：若在运行则停用，再调和（满足条件即重新激活）。
    pub fn restart(&self, id: &str) -> Result<(), KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        if !self.shared.plugin_dir(id).join("plugin.toml").is_file() {
            return Err(KernelError::NotFound(id.to_string()));
        }
        if stop_plugin(&self.shared, id).is_ok() {
            loader::state_event(&self.shared, id, "deactivated");
        }
        self.reconcile_inner()?;
        Ok(())
    }

    /// 优雅停机（`docs/01` §4）：已激活插件按逆向拓扑序逐个停用。
    pub fn shutdown(&self) -> Result<Vec<String>, KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        let order = deps::shutdown_order(&self.shared)?;
        for id in &order {
            if stop_plugin(&self.shared, id).is_ok() {
                loader::state_event(&self.shared, id, "deactivated");
            }
        }
        Ok(order)
    }

    /// 触发一次配置调和（`docs/03` §4）。返回计划报告；环等图错误报 Err。
    pub fn reconcile(&self) -> Result<ReconcileReport, KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        self.reconcile_inner()
    }

    /// 最近一次调和报告（安装/启停/卸载触发的调和同样留存）。
    pub fn last_reconcile(&self) -> Option<ReconcileReport> {
        self.last_reconcile.lock().unwrap().clone()
    }

    fn reconcile_inner(&self) -> Result<ReconcileReport, KernelError> {
        let report = deps::reconcile(&self.shared, &self.core)?;
        *self.last_reconcile.lock().unwrap() = Some(report.clone());
        Ok(report)
    }

    /// 命名空间越权记录（方法名/事件主题未带前缀，供 `ling doctor` 检查）。
    pub fn namespace_violations(&self) -> Vec<Violation> {
        self.shared.violations.lock().unwrap().clone()
    }

    /// 已安装插件编号（按目录扫描，以 `plugin.toml` 存在为准；
    /// 非 purge 卸载后仅剩 `data/` 的目录不再计入）。
    pub fn installed_plugins(&self) -> Vec<String> {
        loader::installed_ids(&self.shared)
    }

    /// 已装载且已激活的插件。
    pub fn activated_plugins(&self) -> Vec<String> {
        let plugins = self.shared.plugins.read().unwrap();
        let mut ids: Vec<String> = plugins
            .iter()
            .filter(|(_, r)| r.state == PluginState::Activated)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// 已安装插件及其判定状态（`activated` / `pending` / `disabled`，
    /// 管理端口 `kernel.plugins.list` 用）。
    pub fn plugin_list(&self) -> Vec<PluginStatus> {
        let activated: std::collections::BTreeSet<String> =
            self.activated_plugins().into_iter().collect();
        let disabled = self.shared.disabled.read().unwrap().clone();
        self.installed_plugins()
            .into_iter()
            .map(|id| {
                let version = manifest::read_manifest(&self.shared.plugin_dir(&id))
                    .map(|m| m.plugin.version)
                    .unwrap_or_default();
                let state = if disabled.contains(&id) {
                    "disabled"
                } else if activated.contains(&id) {
                    "activated"
                } else {
                    "pending"
                };
                PluginStatus { id, version, state }
            })
            .collect()
    }

    /// 查看某插件的效应账本（调试用）。
    pub fn ledger_of(&self, plugin: &str) -> Vec<ling_sdk::LedgerEntryInfo> {
        let plugins = self.shared.plugins.read().unwrap();
        plugins.get(plugin).map(ledger::info).unwrap_or_default()
    }

    /// 查看已装载插件的清单。
    pub fn manifest_of(&self, plugin: &str) -> Option<manifest::Manifest> {
        let plugins = self.shared.plugins.read().unwrap();
        plugins.get(plugin).map(|r| r.manifest().clone())
    }

    /// 卸载（`docs/01` §3.4、`docs/03` §2.2）：先停用激活中的传递依赖方
    /// （工件保留）→ 停用目标（on_deactivate 回调 → 账本倒序撤销 → 核对残留
    /// → 移除装载记录与能力）→ 清理安装工件（非 purge 保留 `data/`）→
    /// 广播 `kernel.plugin.state: uninstalled` → 配置调和。撤销失败不停止，
    /// 结果进 [`UninstallReport`]。
    pub fn uninstall(&self, id: &str, purge: bool) -> Result<UninstallReport, KernelError> {
        let _gate = self.shared.config_gate.lock().unwrap();
        let had_record = self.shared.plugins.read().unwrap().contains_key(id);
        if !had_record && !self.shared.plugin_dir(id).exists() {
            return Err(KernelError::NotFound(id.to_string()));
        }

        for dependent in deps::active_dependents(&self.shared, id)? {
            if stop_plugin(&self.shared, &dependent).is_ok() {
                loader::state_event(&self.shared, &dependent, "deactivated");
            }
        }

        let outcome = match stop_plugin(&self.shared, id) {
            Ok(outcome) => Some(outcome),
            Err(KernelError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };

        loader::remove_install_files(&self.shared, id, purge)?;
        if outcome.is_some() {
            loader::state_event(&self.shared, id, "uninstalled");
        }
        let _ = self.reconcile_inner();

        let outcome = outcome.unwrap_or_default();
        Ok(UninstallReport {
            plugin: id.to_string(),
            effects_total: outcome.effects_total,
            undo_failures: outcome.undo_failures,
            remnant_methods: outcome.remnant_methods,
            remnant_subs: outcome.remnant_subs,
            purged: purge,
        })
    }
}

/// 内核析构即优雅停机（幂等：`shutdown` 对已停插件为空操作）。
/// 测试/宿主未显式 `shutdown` 时的资源兜底——否则插件监听线程与端口泄漏。
impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Default)]
pub(crate) struct StopOutcome {
    pub effects_total: usize,
    pub undo_failures: Vec<String>,
    pub remnant_methods: Vec<String>,
    pub remnant_subs: Vec<String>,
}

/// 停用一个已装载插件：on_deactivate 回调 → 账本倒序撤销 → 核对残留 →
/// 移除装载记录与能力。不广播事件（由调用方按场景播报）。
pub(crate) fn stop_plugin(shared: &Arc<Shared>, id: &str) -> Result<StopOutcome, KernelError> {
    // 侧车：先优雅停进程（deactivate→shutdown→强杀），再走账本撤销。
    if let Some(handle) = shared.sidecars.lock().unwrap().remove(id) {
        handle
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        crate::sidecar::graceful_stop(&handle);
    }
    let record = shared.plugins.write().unwrap().remove(id);
    let Some(mut record) = record else {
        return Err(KernelError::NotFound(id.to_string()));
    };
    let effects_total = record.ledger.len();
    let library_file = record.library_file.clone();
    let mut instance = record.instance.take();
    if let Some(inst) = instance.as_mut() {
        inst.deactivate();
    }
    for cb in record.take_deactivate_cbs() {
        cb();
    }
    let entries = ledger::take_ledger(&mut record);
    let undo_failures = ledger::run_reverse(entries);
    drop(instance);
    drop(record);
    if let Some(name) = library_file {
        let _ = std::fs::remove_file(shared.plugin_dir(id).join(name));
    }
    let (remnant_methods, remnant_subs) = remnant_scan(shared, id);
    caps::remove(shared, id);
    Ok(StopOutcome {
        effects_total,
        undo_failures,
        remnant_methods,
        remnant_subs,
    })
}

/// 卸载完整性核对（`docs/01` §5 交付物 4）：账本撤销后，方法表与订阅表中
/// 不应再有该插件的任何残留。
fn remnant_scan(shared: &Shared, id: &str) -> (Vec<String>, Vec<String>) {
    let prefix = format!("{id}.");
    let mut methods: Vec<String> = shared
        .methods
        .read()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .cloned()
        .collect();
    methods.sort();
    let mut subs: Vec<String> = shared
        .subs
        .read()
        .unwrap()
        .iter()
        .filter(|s| s.owner == id)
        .map(|s| s.pattern.clone())
        .collect();
    subs.sort();
    (methods, subs)
}

/// 热更新报告（`docs/05` §2）：新版装载成功后，记录被停用并重新激活的下游。
#[derive(Debug)]
pub struct ReloadReport {
    pub plugin: String,
    pub downstream: Vec<String>,
    pub version: String,
}

/// 插件状态条目（管理端口列表用）。
#[derive(Debug, Clone)]
pub struct PluginStatus {
    pub id: String,
    pub version: String,
    pub state: &'static str,
}

/// 卸载报告（`docs/01` §3.4 保证：单条撤销失败不停止，最后报告
/// "卸载不完整"并给出清单）。
#[derive(Debug)]
pub struct UninstallReport {
    pub plugin: String,
    pub effects_total: usize,
    pub undo_failures: Vec<String>,
    pub remnant_methods: Vec<String>,
    pub remnant_subs: Vec<String>,
    pub purged: bool,
}

impl UninstallReport {
    pub fn complete(&self) -> bool {
        self.undo_failures.is_empty()
            && self.remnant_methods.is_empty()
            && self.remnant_subs.is_empty()
    }

    /// "卸载完整/不完整 + 清单"的可读汇总。
    pub fn summary(&self) -> String {
        if self.complete() {
            format!(
                "卸载完整：{}（撤销效应 {} 条{}）",
                self.plugin,
                self.effects_total,
                if self.purged {
                    "，已清除数据"
                } else {
                    ""
                }
            )
        } else {
            let mut items = self.undo_failures.clone();
            items.extend(self.remnant_methods.iter().map(|m| format!("残留方法 {m}")));
            items.extend(self.remnant_subs.iter().map(|p| format!("残留订阅 {p}")));
            format!("卸载不完整：{}；清单：[{}]", self.plugin, items.join("；"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use ling_sdk::EffectKind;

    use super::Kernel;
    use crate::manifest::Manifest;
    use crate::state::{Handler, MethodEntry, PluginRecord, PluginState};

    const MANIFEST_TOML: &str = r#"
[plugin]
id = "fx-test"
version = "0.1.0"
api = "1"
"#;

    #[test]
    fn uninstall_continues_past_failed_undo_and_reports_incomplete() {
        let root = std::env::temp_dir().join(format!("ling-uninstall-unit-{}", std::process::id()));
        let kernel = Kernel::new(root.join("ling")).expect("kernel");

        let handler: Handler = Arc::new(|_| Ok(ling_sdk::Value::Null));
        kernel.shared.methods.write().unwrap().insert(
            "fx-test.m".to_string(),
            MethodEntry {
                owner: "fx-test".to_string(),
                requires: None,
                handler,
            },
        );

        let manifest =
            Manifest::parse_and_validate(MANIFEST_TOML, Path::new("/")).expect("测试清单可解析");
        let mut record = PluginRecord {
            manifest,
            manifest_text: MANIFEST_TOML.to_string(),
            state: PluginState::Activated,
            instance: None,
            library: None,
            library_file: None,
            ledger: Vec::new(),
            next_seq: 1,
            deactivate_cbs: Vec::new(),
        };
        let shared_ok = Arc::clone(&kernel.shared);
        record.push_effect(
            EffectKind::MethodRegistered,
            "注册方法 fx-test.m",
            Box::new(move || {
                shared_ok.methods.write().unwrap().remove("fx-test.m");
                Ok(())
            }),
        );
        record.push_effect(
            EffectKind::MethodRegistered,
            "注册方法 fx-test.bad",
            Box::new(|| Err("注入错误：撤销失败".to_string())),
        );
        kernel
            .shared
            .plugins
            .write()
            .unwrap()
            .insert("fx-test".to_string(), record);

        let report = kernel
            .uninstall("fx-test", false)
            .expect("卸载返回报告而非中断");
        assert_eq!(report.effects_total, 2);
        assert!(!report.complete(), "{}", report.summary());
        assert_eq!(report.undo_failures.len(), 1);
        assert!(report.undo_failures[0].contains("[2]"));
        assert!(report.undo_failures[0].contains("注入错误：撤销失败"));
        assert!(report.summary().starts_with("卸载不完整"));
        assert!(report.summary().contains("清单"));

        assert!(
            report.remnant_methods.is_empty(),
            "失败条目之后其余撤销继续执行"
        );
        assert!(
            !kernel
                .shared
                .methods
                .read()
                .unwrap()
                .contains_key("fx-test.m"),
            "成功条目已撤销"
        );
        assert!(kernel.activated_plugins().is_empty());
    }
}
