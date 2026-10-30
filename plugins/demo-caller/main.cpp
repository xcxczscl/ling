// 调用方示例：不注册任何方法，激活时调一次 demo-a.ping，把结果打到 stdout。
// 命令行没有 call 子命令（管理端口只接受 kernel.* 方法），所以「调通一次」由插件验证。
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include "ling/plugin_api.h"

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc,
                                             void** out_plugin) {
    if (host == nullptr || out_desc == nullptr || out_plugin == nullptr) {
        return 1;
    }

    void* ctx = host->context_alloc("demo-caller", 0);
    if (ctx == nullptr) {
        return 2;
    }

    uint8_t* reply = nullptr;
    size_t reply_len = 0;
    const int32_t code = host->call(ctx, "demo-a.ping", nullptr, 0, &reply, &reply_len);
    if (code != 0) {
        std::printf("[demo-caller] call failed code=%d\n", static_cast<int>(code));
    } else {
        std::printf("[demo-caller] reply=%.*s\n", static_cast<int>(reply_len),
                    reinterpret_cast<const char*>(reply));
        if (reply != nullptr) {
            host->free_buffer(ctx, reply, reply_len);
        }
    }

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "demo-caller";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
