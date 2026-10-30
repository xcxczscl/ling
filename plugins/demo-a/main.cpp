// 最小示例插件：注册 demo-a.ping，返回 "pong"。
// 写法刻意朴素，用于验证「装载 → 注册 → 调用」这条最小闭环。
#include <cstdlib>
#include <cstring>

#include "ling/plugin_api.h"

namespace {

int32_t on_ping(void* handler_ctx, const uint8_t* args, size_t args_len, uint8_t** out,
                size_t* out_len) {
    (void)handler_ctx;
    (void)args;
    (void)args_len;

    const char* reply = "pong";
    const size_t len = std::strlen(reply);
    auto* buffer = static_cast<uint8_t*>(std::malloc(len));
    if (buffer == nullptr) {
        return 1;
    }
    std::memcpy(buffer, reply, len);
    *out = buffer;
    *out_len = len;
    return 0;
}

}  // namespace

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc,
                                             void** out_plugin) {
    if (host == nullptr || out_desc == nullptr || out_plugin == nullptr) {
        return 1;
    }

    void* ctx = host->context_alloc("demo-a", 0);
    if (ctx == nullptr) {
        return 2;
    }

    const int32_t code = host->register_method(ctx, "demo-a.ping", reinterpret_cast<void*>(&on_ping),
                                               nullptr, 0);
    if (code != 0) {
        return 3;
    }

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "demo-a";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
