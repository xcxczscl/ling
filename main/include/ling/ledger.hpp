// 效应账本。每次「会改变宿主状态」的动作登记一条，插件停用时按倒序撤销。
// 语义依据 docs/ling/04-详细设计/03-上下文与效应账本.md：
// 账本不落盘、按激活代分段、条目数上限 10000、撤销失败不回滚已撤销的部分。
#pragma once

#include <cstdint>
#include <functional>
#include <string>
#include <vector>

namespace ling {

// 五类效应里 S2 只实现前两类，其余在接入事件与配置后补。
enum class EffectKind { kMethodRegistered, kSubscribed, kFsWrite, kConfigWrite, kHandleOpened };

struct Effect {
    EffectKind kind = EffectKind::kMethodRegistered;
    std::string detail;                  // 方法名、主题、路径等，用于日志与排错
    std::function<bool()> undo;          // 返回 false 表示撤销失败
};

struct UndoReport {
    std::size_t attempted = 0;
    std::size_t failed = 0;
    std::vector<std::string> failed_items;
};

class Ledger {
public:
    static constexpr std::size_t kMaxEntries = 10000;

    // 追加一条效应。超过上限返回 false，调用方应返回 E_LEDGER_FULL。
    bool append(Effect effect);

    // 倒序撤销全部条目，撤销后清空。撤销失败的不中断后续撤销。
    UndoReport undo_all();

    std::size_t size() const { return entries_.size(); }
    bool empty() const { return entries_.empty(); }

private:
    std::vector<Effect> entries_;
};

}  // namespace ling
