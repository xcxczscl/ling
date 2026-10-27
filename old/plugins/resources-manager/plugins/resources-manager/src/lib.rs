//! resources-manager 插件入口（`docs/04` §1）：只做符号导出，业务在 `rm-plugin`。

use ling_sdk::Plugin;

pub use rm_plugin::ResourcesManager;

/// ling 装载约定的入口符号（`ling/docs/04` §2）。
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(ResourcesManager::new()))
}
