// 侧车协议帧的单元测试。
// 依据 docs/ling/04-详细设计/02-插件装载与接口.md §8.1 至 §8.3。
#include <catch_amalgamated.hpp>

#include <string>

#include "ling/sidecar_proto.hpp"

using namespace ling::sidecar;

TEST_CASE("侧车帧：正常解析", "[sidecar]") {
    Frame frame;
    std::string error;
    REQUIRE(parse(R"({"v":1,"id":7,"op":"invoke","method":"demo-a.ping"})", &frame, &error));
    CHECK(frame.id == 7);
    CHECK(frame.op == "invoke");
    CHECK(frame.fields["method"] == "demo-a.ping");
}

TEST_CASE("侧车帧：不合规要说明原因", "[sidecar]") {
    Frame frame;
    std::string error;

    CHECK_FALSE(parse("", &frame, &error));
    CHECK(error.find("空帧") != std::string::npos);

    CHECK_FALSE(parse("not json", &frame, &error));
    CHECK(error.find("JSON") != std::string::npos);

    CHECK_FALSE(parse(R"([1,2,3])", &frame, &error));
    CHECK(error.find("对象") != std::string::npos);

    CHECK_FALSE(parse(R"({"op":"hello"})", &frame, &error));
    CHECK(error.find("协议版本") != std::string::npos);

    CHECK_FALSE(parse(R"({"v":2,"op":"hello"})", &frame, &error));
    CHECK(error.find("协议版本") != std::string::npos);

    CHECK_FALSE(parse(R"({"v":1})", &frame, &error));
    CHECK(error.find("op") != std::string::npos);

    // 超过 1 MB 直接拒，不尝试解析（§8.6）
    const std::string huge(kMaxFrameBytes + 1, 'x');
    CHECK_FALSE(parse(huge, &frame, &error));
    CHECK(error.find("1 MB") != std::string::npos);
}

TEST_CASE("侧车帧：没有 id 时按 0 处理", "[sidecar]") {
    Frame frame;
    std::string error;
    REQUIRE(parse(R"({"v":1,"op":"hello"})", &frame, &error));
    CHECK(frame.id == 0);
    CHECK(frame.op == "hello");
}

TEST_CASE("侧车帧：编码再解析要还原", "[sidecar]") {
    Frame frame;
    frame.id = 3;
    frame.op = "call";
    frame.fields = {{"method", "demo-a.ping"}, {"args", {{"x", 1}}}};

    const std::string line = encode(frame);
    REQUIRE(line.back() == '\n');

    Frame back;
    std::string error;
    REQUIRE(parse(line.substr(0, line.size() - 1), &back, &error));
    CHECK(back.id == 3);
    CHECK(back.op == "call");
    CHECK(back.fields["method"] == "demo-a.ping");
    CHECK(back.fields["args"]["x"] == 1);
}

TEST_CASE("侧车帧：成功与失败的应答结构", "[sidecar]") {
    const auto result = encode_result(4, {{"ok", true}});
    Frame parsed;
    std::string error;
    REQUIRE(parse(result.substr(0, result.size() - 1), &parsed, &error));
    CHECK(parsed.op == "result");
    CHECK(parsed.id == 4);
    CHECK(parsed.fields["ok"] == true);

    const auto failure = encode_error(5, "E_TIMEOUT", "宿主没有应答");
    REQUIRE(parse(failure.substr(0, failure.size() - 1), &parsed, &error));
    CHECK(parsed.op == "error");
    CHECK(parsed.fields["ok"] == false);
    CHECK(parsed.fields["error"]["code"] == "E_TIMEOUT");
}

TEST_CASE("侧车帧：op 分类", "[sidecar]") {
    for (const char* op : {"hello", "register_method", "subscribe", "call", "emit", "result",
                           "error", "deactivated"}) {
        CHECK(is_sidecar_op(op));
        CHECK_FALSE(is_host_op(op));
    }
    for (const char* op : {"activate", "invoke", "event", "deactivate", "shutdown"}) {
        CHECK(is_host_op(op));
        CHECK_FALSE(is_sidecar_op(op));
    }
    CHECK_FALSE(is_sidecar_op("nope"));
    CHECK_FALSE(is_host_op(""));
}
