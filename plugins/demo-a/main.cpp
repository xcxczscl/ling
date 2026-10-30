// 最小示例插件：注册 demo-a.ping，返回 "pong"，并广播 demo-a.pinged。
// 事件用来验证「订阅 → 派发」这条链路，载荷与返回值相同。
#include <cstdlib>
#include <cstring>

#include "ling/plugin_api.h"

namespace {

const char* kReply = "pong";

// 插件侧的运行状态。回调的第一个参数是宿主的上下文句柄而不是插件自己的结构，
// 所以示例插件把 host 与 ctx 放在文件作用域里：示例插件只装载一次，这样够用。
// 真实插件应当在 entry 里分配自己的状态对象，再经由自己的全局指针取用。
struct PluginState {
    const ling_host_api* host = nullptr;
    void* ctx = nullptr;
};

PluginState g_state;

int32_t on_ping(void* handler_ctx, const uint8_t* args, size_t args_len, uint8_t** out,
                size_t* out_len) {
    (void)handler_ctx;
    (void)args;
    (void)args_len;

    if (g_state.host != nullptr && g_state.ctx != nullptr) {
        const auto* payload = reinterpret_cast<const uint8_t*>(kReply);
        g_state.host->emit(g_state.ctx, "demo-a.pinged", payload, std::strlen(kReply));
    }

    const size_t len = std::strlen(kReply);
    auto* buffer = static_cast<uint8_t*>(std::malloc(len));
    if (buffer == nullptr) {
        return 1;
    }
    std::memcpy(buffer, kReply, len);
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

    g_state.host = host;
    g_state.ctx = ctx;

    const int32_t code = host->register_method(ctx, "demo-a.ping",
                                               reinterpret_cast<void*>(&on_ping), nullptr, 0);
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
