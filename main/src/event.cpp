#include "ling/event.hpp"

#include <cctype>
#include <chrono>
#include <exception>
#include <utility>

#include "ling/errors.hpp"
#include "ling/log.hpp"
#include "ling/plugin_api.h"

namespace ling {
namespace {

constexpr std::size_t kMaxTopicBytes = 128;

}  // namespace

bool valid_topic_shape(const std::string& topic) {
    if (topic.empty() || topic.size() > kMaxTopicBytes) {
        return false;
    }
    if (topic.find('.') == std::string::npos) {
        return false;  // 主题至少两段，形如 demo-a.pinged
    }
    for (char ch : topic) {
        const auto uch = static_cast<unsigned char>(ch);
        if (std::isalnum(uch) == 0 && ch != '_' && ch != '.' && ch != '-') {
            return false;
        }
    }
    // 空段（首尾点、连续点）不合规，避免 demo-a..x 这类拼写混进订阅表。
    if (topic.front() == '.' || topic.back() == '.' ||
        topic.find("..") != std::string::npos) {
        return false;
    }
    return true;
}

bool valid_pattern_shape(const std::string& pattern) {
    if (pattern == "*") {
        return true;
    }
    // 只允许末尾通配：<前缀>.*
    const std::string suffix = ".*";
    if (pattern.size() > suffix.size() &&
        pattern.compare(pattern.size() - suffix.size(), suffix.size(), suffix) == 0) {
        const std::string prefix = pattern.substr(0, pattern.size() - suffix.size());
        if (prefix.find('*') != std::string::npos) {
            return false;
        }
        // 前缀按普通主题校验，但可以只有一段（demo-a.*）。
        return valid_topic_shape(prefix + ".x");
    }
    if (pattern.find('*') != std::string::npos) {
        return false;  // 中缀或整段通配一律拒绝
    }
    return valid_topic_shape(pattern);
}

bool pattern_matches(const std::string& pattern, const std::string& topic) {
    if (pattern == "*") {
        return true;
    }
    const std::string suffix = ".*";
    if (pattern.size() > suffix.size() &&
        pattern.compare(pattern.size() - suffix.size(), suffix.size(), suffix) == 0) {
        // 保留末位的点一起比较：demo-a. 命中 demo-a.x，不命中 demo-abc。
        const std::string prefix = pattern.substr(0, pattern.size() - 1);
        return topic.size() > prefix.size() && topic.compare(0, prefix.size(), prefix) == 0;
    }
    return pattern == topic;
}

std::size_t EventBus::wildcard_count(const std::string& plugin_id) const {
    std::size_t count = 0;
    for (const auto& sub : subs_) {
        if (sub.plugin_id == plugin_id && sub.pattern.find('*') != std::string::npos) {
            ++count;
        }
    }
    return count;
}

std::string EventBus::subscribe(const std::string& plugin_id, const std::string& pattern,
                                void* fn, void* callback_ctx, std::uint64_t* out_sub_id) {
    // 第 2 条：回调非空。
    if (fn == nullptr) {
        return err::kLedgerInvalid;
    }
    // 第 3、4 条：形状与通配位置。
    if (!valid_pattern_shape(pattern)) {
        return err::kNamespaceViolation;
    }
    // 第 5 条：通配订阅上限。
    if (pattern.find('*') != std::string::npos &&
        wildcard_count(plugin_id) >= kMaxWildcardPerPlugin) {
        return err::kCapabilityDenied;
    }

    const std::uint64_t sub_id = ++next_sub_id_[plugin_id];
    SubEntry entry;
    entry.plugin_id = plugin_id;
    entry.pattern = pattern;
    entry.callback = fn;
    entry.callback_ctx = callback_ctx;
    entry.sub_id = sub_id;
    entry.register_seq = next_seq_++;
    subs_.push_back(std::move(entry));

    if (out_sub_id != nullptr) {
        *out_sub_id = sub_id;
    }
    return {};
}

std::string EventBus::unsubscribe(const std::string& plugin_id, std::uint64_t sub_id) {
    for (auto it = subs_.begin(); it != subs_.end(); ++it) {
        if (it->sub_id == sub_id && it->plugin_id == plugin_id) {
            subs_.erase(it);
            return {};
        }
    }
    return err::kPluginMissing;
}

std::vector<std::string> EventBus::unsubscribe_all(const std::string& plugin_id) {
    std::vector<std::string> removed;
    for (auto it = subs_.begin(); it != subs_.end();) {
        if (it->plugin_id == plugin_id) {
            removed.push_back(it->pattern);
            it = subs_.erase(it);
        } else {
            ++it;
        }
    }
    return removed;
}

std::vector<SubEntry> EventBus::match(const std::string& topic) const {
    std::vector<SubEntry> result;
    // subs_ 按 register_seq 升序存放，顺序扫描得到的就是派发顺序，不必再排。
    for (const auto& sub : subs_) {
        if (pattern_matches(sub.pattern, topic)) {
            result.push_back(sub);
        }
    }
    return result;
}

EmitStats EventBus::dispatch(const std::string& topic, const std::uint8_t* payload,
                             std::size_t len) {
    EmitStats stats;
    // 复制收件人：回调里退订或新增订阅会改动 subs_，持有指针或引用都会失效。
    const std::vector<SubEntry> recipients = match(topic);
    stats.recipients = recipients.size();

    for (const auto& sub : recipients) {
        const auto fn = reinterpret_cast<ling_event_fn>(sub.callback);
        if (fn == nullptr) {
            ++stats.failed;
            continue;
        }
        const auto started = std::chrono::steady_clock::now();
        int32_t code = 0;
        try {
            code = fn(sub.callback_ctx, topic.c_str(), payload, len);
        } catch (...) {
            // 进程内同步调用，插件的异常不能穿过派发循环。
            code = -1;
        }
        const auto cost = std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now() - started);

        if (code != 0) {
            ++stats.failed;
            log_warn("event", topic + " 订阅者 " + sub.plugin_id + "（sub_id=" +
                                   std::to_string(sub.sub_id) + "）返回 " +
                                   std::to_string(code));
        } else {
            ++stats.delivered;
        }
        // 同步调用无法抢占：超过阈值的只能事后记账，做不到中途跳过。
        if (cost.count() > static_cast<long long>(kCallbackTimeoutMs)) {
            ++stats.slow;
            log_warn("event", topic + " 订阅者 " + sub.plugin_id + " 耗时 " +
                                   std::to_string(cost.count()) + " 毫秒，超过 " +
                                   std::to_string(kCallbackTimeoutMs) + " 毫秒阈值");
        }
    }
    return stats;
}

}  // namespace ling
