// 依赖演示插件：清单里声明依赖 demo-a@1，用来验证调和（04/04 §5）。
//
// 它自己只注册一个方法 demo-b.hello。单独装它时需求不满足，宿主应当把它留在
// 「已安装、未激活」并说明缺什么；装好 demo-a 之后由调和自动激活，不需要重启。
#include <cstdlib>
#include <cstring>

#include "ling/plugin_api.h"

namespace {

const char* kReply = "hello from demo-b";

int32_t on_hello(void*, const uint8_t*, size_t, uint8_t** out, size_t* out_len) {
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

    void* ctx = host->context_alloc("demo-b", 0);
    if (ctx == nullptr) {
        return 2;
    }

    const int32_t code = host->register_method(ctx, "demo-b.hello",
                                               reinterpret_cast<void*>(&on_hello), nullptr, 0);
    if (code != 0) {
        return 3;
    }

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "demo-b";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
