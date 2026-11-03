// 错误码常量。取值与含义以 docs/ling/04-详细设计/02-插件装载与接口.md 第 5 节
// 「错误码登记表」为唯一来源；这里只做字符串常量，方便返回值与日志统一。
#pragma once

namespace ling {

namespace err {
inline constexpr const char* kManifestInvalid = "E_MANIFEST_INVALID";
inline constexpr const char* kApiMismatch = "E_API_MISMATCH";
inline constexpr const char* kHostTooOld = "E_HOST_TOO_OLD";
inline constexpr const char* kNamespaceViolation = "E_NAMESPACE_VIOLATION";
inline constexpr const char* kCapabilityUnknown = "E_CAPABILITY_UNKNOWN";
inline constexpr const char* kCapabilityDenied = "E_CAPABILITY_DENIED";
inline constexpr const char* kPluginMissing = "E_PLUGIN_MISSING";
inline constexpr const char* kMethodMissing = "E_METHOD_MISSING";
inline constexpr const char* kTimeout = "E_TIMEOUT";
inline constexpr const char* kPayloadTooLarge = "E_PAYLOAD_TOO_LARGE";
inline constexpr const char* kDependencyCycle = "E_DEPENDENCY_CYCLE";
inline constexpr const char* kReloadFailed = "E_RELOAD_FAILED";
// 保留未用：撤销失败目前用 E_UNINSTALL_INCOMPLETE 表达。
inline constexpr const char* kUndoFailed = "E_UNDO_FAILED";
inline constexpr const char* kLedgerInvalid = "E_LEDGER_INVALID";
inline constexpr const char* kAuth = "E_AUTH";
inline constexpr const char* kPluginExists = "E_PLUGIN_EXISTS";
inline constexpr const char* kProtocol = "E_PROTOCOL";
inline constexpr const char* kIo = "E_IO";
inline constexpr const char* kUninstallIncomplete = "E_UNINSTALL_INCOMPLETE";
inline constexpr const char* kDuplicateMethod = "E_DUPLICATE_METHOD";
inline constexpr const char* kInternal = "E_INTERNAL";
// 保留未用：关机流程当前直接拒绝新请求并断开，没有单独回这个码。
inline constexpr const char* kShuttingDown = "E_SHUTTING_DOWN";
inline constexpr const char* kLedgerFull = "E_LEDGER_FULL";
// 保留未用：批量计划的取消当前只回 succeeded=false，没有单独回这个码。
inline constexpr const char* kCancelled = "E_CANCELLED";
inline constexpr const char* kResidentLimit = "E_RESIDENT_LIMIT";

// core-ui 的面板机制专用。语义见
// docs/ling/builtin-plugins/core-ui/03-架构设计.md §9，
// 取值登记在 docs/ling/plugin-support/01-插件装载与接口.md §5.2。
inline constexpr const char* kUiPanelDecl = "E_UI_PANEL_DECL";
inline constexpr const char* kUiPanelNotFound = "E_UI_PANEL_NOTFOUND";
inline constexpr const char* kUiNotReady = "E_UI_NOT_READY";
inline constexpr const char* kUiForbidden = "E_UI_FORBIDDEN";
inline constexpr const char* kUiBusy = "E_UI_BUSY";
}  // namespace err

}  // namespace ling
