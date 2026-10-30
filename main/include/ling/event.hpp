// 事件订阅表与派发。语义依据 docs/ling/04-详细设计/01-内核四服务.md §3.2、§6。
// 本期是单线程同步派发：广播在调用方线程执行，不分队列、不重投。
#pragma once

#include <cstddef>
#include <cstdint>
#include <map>
#include <string>
#include <vector>

namespace ling {

struct SubEntry {
    std::string plugin_id;
    std::string pattern;        // 精确主题、demo-a.* 或 *
    void* callback = nullptr;   // ling_event_fn
    void* callback_ctx = nullptr;
    std::uint64_t sub_id = 0;   // 同一插件内唯一，从 1 开始
    std::uint64_t register_seq = 0;  // 全局登记序号，派发顺序依据
};

struct EmitStats {
    std::string code;            // 非空表示这次广播根本没发出去（越权、超限）
    std::size_t recipients = 0;  // 命中的订阅数
    std::size_t delivered = 0;   // 回调返回 0 的
    std::size_t failed = 0;      // 回调返回非 0 的
    std::size_t slow = 0;        // 耗时超过阈值的
};

class EventBus {
public:
    static constexpr std::size_t kMaxWildcardPerPlugin = 32;
    static constexpr std::size_t kMaxPayloadBytes = 1024 * 1024;
    static constexpr std::uint32_t kCallbackTimeoutMs = 5000;

    // 第 3、4、5 条校验。第 1 条（插件状态）与第 2 条（回调非空）由调用方先做。
    std::string subscribe(const std::string& plugin_id, const std::string& pattern, void* fn,
                          void* callback_ctx, std::uint64_t* out_sub_id);

    // 按 sub_id 退订；插件不匹配或编号不存在返回 E_PLUGIN_MISSING。
    std::string unsubscribe(const std::string& plugin_id, std::uint64_t sub_id);

    // 退订某插件的全部订阅，返回被退订的模式列表（按登记顺序），供账本对账。
    std::vector<std::string> unsubscribe_all(const std::string& plugin_id);

    // 派发到命中的订阅者。订阅者列表在派发前复制，回调内退订不会让迭代失效。
    EmitStats dispatch(const std::string& topic, const std::uint8_t* payload, std::size_t len);

    // 命中的订阅者，按 register_seq 升序。
    std::vector<SubEntry> match(const std::string& topic) const;

    std::size_t size() const { return subs_.size(); }
    std::size_t wildcard_count(const std::string& plugin_id) const;
    const std::vector<SubEntry>& entries() const { return subs_; }

private:
    // 始终保持按 register_seq 升序：追加即有序，退订后仍有序，派发不必再排序。
    std::vector<SubEntry> subs_;
    std::map<std::string, std::uint64_t> next_sub_id_;
    std::uint64_t next_seq_ = 1;
};

// 主题形状：非空、不超过 128 字节、只含字母数字与 . _ -、至少含一个点或等于 *。
bool valid_topic_shape(const std::string& topic);

// 通配规则：整串 * 或 <前缀>.* ，不允许中缀通配。
bool valid_pattern_shape(const std::string& pattern);

// 模式是否命中主题。
bool pattern_matches(const std::string& pattern, const std::string& topic);

}  // namespace ling
