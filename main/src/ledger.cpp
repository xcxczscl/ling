#include "ling/ledger.hpp"

#include <utility>

namespace ling {

bool Ledger::append(Effect effect) {
    if (entries_.size() >= kMaxEntries) {
        return false;
    }
    entries_.push_back(std::move(effect));
    return true;
}

UndoReport Ledger::undo_all() {
    UndoReport report;

    // 倒序：后登记的先撤销。正序撤会让「先注册 A 再注册 A 的依赖」这类顺序失效。
    for (auto it = entries_.rbegin(); it != entries_.rend(); ++it) {
        report.attempted++;
        bool ok = false;
        try {
            ok = it->undo ? it->undo() : false;
        } catch (...) {
            ok = false;
        }
        if (!ok) {
            report.failed++;
            report.failed_items.push_back(it->detail);
        }
    }
    entries_.clear();
    return report;
}

}  // namespace ling
