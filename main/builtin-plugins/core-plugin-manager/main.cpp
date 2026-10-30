// core-plugin-manager：插件管理器。语义见 docs/ling/04-详细设计/07-内置插件.md §5。
//
// 本版本实现两项：孤儿数据扫描（只剩 data/ 的目录）与目录占用统计。
// 「依赖树」「变更预演」「批量操作」依赖依赖解析与内核写方法，M3 之后补，
// MVP 阶段这两项恰好是命令行没有的，符合「便利功能归插件」的划分。
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <string>
#include <system_error>

#include "ling/plugin_api.h"

#include <nlohmann/json.hpp>

namespace {

struct State {
    const ling_host_api* host = nullptr;
    void* ctx = nullptr;
    std::filesystem::path plugins_dir;
};

State g;

std::filesystem::path data_root_of(const char* data_dir) {
    return std::filesystem::path(data_dir).parent_path().parent_path().parent_path();
}

int32_t reply_json(const nlohmann::json& value, uint8_t** out, size_t* out_len) {
    const std::string text = value.dump();
    auto* buffer = static_cast<uint8_t*>(std::malloc(text.size() + 1));
    if (buffer == nullptr) {
        return 1;
    }
    std::memcpy(buffer, text.data(), text.size());
    buffer[text.size()] = 0;
    *out = buffer;
    *out_len = text.size();
    return 0;
}

nlohmann::json parse_args(const uint8_t* args, size_t len) {
    if (args == nullptr || len == 0) {
        return nlohmann::json::object();
    }
    auto parsed = nlohmann::json::parse(reinterpret_cast<const char*>(args),
                                        reinterpret_cast<const char*>(args) + len, nullptr, false);
    if (parsed.is_discarded() || !parsed.is_object()) {
        return nlohmann::json::object();
    }
    return parsed;
}

std::uintmax_t directory_size(const std::filesystem::path& dir) {
    std::uintmax_t total = 0;
    std::error_code ec;
    for (const auto& entry :
         std::filesystem::recursive_directory_iterator(dir, std::filesystem::directory_options::
                                                                skip_permission_denied,
                                                       ec)) {
        if (entry.is_regular_file(ec)) {
            total += entry.file_size(ec);
        }
    }
    return total;
}

// 孤儿 = 目录里只剩 data/，没有 plugin.toml。这种目录算「未安装」，可重装沿用数据。
int32_t on_orphans(void*, const uint8_t*, size_t, uint8_t** out, size_t* out_len) {
    nlohmann::json items = nlohmann::json::array();
    std::uintmax_t total = 0;
    std::error_code ec;
    for (const auto& entry : std::filesystem::directory_iterator(g.plugins_dir, ec)) {
        if (!entry.is_directory()) {
            continue;
        }
        if (std::filesystem::exists(entry.path() / "plugin.toml", ec)) {
            continue;
        }
        const auto bytes = directory_size(entry.path());
        total += bytes;
        items.push_back({{"id", entry.path().filename().string()}, {"bytes", bytes}});
    }
    nlohmann::json result;
    result["orphans"] = items;
    result["total_bytes"] = total;
    return reply_json(result, out, out_len);
}

int32_t on_sizes(void*, const uint8_t*, size_t, uint8_t** out, size_t* out_len) {
    nlohmann::json items = nlohmann::json::array();
    std::error_code ec;
    for (const auto& entry : std::filesystem::directory_iterator(g.plugins_dir, ec)) {
        if (!entry.is_directory()) {
            continue;
        }
        items.push_back({{"id", entry.path().filename().string()},
                         {"bytes", directory_size(entry.path())}});
    }
    nlohmann::json result;
    result["plugins"] = items;
    return reply_json(result, out, out_len);
}

}  // namespace

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc,
                                             void** out_plugin) {
    if (host == nullptr || out_desc == nullptr || out_plugin == nullptr) {
        return 1;
    }

    void* ctx = host->context_alloc("core-plugin-manager", 0);
    if (ctx == nullptr) {
        return 2;
    }
    const char* data_dir = host->data_dir(ctx);
    if (data_dir == nullptr) {
        return 3;
    }

    g.host = host;
    g.ctx = ctx;
    g.plugins_dir = data_root_of(data_dir) / "plugins";

    const struct {
        const char* name;
        int32_t (*fn)(void*, const uint8_t*, size_t, uint8_t**, size_t*);
    } methods[] = {
        {"core-plugin-manager.orphans", &on_orphans},
        {"core-plugin-manager.sizes", &on_sizes},
    };
    for (const auto& method : methods) {
        if (host->register_method(ctx, method.name, reinterpret_cast<void*>(method.fn), nullptr,
                                  1u) != 0) {
            return 4;
        }
    }

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "core-plugin-manager";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
