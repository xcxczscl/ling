//! ling 内核：零内容宿主的四项服务（接口方法路由、事件广播、能力门禁、
//! 依赖解析）与三项机制（Context、效应账本、配置调和）。
//!
//! 阶段进度见 `docs/07-路线图与验收.md`：本 crate 当前覆盖 L1–L5（清单解析、
//! 装载、调用路由、事件广播、能力门禁、上下文与效应账本、卸载还原、依赖与
//! 配置调和、热更新、管理端口、内置插件目录与首启三件套）。

mod broker;
mod bus;
mod caps;
mod context;
mod deps;
mod error;
mod kernel;
mod ledger;
mod loader;
mod manifest;
mod registry;
mod sidecar;
mod state;

pub use deps::{PlanStep, ReconcileReport};
pub use error::KernelError;
pub use kernel::{Kernel, PluginStatus, ReloadReport, UninstallReport};
pub use manifest::Manifest;
pub use state::{PluginState, Violation};

pub mod admin;
pub mod builtin;
