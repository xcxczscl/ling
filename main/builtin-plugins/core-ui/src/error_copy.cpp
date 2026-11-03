// 错误码文案表的实现。见 error_copy.hpp 的说明。
#include "error_copy.hpp"

#include <map>

#include <nlohmann/json.hpp>

#include "ling/errors.hpp"

namespace ling {
namespace ui {
namespace {

using Table = std::map<std::string, Copy>;

const Table& table() {
    static const Table kTable = {
        // —— core-ui 自己判定的 ——
        {err::kUiForbidden,
         {"面板不能调用这个插件的其它方法", "只能调用本插件命名空间下的方法"}},
        {err::kUiPanelNotFound, {"这个插件没有面板", "让插件作者在 plugin.toml 里写 [ui] 段"}},
        {err::kUiPanelDecl,
         {"插件清单的 [ui] 段不合法", "按文档改 entry / title / icon 三个字段"}},
        {err::kUiNotReady, {"面板还没收到 init 消息", "等收到 init 之后再发 call"}},
        {err::kUiBusy, {"同一时刻的请求太多", "等前一批返回再发，或降低轮询频率"}},
        {err::kProtocol, {"请求格式不对", "刷新界面再试；持续出现就记下操作步骤报问题"}},
        {err::kPayloadTooLarge, {"这次请求带的数据太大", "拖拽请传路径而不是内容；请求体不超过 1 MB"}},
        {err::kTimeout, {"ling 没有响应，正在忙或已卡住", "等几秒再试；还没有反应就看宿主窗口的输出"}},

        // —— 宿主转发的：message 用宿主原话，这里只给 advice ——
        {err::kPluginMissing, {"", "先看看装了哪些插件：插件页或 ling plugins"}},
        {err::kMethodMissing, {"", "核对方法名，命令 ling plugins 里能看到每个插件的方法"}},
        {err::kNamespaceViolation, {"", "检查请求的方法名是不是越界了"}},
        {err::kManifestInvalid, {"", "看插件目录里的 plugin.toml 是否完整，产物文件在不在"}},
        {err::kApiMismatch, {"", "这个插件是给别的接口版本写的，换一个与当前 ling 匹配的版本"}},
        {err::kHostTooOld, {"", "升级 ling 再装这个插件"}},
        {err::kCapabilityUnknown, {"", "清单里的 capabilities 只认 fs、network、ui、shell"}},
        {err::kCapabilityDenied, {"", "把这个插件需要的能力补进清单，或换一个实现方式"}},
        {err::kDependencyCycle, {"", "把成环的依赖去掉一条"}},
        {err::kPluginExists, {"", "要先卸载旧版本，或者换一个插件名"}},
        {err::kUninstallIncomplete, {"", "看插件页里剩下的文件，手动删掉后重试"}},
        {err::kReloadFailed, {"", "看日志确认新产物能不能装载，必要时退回旧版本"}},
        {err::kIo, {"", "检查数据目录的写权限和磁盘剩余空间"}},
        {err::kInternal, {"", "看宿主窗口的输出，那里有具体原因"}},
        {err::kAuth, {"", "从 ling 主程序重新打开界面（口令每次启动宿主都会换）"}},
        {err::kShuttingDown, {"", "等 ling 重新起来再操作"}},
    };
    return kTable;
}

}  // namespace

Copy copy_for(const std::string& code) {
    const auto& all = table();
    const auto it = all.find(code);
    if (it == all.end()) {
        return {};
    }
    return it->second;
}

std::string error_json(const std::string& code, const std::string& message,
                       const std::string& detail) {
    const Copy copy = copy_for(code);
    nlohmann::json error;
    error["code"] = code;
    // message 恒有：调用方给了就用调用方的（宿主原话），没给才用表里的。
    const std::string text = message.empty() ? copy.message : message;
    error["message"] = text.empty() ? code : text;
    // advice 没有就整条省略，不写 null、不写空串：面板侧用 "advice" in error 判断。
    if (!copy.advice.empty()) {
        error["advice"] = copy.advice;
    }
    // detail 是下游带出来的结构化补充，原样透传，截断只发生在展示层。
    if (!detail.empty()) {
        const auto parsed = nlohmann::json::parse(detail, nullptr, false);
        error["detail"] = parsed.is_discarded() ? nlohmann::json(detail) : parsed;
    }
    nlohmann::json body;
    body["ok"] = false;
    body["error"] = std::move(error);
    return body.dump();
}

}  // namespace ui
}  // namespace ling
