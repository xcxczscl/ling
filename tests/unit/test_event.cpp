// 事件订阅表与派发的单元测试。
// 依据 docs/ling/04-详细设计/01-内核四服务.md §6 与 §3.2 的订阅表不变式。
#include <catch_amalgamated.hpp>

#include <cstdint>
#include <string>
#include <vector>

#include "ling/errors.hpp"
#include "ling/event.hpp"

namespace {

struct Recorder {
    std::vector<std::string> topics;
    std::vector<std::string> payloads;
    int32_t return_code = 0;
    // 非空时，回调第一次被调用后执行它，用来验证「回调内退订」不会破坏派发循环。
    void (*on_first_call)(void*) = nullptr;
    void* on_first_call_arg = nullptr;
    int calls = 0;
};

int32_t record_event(void* handler_ctx, const char* topic, const uint8_t* payload, size_t len) {
    auto* recorder = static_cast<Recorder*>(handler_ctx);
    if (recorder == nullptr) {
        return 1;
    }
    recorder->topics.emplace_back(topic == nullptr ? "" : topic);
    recorder->payloads.emplace_back(reinterpret_cast<const char*>(payload), len);
    ++recorder->calls;
    if (recorder->calls == 1 && recorder->on_first_call != nullptr) {
        recorder->on_first_call(recorder->on_first_call_arg);
    }
    return recorder->return_code;
}

int32_t always_ok(void*, const char*, const uint8_t*, size_t) { return 0; }

}  // namespace

TEST_CASE("主题形状校验", "[event]") {
    REQUIRE(ling::valid_topic_shape("demo-a.pinged"));
    REQUIRE(ling::valid_topic_shape("kernel.plugin.state"));
    REQUIRE(ling::valid_topic_shape("a.b"));

    REQUIRE_FALSE(ling::valid_topic_shape(""));
    REQUIRE_FALSE(ling::valid_topic_shape("nodot"));          // 至少两段
    REQUIRE_FALSE(ling::valid_topic_shape(".starts"));
    REQUIRE_FALSE(ling::valid_topic_shape("ends."));
    REQUIRE_FALSE(ling::valid_topic_shape("double..dot"));
    REQUIRE_FALSE(ling::valid_topic_shape("has space.x"));
    REQUIRE_FALSE(ling::valid_topic_shape("star.*"));
    REQUIRE_FALSE(ling::valid_topic_shape(std::string(129, 'a') + ".b"));
}

TEST_CASE("订阅模式形状", "[event]") {
    REQUIRE(ling::valid_pattern_shape("*"));
    REQUIRE(ling::valid_pattern_shape("demo-a.*"));
    REQUIRE(ling::valid_pattern_shape("demo-a.pinged"));

    REQUIRE_FALSE(ling::valid_pattern_shape(""));
    REQUIRE_FALSE(ling::valid_pattern_shape("demo-*"));      // 只允许末尾通配
    REQUIRE_FALSE(ling::valid_pattern_shape("a.*.c"));       // 不允许中缀通配
    REQUIRE_FALSE(ling::valid_pattern_shape("*.pinged"));    // 不允许前缀通配
    REQUIRE_FALSE(ling::valid_pattern_shape("demo-a.*."));
}

TEST_CASE("模式匹配", "[event]") {
    // 整串星号收全部事件，包括内核事件。
    REQUIRE(ling::pattern_matches("*", "demo-a.pinged"));
    REQUIRE(ling::pattern_matches("*", "kernel.plugin.state"));

    // 末尾通配要求主题在该前缀之后还有内容，且前缀以点结尾。
    REQUIRE(ling::pattern_matches("demo-a.*", "demo-a.pinged"));
    REQUIRE(ling::pattern_matches("demo-a.*", "demo-a.x.y"));
    REQUIRE_FALSE(ling::pattern_matches("demo-a.*", "demo-ab.pinged"));
    REQUIRE_FALSE(ling::pattern_matches("demo-a.*", "demo-a."));

    // 精确主题只命中自己。
    REQUIRE(ling::pattern_matches("demo-a.pinged", "demo-a.pinged"));
    REQUIRE_FALSE(ling::pattern_matches("demo-a.pinged", "demo-a.pinged2"));
}

TEST_CASE("订阅成功分配插件内自增的 sub_id", "[event]") {
    ling::EventBus bus;
    std::uint64_t first = 0;
    std::uint64_t second = 0;
    REQUIRE(bus.subscribe("demo-caller", "demo-a.*", reinterpret_cast<void*>(&always_ok), nullptr,
                          &first)
                .empty());
    REQUIRE(bus.subscribe("demo-caller", "kernel.plugin.state",
                          reinterpret_cast<void*>(&always_ok), nullptr, &second)
                .empty());
    REQUIRE(first == 1);
    REQUIRE(second == 2);
    REQUIRE(bus.size() == 2);

    // 编号按插件各自自增，互不影响。
    std::uint64_t other = 0;
    REQUIRE(bus.subscribe("demo-z", "demo-a.*", reinterpret_cast<void*>(&always_ok), nullptr,
                          &other)
                .empty());
    REQUIRE(other == 1);
}

TEST_CASE("订阅的负向校验", "[event]") {
    ling::EventBus bus;
    std::uint64_t sub_id = 0;

    // 回调为空。
    REQUIRE(bus.subscribe("demo-a", "demo-a.*", nullptr, nullptr, &sub_id) ==
            ling::err::kLedgerInvalid);
    // 形状不合法。
    REQUIRE(bus.subscribe("demo-a", "a.*.c", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id) == ling::err::kNamespaceViolation);
    REQUIRE(bus.subscribe("demo-a", "nodot", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id) == ling::err::kNamespaceViolation);
    REQUIRE(bus.size() == 0);
}

TEST_CASE("同一插件的通配订阅上限 32 条", "[event]") {
    ling::EventBus bus;
    std::uint64_t sub_id = 0;
    for (std::size_t i = 0; i < ling::EventBus::kMaxWildcardPerPlugin; ++i) {
        REQUIRE(bus.subscribe("demo-a", "demo-a.*", reinterpret_cast<void*>(&always_ok), nullptr,
                              &sub_id)
                    .empty());
    }
    REQUIRE(bus.subscribe("demo-a", "demo-a.*", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id) == ling::err::kCapabilityDenied);

    // 精确主题不受通配上限约束。
    REQUIRE(bus.subscribe("demo-a", "demo-a.pinged", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id)
                .empty());
    // 换一个插件重新计数。
    REQUIRE(bus.subscribe("demo-b", "demo-b.*", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id)
                .empty());
}

TEST_CASE("退订", "[event]") {
    ling::EventBus bus;
    std::uint64_t sub_id = 0;
    REQUIRE(bus.subscribe("demo-a", "demo-a.*", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id)
                .empty());

    // 别的插件不能退掉这条订阅。
    REQUIRE(bus.unsubscribe("demo-b", sub_id) == ling::err::kPluginMissing);
    REQUIRE(bus.size() == 1);

    REQUIRE(bus.unsubscribe("demo-a", sub_id).empty());
    REQUIRE(bus.size() == 0);
    // 重复退订视为不存在。
    REQUIRE(bus.unsubscribe("demo-a", sub_id) == ling::err::kPluginMissing);
}

TEST_CASE("批量退订只影响目标插件", "[event]") {
    ling::EventBus bus;
    std::uint64_t sub_id = 0;
    REQUIRE(bus.subscribe("demo-a", "demo-a.*", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id)
                .empty());
    REQUIRE(bus.subscribe("demo-a", "kernel.plugin.state", reinterpret_cast<void*>(&always_ok),
                         nullptr, &sub_id)
                .empty());
    REQUIRE(bus.subscribe("demo-b", "demo-a.*", reinterpret_cast<void*>(&always_ok), nullptr,
                          &sub_id)
                .empty());

    const auto removed = bus.unsubscribe_all("demo-a");
    REQUIRE(removed.size() == 2);
    REQUIRE(removed[0] == "demo-a.*");
    REQUIRE(removed[1] == "kernel.plugin.state");
    REQUIRE(bus.size() == 1);
    REQUIRE(bus.match("demo-a.pinged").size() == 1);
}

TEST_CASE("派发按订阅登记顺序", "[event]") {
    ling::EventBus bus;
    Recorder first;
    Recorder second;
    Recorder third;
    std::uint64_t sub_id = 0;

    REQUIRE(bus.subscribe("demo-b", "demo-a.*", reinterpret_cast<void*>(&record_event), &first,
                          &sub_id)
                .empty());
    REQUIRE(bus.subscribe("demo-c", "demo-a.pinged",
                          reinterpret_cast<void*>(&record_event), &second, &sub_id)
                .empty());
    REQUIRE(bus.subscribe("demo-d", "*", reinterpret_cast<void*>(&record_event), &third, &sub_id)
                .empty());

    const std::string payload = "pong";
    const auto stats = bus.dispatch("demo-a.pinged",
                                    reinterpret_cast<const std::uint8_t*>(payload.data()),
                                    payload.size());

    REQUIRE(stats.recipients == 3);
    REQUIRE(stats.delivered == 3);
    REQUIRE(stats.failed == 0);
    REQUIRE(first.topics.size() == 1);
    REQUIRE(first.topics[0] == "demo-a.pinged");
    REQUIRE(first.payloads[0] == "pong");
    REQUIRE(second.calls == 1);
    REQUIRE(third.calls == 1);
}

TEST_CASE("派发不命中任何订阅者也算成功", "[event]") {
    ling::EventBus bus;
    const auto stats = bus.dispatch("demo-a.pinged", nullptr, 0);
    REQUIRE(stats.recipients == 0);
    REQUIRE(stats.delivered == 0);
    REQUIRE(stats.code.empty());
}

TEST_CASE("回调返回非 0 只记账，不中断其他订阅者", "[event]") {
    ling::EventBus bus;
    Recorder failing;
    failing.return_code = 7;
    Recorder healthy;
    std::uint64_t sub_id = 0;

    REQUIRE(bus.subscribe("demo-b", "demo-a.*", reinterpret_cast<void*>(&record_event), &failing,
                          &sub_id)
                .empty());
    REQUIRE(bus.subscribe("demo-c", "demo-a.*", reinterpret_cast<void*>(&record_event), &healthy,
                          &sub_id)
                .empty());

    const auto stats = bus.dispatch("demo-a.pinged", nullptr, 0);
    REQUIRE(stats.recipients == 2);
    REQUIRE(stats.delivered == 1);
    REQUIRE(stats.failed == 1);
    REQUIRE(healthy.calls == 1);
}

namespace {

void unsubscribe_self(void* arg) {
    auto* pair = static_cast<std::pair<ling::EventBus*, std::uint64_t>*>(arg);
    pair->first->unsubscribe("demo-b", pair->second);
}

}  // namespace

TEST_CASE("回调内退订自己不会破坏派发循环", "[event]") {
    ling::EventBus bus;
    std::uint64_t sub_id = 0;
    std::pair<ling::EventBus*, std::uint64_t> context{&bus, 0};

    Recorder first;
    first.on_first_call = &unsubscribe_self;
    first.on_first_call_arg = &context;
    REQUIRE(bus.subscribe("demo-b", "demo-a.*", reinterpret_cast<void*>(&record_event), &first,
                          &sub_id)
                .empty());
    context.second = sub_id;

    Recorder second;
    std::uint64_t other_id = 0;
    REQUIRE(bus.subscribe("demo-c", "demo-a.*", reinterpret_cast<void*>(&record_event), &second,
                          &other_id)
                .empty());

    // 派发前复制了收件人列表，所以第一个回调里退订不会让第二个收件人消失。
    const auto stats = bus.dispatch("demo-a.pinged", nullptr, 0);
    REQUIRE(stats.recipients == 2);
    REQUIRE(second.calls == 1);
    REQUIRE(bus.size() == 1);
}
