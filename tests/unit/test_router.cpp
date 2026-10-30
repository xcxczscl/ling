// 路由表的单元测试。规则来自
// docs/ling/04-详细设计/01-内核四服务.md §5.1（注册六步校验中的 2 至 6）与 §5.2、§5.3。
#include <catch_amalgamated.hpp>

#include <cstring>
#include <string>
#include <vector>

#include "ling/errors.hpp"
#include "ling/router.hpp"

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

TEST_CASE("处理函数返回非 0 时映射为 E_INTERNAL", "[router]") {
    ling::Router router;
    router.register_method("demo-a", "demo-a.boom", reinterpret_cast<void*>(&failing_handler),
                           nullptr, 0);
    const auto outcome = router.call("demo-a.boom", nullptr, 0);
    CHECK(outcome.code == ling::err::kInternal);
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
