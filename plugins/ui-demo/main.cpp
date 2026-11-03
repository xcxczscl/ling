// ui-demo：给 core-ui 的验收用的小插件（07-测试与验收.md §2.1）。
//
// 三个方法各有用处：
//   echo   验证正常链路（面板 → 外壳 → 服务端 → 宿主 → 插件 → 原路返回）
//   sleep  验证超时（默认睡 40 秒，服务端 30 秒、外壳 35 秒都会先到点）
//   boom   验证错误回传（插件返回非零，宿主映射成错误码）
#include <chrono>
#include <cstdint>
#include <cstring>
#include <string>
#include <thread>

#include "ling/plugin_api.h"

#include <nlohmann/json.hpp>

namespace {

nlohmann::json parse(const uint8_t* args, size_t len) {
    if (args == nullptr || len == 0) {
        return nlohmann::json::object();
    }
    const auto parsed = nlohmann::json::parse(
        std::string(reinterpret_cast<const char*>(args), len), nullptr, false);
    return parsed.is_discarded() ? nlohmann::json::object() : parsed;
}

int32_t reply(const nlohmann::json& value, uint8_t** out, size_t* out_len) {
    const std::string text = value.dump();
    auto* buffer = static_cast<uint8_t*>(std::malloc(text.size()));
    if (buffer == nullptr) {
        return 1;
    }
    std::memcpy(buffer, text.data(), text.size());
    *out = buffer;
    *out_len = text.size();
    return 0;
}

int32_t on_echo(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto input = parse(args, len);
    return reply({{"echo", input}, {"pong", true}}, out, out_len);
}

int32_t on_sleep(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto input = parse(args, len);
    int seconds = input.value("seconds", 40);
    if (seconds < 0 || seconds > 120) {
        seconds = 40;
    }
    std::this_thread::sleep_for(std::chrono::seconds(seconds));
    return reply({{"slept", seconds}}, out, out_len);
}

int32_t on_boom(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    (void)args;
    (void)len;
    (void)out;
    (void)out_len;
    return 1;  // 非零即失败，错误码由宿主映射
}

}  // namespace

extern "C" LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                                        ling_plugin_descriptor* out_desc,
                                                        void** out_plugin) {
    (void)host;
    static const struct {
        const char* name;
        void* fn;
    } kMethods[] = {
        {"ui-demo.echo", reinterpret_cast<void*>(&on_echo)},
        {"ui-demo.sleep", reinterpret_cast<void*>(&on_sleep)},
        {"ui-demo.boom", reinterpret_cast<void*>(&on_boom)},
    };
    void* ctx = host->context_alloc("ui-demo", 1);
    if (ctx == nullptr) {
        return 2;
    }
    for (const auto& method : kMethods) {
        if (host->register_method(ctx, method.name, method.fn, nullptr, 0) != 0) {
            return 3;
        }
    }
    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "ui-demo";
    out_desc->plugin_version = "0.1.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
