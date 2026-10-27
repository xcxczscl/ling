//! ling 插件开发套件：插件只需要依赖本 crate。
//!
//! 插件实现 [`Plugin`] trait，在 `activate` 里通过 [`Context`] 注册方法、订阅事件；
//! 所有跨插件数据用 [`Value`]（serde_json）承载，遵守序列化边界纪律。

use std::collections::BTreeSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub use serde_json::{json, Value};

/// `call_async` 返回的未来句柄。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 宿主当前接口主版本（清单 `api` 字段必须等于它）。
pub const HOST_API: u32 = 1;

/// 错误码（与 `docs/04-插件接口规范.md` §8 一一对应）。
pub mod codes {
    pub const E_MANIFEST_INVALID: &str = "E_MANIFEST_INVALID";
    pub const E_API_MISMATCH: &str = "E_API_MISMATCH";
    pub const E_CAPABILITY_UNKNOWN: &str = "E_CAPABILITY_UNKNOWN";
    pub const E_CAPABILITY_DENIED: &str = "E_CAPABILITY_DENIED";
    pub const E_NAMESPACE_VIOLATION: &str = "E_NAMESPACE_VIOLATION";
    pub const E_PLUGIN_MISSING: &str = "E_PLUGIN_MISSING";
    pub const E_METHOD_MISSING: &str = "E_METHOD_MISSING";
    pub const E_DEPENDENCY_UNMET: &str = "E_DEPENDENCY_UNMET";
    pub const E_DEPENDENCY_CYCLE: &str = "E_DEPENDENCY_CYCLE";
    pub const E_ACTIVATE_FAILED: &str = "E_ACTIVATE_FAILED";
    pub const E_UNDO_FAILED: &str = "E_UNDO_FAILED";
    pub const E_RELOAD_FAILED: &str = "E_RELOAD_FAILED";
    pub const E_TIMEOUT: &str = "E_TIMEOUT";
    pub const E_SIDE_CRASHED: &str = "E_SIDE_CRASHED";
    pub const E_AUTH: &str = "E_AUTH";
}

/// 能力（首版固定集，见 `docs/03-协效应与依赖解析.md` §1.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    Fs,
    Network,
    Ui,
    Shell,
}

impl Capability {
    pub const ALL: [Capability; 4] = [
        Capability::Fs,
        Capability::Network,
        Capability::Ui,
        Capability::Shell,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Fs => "fs",
            Capability::Network => "network",
            Capability::Ui => "ui",
            Capability::Shell => "shell",
        }
    }

    pub fn parse(s: &str) -> Option<Capability> {
        match s {
            "fs" => Some(Capability::Fs),
            "network" => Some(Capability::Network),
            "ui" => Some(Capability::Ui),
            "shell" => Some(Capability::Shell),
            _ => None,
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 跨插件调用错误（服务①/③ 的应答载体）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError {
    code: String,
    message: String,
}

impl CallError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn plugin_missing(plugin: &str) -> Self {
        Self::new(codes::E_PLUGIN_MISSING, format!("插件未激活：{plugin}"))
    }

    pub fn method_missing(method: &str) -> Self {
        Self::new(codes::E_METHOD_MISSING, format!("方法未注册：{method}"))
    }

    pub fn capability_denied(detail: impl Into<String>) -> Self {
        Self::new(codes::E_CAPABILITY_DENIED, detail)
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CallError {}

/// 上下文操作错误（注册/订阅被拒时返回，码与 [`CallError`] 同族）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextError {
    code: String,
    message: String,
}

impl ContextError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn namespace_violation(detail: impl Into<String>) -> Self {
        Self::new(codes::E_NAMESPACE_VIOLATION, detail)
    }

    pub fn capability_denied(detail: impl Into<String>) -> Self {
        Self::new(codes::E_CAPABILITY_DENIED, detail)
    }
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ContextError {}

/// 装载阶段 `activate` 失败（`E_ACTIVATE_FAILED`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivateError {
    message: String,
}

impl ActivateError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ActivateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ActivateError {}

/// 效应种类（账本条目分类，见 `docs/02` §3.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    MethodRegistered,
    EventSubscribed,
    ConfigWritten,
    SidecarStarted,
    DeactivateCallback,
}

impl EffectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EffectKind::MethodRegistered => "method_registered",
            EffectKind::EventSubscribed => "event_subscribed",
            EffectKind::ConfigWritten => "config_written",
            EffectKind::SidecarStarted => "sidecar_started",
            EffectKind::DeactivateCallback => "deactivate_callback",
        }
    }
}

/// 账本条目快照（供 `ctx.ledger()` 查看；撤销闭包不外泄）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntryInfo {
    pub seq: u64,
    pub kind: EffectKind,
    pub desc: String,
}

/// 插件必须实现的唯一 trait（`docs/04` §3.1）。
///
/// 动态库导出 `ling_plugin_create` 返回本 trait 的实例；
/// `activate` 里的所有注册/订阅都经 [`Context`]，内核记入账本。
pub trait Plugin: Send + Sync {
    /// 插件编号，必须与清单 `id` 一致（装载时核对）。
    fn id(&self) -> &str;

    /// 装载时调用。失败则内核执行已记账本的倒序撤销并卸载动态库。
    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError>;

    /// 停用/卸载/热更新前调用。在此收敛自己的线程、落盘。
    fn deactivate(&mut self);
}

/// 方法处理函数：入参/返回全部走序列化边界。
pub type MethodHandler = Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>;

/// 事件回调。
pub type EventCallback = Box<dyn Fn(Value) + Send + Sync>;

/// 撤销闭包（P3 纪律：注册必须同时给出）。
pub type UndoFn = Box<dyn FnOnce() + Send>;

/// 内核实现的上下文后端；插件通过 [`Context`] 句柄使用，不要直接实现本 trait。
pub trait ContextCore: Send + Sync {
    fn register_method(
        &self,
        plugin: &str,
        name: &str,
        handler: MethodHandler,
        unregister: UndoFn,
        requires: Option<Capability>,
    ) -> Result<(), ContextError>;

    fn call(&self, plugin: &str, method: &str, args: Value) -> Result<Value, CallError>;

    fn subscribe(
        &self,
        plugin: &str,
        pattern: &str,
        callback: EventCallback,
        unsubscribe: UndoFn,
    ) -> Result<(), ContextError>;

    fn emit(&self, plugin: &str, topic: &str, payload: Value) -> Result<(), ContextError>;

    fn data_dir(&self, plugin: &str) -> PathBuf;

    fn on_deactivate(&self, plugin: &str, callback: UndoFn);

    fn ledger_info(&self, plugin: &str) -> Vec<LedgerEntryInfo>;
}

/// 插件装载时由内核注入的句柄（`docs/02` §2.1）。
#[derive(Clone)]
pub struct Context {
    core: Arc<dyn ContextCore>,
    plugin_id: String,
    caps: BTreeSet<Capability>,
}

impl Context {
    /// 仅供内核装载插件时构造；插件不要调用。
    pub fn new(
        core: Arc<dyn ContextCore>,
        plugin_id: impl Into<String>,
        caps: BTreeSet<Capability>,
    ) -> Self {
        Self {
            core,
            plugin_id: plugin_id.into(),
            caps,
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// 注册接口方法。`name` 必须带本插件编号前缀；`requires` 注册时校验 ⊆ 本插件清单能力。
    pub fn register_method(
        &self,
        name: &str,
        handler: MethodHandler,
        unregister: UndoFn,
        requires: Option<Capability>,
    ) -> Result<(), ContextError> {
        self.core
            .register_method(&self.plugin_id, name, handler, unregister, requires)
    }

    /// 跨插件调用（同进程直调，走服务①路由与服务③门禁）。
    pub fn call(&self, plugin: &str, method: &str, args: Value) -> Result<Value, CallError> {
        self.core.call(plugin, method, args)
    }

    /// 异步调用：与 [`call`](Self::call) 同一语义，返回可组合的未来。
    pub fn call_async(
        &self,
        plugin: &str,
        method: &str,
        args: Value,
    ) -> BoxFuture<'static, Result<Value, CallError>> {
        let core = Arc::clone(&self.core);
        let plugin = plugin.to_string();
        let method = method.to_string();
        Box::pin(async move { core.call(&plugin, &method, args) })
    }

    /// 订阅事件（模式支持 `*` 通配，如 `kernel.*`）。`unsubscribe` 在退订时执行。
    pub fn subscribe(
        &self,
        topic_pattern: &str,
        callback: EventCallback,
        unsubscribe: UndoFn,
    ) -> Result<(), ContextError> {
        self.core
            .subscribe(&self.plugin_id, topic_pattern, callback, unsubscribe)
    }

    /// 广播事件。主题必须以本插件编号开头；越权广播被内核拒绝（返回
    /// `E_NAMESPACE_VIOLATION`）并记录（供 doctor 检查）。
    pub fn emit(&self, topic: &str, payload: Value) -> Result<(), ContextError> {
        self.core.emit(&self.plugin_id, topic, payload)
    }

    /// 读自己清单声明的能力。
    pub fn has_capability(&self, cap: Capability) -> bool {
        self.caps.contains(&cap)
    }

    /// 自己的数据目录 `ling/plugins/<编号>/data/`（内核保证存在）。
    pub fn data_dir(&self) -> PathBuf {
        self.core.data_dir(&self.plugin_id)
    }

    /// 登记停用/卸载前的收尾回调（落盘用，不是状态保存的替代）。
    pub fn on_deactivate(&self, callback: UndoFn) {
        self.core.on_deactivate(&self.plugin_id, callback);
    }

    /// 查看自己的效应账本（调试/日志用）。
    pub fn ledger(&self) -> Vec<LedgerEntryInfo> {
        self.core.ledger_info(&self.plugin_id)
    }
}
