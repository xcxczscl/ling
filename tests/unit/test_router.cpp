// 路由表的单元测试。规则来自
// docs/ling/04-详细设计/01-内核四服务.md §5.1（注册六步校验中的 2 至 6）与 §5.2、§5.3。
#include <catch_amalgamated.hpp>

#include <cstdlib>
#include <cstring>
#include <stdexcept>
#include <string>
#include <vector>

#include "ling/errors.hpp"
#include "ling/kernel.hpp"
#include "ling/router.hpp"

#include <nlohmann/json.hpp>

namespace {

int32_t echo_handler(void* ctx, const uint8_t* args, size_t args_len, uint8_t** out,
                     size_t* out_len) {
    (void)ctx;
    (void)args;
    (void)args_len;
    const char* text = "pong";
    const size_t len = std::strlen(text);
    auto* buffer = static_cast<uint8_t*>(std::malloc(len));
    std::memcpy(buffer, text, len);
    *out = buffer;
    *out_len = len;
    return 0;
}

int32_t failing_handler(void*, const uint8_t*, size_t, uint8_t**, size_t*) { return 1; }

int32_t failing_handler_seven(void*, const uint8_t*, size_t, uint8_t**, size_t*) { return 7; }

int32_t throwing_handler(void*, const uint8_t*, size_t, uint8_t**, size_t*) {
    throw std::runtime_error("插件内部炸了");
}

// 故意输出超过 1 MB：路由器要整段丢弃并按 E_PAYLOAD_TOO_LARGE 报错。
int32_t huge_output_handler(void*, const uint8_t*, size_t, uint8_t** out, size_t* out_len) {
    const size_t len = 1024 * 1024 + 1;
    auto* buffer = static_cast<uint8_t*>(std::malloc(len));
    if (buffer == nullptr) {
        return 1;
    }
    *out = buffer;
    *out_len = len;
    return 0;
}

}  // namespace

TEST_CASE("方法名合法性", "[router]") {
    CHECK(ling::valid_method_name("demo-a.ping"));
    CHECK(ling::valid_method_name("a.b_c1"));
    CHECK_FALSE(ling::valid_method_name(""));
    CHECK_FALSE(ling::valid_method_name("a b"));
    CHECK_FALSE(ling::valid_method_name(std::string(129, 'a')));
}

TEST_CASE("注册要求命名空间前缀", "[router]") {
    ling::Router router;
    CHECK(router.register_method("demo-a", "other.ping", reinterpret_cast<void*>(&echo_handler),
                                 nullptr, 0) == ling::err::kNamespaceViolation);
}

TEST_CASE("处理函数为空被拒", "[router]") {
    ling::Router router;
    CHECK(router.register_method("demo-a", "demo-a.ping", nullptr, nullptr, 0) ==
          ling::err::kLedgerInvalid);
}

TEST_CASE("重复注册同一方法返回 E_DUPLICATE_METHOD", "[router]") {
    ling::Router router;
    CHECK(router.register_method("demo-a", "demo-a.ping", reinterpret_cast<void*>(&echo_handler),
                                 nullptr, 0) == "");
    CHECK(router.register_method("demo-a", "demo-a.ping", reinterpret_cast<void*>(&echo_handler),
                                 nullptr, 0) == ling::err::kDuplicateMethod);
}

TEST_CASE("注销要求归属一致", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.ping", reinterpret_cast<void*>(&echo_handler), nullptr,
                           0);
    CHECK(router.unregister_method("demo-b", "demo-a.ping") == ling::err::kCapabilityDenied);
    CHECK(router.unregister_method("demo-a", "demo-a.ping") == "");
    CHECK(router.unregister_method("demo-a", "demo-a.ping") == ling::err::kMethodMissing);
}

TEST_CASE("调用成功返回载荷", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.ping", reinterpret_cast<void*>(&echo_handler), nullptr,
                           0);
    const auto outcome = router.call("demo-a.ping", nullptr, 0);
    REQUIRE(outcome.ok());
    CHECK(std::string(outcome.data.begin(), outcome.data.end()) == "pong");
}

TEST_CASE("调用未注册的方法返回 E_METHOD_MISSING", "[router]") {
    ling::Router router;
    const auto outcome = router.call("demo-a.nope", nullptr, 0);
    CHECK(outcome.code == ling::err::kMethodMissing);
}

TEST_CASE("载荷超过 1 MB 被拒", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.ping", reinterpret_cast<void*>(&echo_handler), nullptr,
                           0);
    std::vector<uint8_t> big(1024 * 1024 + 1, 0);
    const auto outcome = router.call("demo-a.ping", big.data(), big.size());
    CHECK(outcome.code == ling::err::kPayloadTooLarge);
}

namespace {

// 输出缓冲区非空但长度为 0：合法结果，路由器要归还缓冲并返回空数据。
int32_t zero_output_handler(void*, const uint8_t*, size_t, uint8_t** out, size_t* out_len) {
    auto* buffer = static_cast<uint8_t*>(std::malloc(16));
    if (buffer == nullptr) {
        return 1;
    }
    *out = buffer;
    *out_len = 0;
    return 0;
}

}  // namespace

TEST_CASE("输出缓冲区非空但长度为 0 时正常返回", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.zero",
                           reinterpret_cast<void*>(&zero_output_handler), nullptr, 0);
    const auto outcome = router.call("demo-a.zero", nullptr, 0);
    CHECK(outcome.code.empty());
    CHECK(outcome.data.empty());
}

TEST_CASE("处理函数返回非 0 时映射为 E_INTERNAL", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.boom", reinterpret_cast<void*>(&failing_handler),
                           nullptr, 0);
    const auto outcome = router.call("demo-a.boom", nullptr, 0);
    CHECK(outcome.code == ling::err::kInternal);
}

TEST_CASE("不同的非 0 返回码都按 E_INTERNAL 处理", "[router]") {
    // 进程内 ABI 只有 int32 返回码，映射不到字符串错误码表，所以 router 统一记日志后
    // 回落 E_INTERNAL；这条用例把这个语义钉住，避免有人以为能「原样透传」。
    ling::Router router;
    router.register_method("demo-a", "demo-a.seven",
                           reinterpret_cast<void*>(&failing_handler_seven), nullptr, 0);
    const auto outcome = router.call("demo-a.seven", nullptr, 0);
    CHECK(outcome.code == ling::err::kInternal);
    CHECK(outcome.data.empty());
}

TEST_CASE("处理函数抛异常不穿过边界，映射为 E_INTERNAL", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.throw", reinterpret_cast<void*>(&throwing_handler),
                           nullptr, 0);
    const auto outcome = router.call("demo-a.throw", nullptr, 0);
    CHECK(outcome.code == ling::err::kInternal);
    CHECK(outcome.data.empty());
}

TEST_CASE("输出超过 1 MB 被拒且不搬运数据", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.huge",
                           reinterpret_cast<void*>(&huge_output_handler), nullptr, 0);
    const auto outcome = router.call("demo-a.huge", nullptr, 0);
    CHECK(outcome.code == ling::err::kPayloadTooLarge);
    CHECK(outcome.data.empty());
}

TEST_CASE("批量注销按编号清空", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.one", reinterpret_cast<void*>(&echo_handler), nullptr, 0);
    router.register_method("demo-a", "demo-a.two", reinterpret_cast<void*>(&echo_handler), nullptr, 0);
    router.register_method("demo-b", "demo-b.one", reinterpret_cast<void*>(&echo_handler), nullptr, 0);
    const auto removed = router.unregister_all("demo-a");
    CHECK(removed.size() == 2);
    CHECK(router.size() == 1);
}

TEST_CASE("身份敏感方法用真实调用者覆盖 plugin", "[identity]") {
    const std::string args = R"({"plugin":"demo-b","key":"demo-b.timeout","value":30})";
    std::string rewritten;
    REQUIRE(ling::rewrite_caller_plugin(
        "core-settings.write", "demo-a", reinterpret_cast<const uint8_t*>(args.data()), args.size(),
        &rewritten));

    const auto parsed = nlohmann::json::parse(rewritten);
    CHECK(parsed["plugin"] == "demo-a");
    CHECK(parsed["key"] == "demo-b.timeout");
    CHECK(parsed["value"] == 30);
}

TEST_CASE("身份敏感方法缺 plugin 时补上真实调用者", "[identity]") {
    const std::string args = R"({"message":"hello"})";
    std::string rewritten;
    REQUIRE(ling::rewrite_caller_plugin("core-log.write", "demo-a",
                                       reinterpret_cast<const uint8_t*>(args.data()), args.size(),
                                       &rewritten));

    const auto parsed = nlohmann::json::parse(rewritten);
    CHECK(parsed["plugin"] == "demo-a");
    CHECK(parsed["message"] == "hello");
}

TEST_CASE("身份改写处理空参数与非字符串 plugin", "[identity]") {
    std::string rewritten;
    CHECK_FALSE(ling::rewrite_caller_plugin("core-log.write", "demo-a", nullptr, 0, &rewritten));

    const std::string args = R"({"plugin":42,"message":"hello"})";
    REQUIRE(ling::rewrite_caller_plugin("core-log.write", "demo-a",
                                       reinterpret_cast<const uint8_t*>(args.data()), args.size(),
                                       &rewritten));
    const auto parsed = nlohmann::json::parse(rewritten);
    CHECK(parsed["plugin"] == "demo-a");
    CHECK(parsed["message"] == "hello");
}

TEST_CASE("自报编号一致或方法不敏感时不改写", "[identity]") {
    const std::string args = R"({"plugin":"demo-a","message":"hello"})";
    std::string rewritten;
    CHECK_FALSE(ling::rewrite_caller_plugin("core-log.write", "demo-a",
                                            reinterpret_cast<const uint8_t*>(args.data()),
                                            args.size(), &rewritten));
    CHECK_FALSE(ling::rewrite_caller_plugin("demo-a.ping", "demo-a",
                                            reinterpret_cast<const uint8_t*>(args.data()),
                                            args.size(), &rewritten));
}
