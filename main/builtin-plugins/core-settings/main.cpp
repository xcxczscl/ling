// core-settings：键值配置的存储与查询。语义见 docs/ling/04-详细设计/07-内置插件.md §4。
//
// 存储：<数据根>/settings.json，顶层 schema_version 与 entries，
// entries 的键是 "<插件编号>.<键名>"，值是 {"v": 任意 JSON, "updated_at": 时间}。
// 键空间按前缀隔离；内核经 write_config 转发时把调用方编号放在 args.plugin 里。
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <string>

#include "ling/plugin_api.h"

#include <nlohmann/json.hpp>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

namespace {

constexpr std::size_t kMaxSettingsBytes = 4 * 1024 * 1024;
constexpr int kSchemaVersion = 1;

struct State {
    const ling_host_api* host = nullptr;
    void* ctx = nullptr;
    std::filesystem::path settings_path;
    nlohmann::json document;
};

State g;

std::string now_text() {
    const std::time_t stamp = std::time(nullptr);
    std::tm local{};
#ifdef _WIN32
    localtime_s(&local, &stamp);
#else
    localtime_r(&stamp, &local);
#endif
    char buffer[32] = {};
    std::strftime(buffer, sizeof(buffer), "%Y-%m-%dT%H:%M:%S", &local);
    return buffer;
}

// <数据根>/plugins/core-settings/data → <数据根>
std::filesystem::path data_root_of(const char* data_dir) {
    return std::filesystem::path(data_dir).parent_path().parent_path().parent_path();
}

bool save_document() {
    const std::string text = g.document.dump(2);
    if (text.size() > kMaxSettingsBytes) {
        return false;
    }
    const auto tmp = g.settings_path;
    auto tmp_path = tmp;
    tmp_path += ".tmp";

    std::ofstream output(tmp_path, std::ios::trunc | std::ios::binary);
    if (!output) {
        return false;
    }
    output.write(text.data(), static_cast<std::streamsize>(text.size()));
    output.flush();
    if (!output) {
        return false;
    }
    output.close();

#ifdef _WIN32
    // rename 在 Windows 上不覆盖已存在的目标，用 MoveFileEx 换取「替换」语义。
    return ::MoveFileExA(tmp_path.string().c_str(), tmp.string().c_str(),
                         MOVEFILE_REPLACE_EXISTING) != 0;
#else
    std::error_code ec;
    std::filesystem::rename(tmp_path, tmp, ec);
    return !ec;
#endif
}

void load_document() {
    g.document = nlohmann::json::object();
    g.document["schema_version"] = kSchemaVersion;
    g.document["entries"] = nlohmann::json::object();

    std::ifstream input(g.settings_path, std::ios::binary);
    if (!input) {
        return;
    }
    auto parsed = nlohmann::json::parse(input, nullptr, false);
    if (parsed.is_discarded() || !parsed.is_object()) {
        return;
    }
    // 高版本配置不猜、不丢字段，直接按空配置起步并保留原文件不动。
    if (parsed.value("schema_version", 0) > kSchemaVersion) {
        return;
    }
    if (parsed.contains("entries") && parsed["entries"].is_object()) {
        g.document["entries"] = parsed["entries"];
    }
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

// 键必须有 "<编号>." 前缀，否则越权（04/07 §4.2）。
bool key_owned_by(const std::string& key, const std::string& plugin_id) {
    return key.size() > plugin_id.size() + 1 && key.compare(0, plugin_id.size(), plugin_id) == 0 &&
           key[plugin_id.size()] == '.';
}

int32_t on_write(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string plugin = parsed.value("plugin", "");
    const std::string key = parsed.value("key", "");
    if (plugin.empty() || key.empty()) {
        return 2;
    }
    if (!key_owned_by(key, plugin)) {
        return 3;  // E_NAMESPACE_VIOLATION
    }
    if (!parsed.contains("value")) {
        return 2;
    }

    nlohmann::json entry;
    entry["v"] = parsed["value"];
    entry["updated_at"] = now_text();
    g.document["entries"][key] = entry;
    if (!save_document()) {
        return 4;  // E_IO
    }
    return reply_json({{"key", key}}, out, out_len);
}

int32_t on_get(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string key = parsed.value("key", "");
    nlohmann::json result;
    result["key"] = key;
    const auto& entries = g.document["entries"];
    if (entries.contains(key)) {
        result["value"] = entries[key].contains("v") ? entries[key]["v"] : nlohmann::json();
        result["found"] = true;
    } else {
        result["value"] = nullptr;
        result["found"] = false;
    }
    return reply_json(result, out, out_len);
}

int32_t on_list(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string prefix = parsed.value("prefix", "");
    nlohmann::json keys = nlohmann::json::array();
    for (const auto& item : g.document["entries"].items()) {
        if (prefix.empty() || item.key().rfind(prefix, 0) == 0) {
            keys.push_back(item.key());
        }
    }
    return reply_json({{"keys", keys}}, out, out_len);
}

int32_t on_export(void*, const uint8_t*, size_t, uint8_t** out, size_t* out_len) {
    return reply_json(g.document, out, out_len);
}

int32_t on_import(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    if (!parsed.contains("entries") || !parsed["entries"].is_object()) {
        return 2;
    }
    std::size_t added = 0;
    for (const auto& item : parsed["entries"].items()) {
        // 只补不覆盖：误导入旧备份不该冲掉当前配置（04/07 §4.4）。
        if (g.document["entries"].contains(item.key())) {
            continue;
        }
        g.document["entries"][item.key()] = item.value();
        ++added;
    }
    if (!save_document()) {
        return 4;
    }
    return reply_json({{"added", added}}, out, out_len);
}

}  // namespace

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc,
                                             void** out_plugin) {
    if (host == nullptr || out_desc == nullptr || out_plugin == nullptr) {
        return 1;
    }

    void* ctx = host->context_alloc("core-settings", 0);
    if (ctx == nullptr) {
        return 2;
    }
    const char* data_dir = host->data_dir(ctx);
    if (data_dir == nullptr) {
        return 3;
    }

    g.host = host;
    g.ctx = ctx;
    g.settings_path = data_root_of(data_dir) / "settings.json";
    load_document();

    // requires_mask 只有 fs：这个插件只碰自己的配置文件。
    const struct {
        const char* name;
        int32_t (*fn)(void*, const uint8_t*, size_t, uint8_t**, size_t*);
    } methods[] = {
        {"core-settings.write", &on_write},   {"core-settings.get", &on_get},
        {"core-settings.list", &on_list},     {"core-settings.export", &on_export},
        {"core-settings.import", &on_import},
    };
    for (const auto& method : methods) {
        if (host->register_method(ctx, method.name, reinterpret_cast<void*>(method.fn), nullptr,
                                  1u) != 0) {
            return 4;
        }
    }

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "core-settings";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
