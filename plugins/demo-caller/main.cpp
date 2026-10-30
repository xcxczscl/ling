// 调用方示例：激活时订阅两类事件、调一次 demo-a.ping，把结果与收到的事件写进自己的日志。
// 命令行没有 call 子命令（管理端口只接受 kernel.* 方法），所以「调通一次」由插件验证。
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#include "ling/plugin_api.h"

#include <nlohmann/json.hpp>

namespace {

const char* kLogName = "demo-caller.log";

struct PluginState {
    const ling_host_api* host = nullptr;
    void* ctx = nullptr;
};

PluginState g_state;

void append_log(const std::string& line) {
    if (g_state.host == nullptr || g_state.ctx == nullptr) {
        return;
    }
    const char* dir = g_state.host->data_dir(g_state.ctx);
    if (dir == nullptr) {
        return;
    }
    std::string path(dir);
    path += "/";
    path += kLogName;

    std::FILE* file = nullptr;
#ifdef _WIN32
    // MSVC 把 fopen 标为不安全，而插件目标同样开着 /WX。
    if (fopen_s(&file, path.c_str(), "ab") != 0) {
        file = nullptr;
    }
#else
    file = std::fopen(path.c_str(), "ab");
#endif
    if (file == nullptr) {
        return;
    }
    std::fwrite(line.data(), 1, line.size(), file);
    std::fputc('\n', file);
    std::fclose(file);
    std::printf("[demo-caller] %s\n", line.c_str());

    // 同时交给 core-log（若它已激活）；调用失败说明它不在，忽略即可。
    nlohmann::json request;
    request["plugin"] = "demo-caller";
    request["level"] = "info";
    request["message"] = line;
    const std::string text = request.dump();
    uint8_t* out = nullptr;
    size_t out_len = 0;
    g_state.host->call(g_state.ctx, "core-log.write",
                       reinterpret_cast<const uint8_t*>(text.data()), text.size(), &out, &out_len);
    if (out != nullptr) {
        g_state.host->free_buffer(g_state.ctx, out, out_len);
    }
}

// 事件回调：主题与载荷一起打印，验证进程内轨道确实把主题传了过来。
int32_t on_event(void* handler_ctx, const char* topic, const uint8_t* payload, size_t len) {
    (void)handler_ctx;
    const std::string topic_text = topic == nullptr ? std::string("(空)") : std::string(topic);
    const std::string body(reinterpret_cast<const char*>(payload), len);
    append_log("event " + topic_text + " " + body);
    return 0;
}

}  // namespace

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

    g_state.host = host;
    g_state.ctx = ctx;

    // 先订阅再调用，否则 demo-a.pinged 在订阅登记之前就发完了。
    uint64_t sub_id = 0;
    if (host->subscribe(ctx, "demo-a.*", reinterpret_cast<void*>(&on_event), nullptr, &sub_id) !=
        0) {
        append_log("subscribe demo-a.* failed");
    }
    if (host->subscribe(ctx, "kernel.plugin.state", reinterpret_cast<void*>(&on_event), nullptr,
                        &sub_id) != 0) {
        append_log("subscribe kernel.plugin.state failed");
    }

    uint8_t* reply = nullptr;
    size_t reply_len = 0;
    const int32_t code = host->call(ctx, "demo-a.ping", nullptr, 0, &reply, &reply_len);
    if (code != 0) {
        append_log("call failed code=" + std::to_string(code));
    } else {
        append_log("reply=" + std::string(reinterpret_cast<const char*>(reply), reply_len));
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
