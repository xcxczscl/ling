use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};

use ling_sdk::{Capability, EffectKind, Plugin, Value};

use crate::manifest::Manifest;

pub type Handler = Arc<dyn Fn(Value) -> Result<Value, ling_sdk::CallError> + Send + Sync>;
/// 订阅回调：携带主题与载荷（侧车转发事件需要主题；插件侧 SDK 回调只见载荷）。
pub type Callback = Arc<dyn Fn(&str, Value) + Send + Sync>;
pub type Undo = Box<dyn FnOnce() -> Result<(), String> + Send>;

pub struct MethodEntry {
    pub owner: String,
    pub requires: Option<Capability>,
    pub handler: Handler,
}

pub struct SubEntry {
    pub id: u64,
    pub owner: String,
    pub pattern: String,
    pub callback: Callback,
}

/// 已装载插件的清单能力声明。用户撤销黑名单是宿主级的（[`Shared::revoked`]，
/// 持久化于 `ling/registry.json`，`docs/06` §4.4），不随装载/卸载清除。
#[derive(Default)]
pub struct CapEntry {
    pub declared: BTreeSet<Capability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    Activating,
    Activated,
}

pub struct LedgerEntry {
    pub seq: u64,
    pub kind: EffectKind,
    pub desc: String,
    pub undo: Mutex<Option<Undo>>,
}

pub struct PluginRecord {
    pub manifest: Manifest,
    /// 装载时的清单原文（热更新回滚用：磁盘上的 plugin.toml 可能已被替换）。
    pub manifest_text: String,
    pub state: PluginState,
    pub instance: Option<Box<dyn Plugin>>,
    pub library: Option<libloading::Library>,
    /// 已装载动态库的文件名（装载后改名为内核私有名，释放 `{id}.so` 路径）。
    pub library_file: Option<String>,
    pub ledger: Vec<LedgerEntry>,
    pub next_seq: u64,
    pub deactivate_cbs: Vec<Mutex<Option<ling_sdk::UndoFn>>>,
}

impl PluginRecord {
    pub fn push_effect(&mut self, kind: EffectKind, desc: impl Into<String>, undo: Undo) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.ledger.push(LedgerEntry {
            seq,
            kind,
            desc: desc.into(),
            undo: Mutex::new(Some(undo)),
        });
        seq
    }

    pub fn push_deactivate_cb(&mut self, cb: ling_sdk::UndoFn) {
        self.deactivate_cbs.push(Mutex::new(Some(cb)));
    }

    pub fn take_deactivate_cbs(&mut self) -> Vec<ling_sdk::UndoFn> {
        std::mem::take(&mut self.deactivate_cbs)
            .into_iter()
            .filter_map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
            .collect()
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub plugin: String,
    pub kind: &'static str,
    pub name: String,
}

pub struct Shared {
    pub data_root: PathBuf,
    pub methods: RwLock<HashMap<String, MethodEntry>>,
    pub subs: RwLock<Vec<SubEntry>>,
    pub caps: RwLock<HashMap<String, CapEntry>>,
    pub plugins: RwLock<HashMap<String, PluginRecord>>,
    pub violations: Mutex<Vec<Violation>>,
    pub sub_seq: AtomicU64,
    /// 配置变更串行闸（install/uninstall/enable/disable/reconcile/reload/授权），
    /// 保证调和读到一致的配置快照。
    pub config_gate: Mutex<()>,
    /// 停用名单（`docs/03` §2.2，宿主级，持久化于 `ling/registry.json`）。
    pub disabled: RwLock<BTreeSet<String>>,
    /// 能力撤销黑名单（`docs/06` §4.4，宿主级，持久化于 `ling/registry.json`）。
    pub revoked: RwLock<BTreeMap<String, BTreeSet<Capability>>>,
    pub plan_seq: AtomicU64,
    /// 动态库私有文件名序号（`docs/05` §3.1 对策②）。
    pub lib_seq: AtomicU64,
    /// 进程外侧车句柄（`docs/04` §6；停用/崩溃路径据此收敛进程）。
    pub sidecars: Mutex<BTreeMap<String, Arc<SidecarHandle>>>,
}

/// 侧车进程句柄（`sidecar.rs` 定义；此处仅存表）。
pub type SidecarHandle = crate::sidecar::SidecarHandle;

impl Shared {
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            methods: RwLock::new(HashMap::new()),
            subs: RwLock::new(Vec::new()),
            caps: RwLock::new(HashMap::new()),
            plugins: RwLock::new(HashMap::new()),
            violations: Mutex::new(Vec::new()),
            sub_seq: AtomicU64::new(1),
            config_gate: Mutex::new(()),
            disabled: RwLock::new(BTreeSet::new()),
            revoked: RwLock::new(BTreeMap::new()),
            plan_seq: AtomicU64::new(1),
            lib_seq: AtomicU64::new(1),
            sidecars: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.data_root.join("plugins")
    }

    pub fn plugin_dir(&self, id: &str) -> PathBuf {
        self.plugins_dir().join(id)
    }

    pub fn full_method_name(plugin: &str, method: &str) -> String {
        if method.starts_with(plugin) && method.as_bytes().get(plugin.len()) == Some(&b'.') {
            method.to_string()
        } else {
            format!("{plugin}.{method}")
        }
    }

    pub fn record_violation(&self, plugin: &str, kind: &'static str, name: &str) {
        self.violations.lock().unwrap().push(Violation {
            plugin: plugin.to_string(),
            kind,
            name: name.to_string(),
        });
    }

    pub fn dispatch(&self, topic: &str, payload: Value) {
        let targets: Vec<Callback> = {
            let subs = self.subs.read().unwrap();
            subs.iter()
                .filter(|s| topic_matches(&s.pattern, topic))
                .map(|s| Arc::clone(&s.callback))
                .collect()
        };
        for cb in targets {
            cb(topic, payload.clone());
        }
    }
}

pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    let p = pattern.as_bytes();
    let t = topic.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::topic_matches;

    #[test]
    fn glob_matching() {
        assert!(topic_matches("kernel.*", "kernel.plugin.state"));
        assert!(topic_matches("demo-a.*", "demo-a.x.y"));
        assert!(topic_matches("demo-a.*", "demo-a."));
        assert!(topic_matches("a.*.c", "a.b.b.b.c"));
        assert!(topic_matches("exact.topic", "exact.topic"));
        assert!(!topic_matches("exact.topic", "exact.topics"));
        assert!(!topic_matches("kernel.*", "kernelx.a"));
        assert!(topic_matches("*", "anything.at.all"));
        assert!(!topic_matches("a.*", "b.c"));
    }
}
