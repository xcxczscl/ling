// core-log：收集插件日志，写入 <数据根>/logs/<编号>-YYYY-MM-DD.log 并提供查询。
// 语义见 docs/ling/04-详细设计/07-内置插件.md §7。
//
// 行格式与内核日志一致：<ISO 时间> [级别] <正文>，同一套脚本可以解析两边。
#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include "ling/plugin_api.h"

#include <nlohmann/json.hpp>

namespace {

struct State {
    const ling_host_api* host = nullptr;
    void* ctx = nullptr;
    std::filesystem::path log_dir;
};

State g;

std::filesystem::path data_root_of(const char* data_dir) {
    return std::filesystem::path(data_dir).parent_path().parent_path().parent_path();
}

std::string today_text() {
    const std::time_t stamp = std::time(nullptr);
    std::tm local{};
#ifdef _WIN32
    localtime_s(&local, &stamp);
#else
    localtime_r(&stamp, &local);
#endif
    char buffer[16] = {};
    std::strftime(buffer, sizeof(buffer), "%Y-%m-%d", &local);
    return buffer;
}

std::string stamp_text() {
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

// 插件编号只能是字母数字与连字符：日志文件名直接用它拼，挡目录穿越。
bool valid_plugin_id(const std::string& id) {
    if (id.empty() || id.size() > 64) {
        return false;
    }
    for (char ch : id) {
        const auto uch = static_cast<unsigned char>(ch);
        if (std::isalnum(uch) == 0 && ch != '-') {
            return false;
        }
    }
    return true;
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

std::filesystem::path log_path_of(const std::string& plugin, const std::string& date) {
    return g.log_dir / (plugin + "-" + date + ".log");
}

// 写失败不让业务失败：日志是诊断手段，不是流程的一环（04/07 §7.3）。
int32_t on_write(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string plugin = parsed.value("plugin", "");
    const std::string message = parsed.value("message", "");
    const std::string level = parsed.value("level", "info");
    if (!valid_plugin_id(plugin)) {
        return 2;
    }

    std::error_code ec;
    std::filesystem::create_directories(g.log_dir, ec);
    const auto path = log_path_of(plugin, today_text());
    std::ofstream output(path, std::ios::app | std::ios::binary);
    if (!output) {
        return 0;  // 落盘失败也只返回成功，调用方不必处理
    }
    output << stamp_text() << " [" << level << "] " << message << "\n";
    return reply_json({{"written", true}}, out, out_len);
}

std::vector<std::string> read_lines(const std::filesystem::path& path) {
    std::vector<std::string> lines;
    std::ifstream input(path, std::ios::binary);
    std::string line;
    while (std::getline(input, line)) {
        if (!line.empty() && line.back() == '\r') {
            line.pop_back();
        }
        lines.push_back(line);
    }
    return lines;
}

int32_t on_tail(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string plugin = parsed.value("plugin", "");
    if (!valid_plugin_id(plugin)) {
        return 2;
    }
    const std::size_t count = parsed.value("n", 40);

    // 没有日期参数就取该插件最新的那份日志。
    std::filesystem::path latest;
    std::error_code ec;
    for (const auto& entry : std::filesystem::directory_iterator(g.log_dir, ec)) {
        if (!entry.is_regular_file()) {
            continue;
        }
        const std::string name = entry.path().filename().string();
        if (name.rfind(plugin + "-", 0) != 0 || entry.path().extension() != ".log") {
            continue;
        }
        if (latest.empty() || name > latest.filename().string()) {
            latest = entry.path();
        }
    }

    nlohmann::json result;
    result["plugin"] = plugin;
    result["path"] = latest.empty() ? std::string() : latest.string();
    result["lines"] = nlohmann::json::array();
    if (!latest.empty()) {
        auto lines = read_lines(latest);
        const std::size_t begin = lines.size() > count ? lines.size() - count : 0;
        for (std::size_t i = begin; i < lines.size(); ++i) {
            result["lines"].push_back(lines[i]);
        }
    }
    return reply_json(result, out, out_len);
}

int32_t on_query(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string plugin = parsed.value("plugin", "");
    if (!valid_plugin_id(plugin)) {
        return 2;
    }
    const std::string date = parsed.value("date", today_text());
    const std::string level = parsed.value("level", "");

    auto lines = read_lines(log_path_of(plugin, date));
    nlohmann::json result;
    result["plugin"] = plugin;
    result["date"] = date;
    result["lines"] = nlohmann::json::array();
    for (const auto& line : lines) {
        if (!level.empty() && line.find("[" + level + "]") == std::string::npos) {
            continue;
        }
        result["lines"].push_back(line);
    }
    return reply_json(result, out, out_len);
}

}  // namespace

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc,
                                             void** out_plugin) {
    if (host == nullptr || out_desc == nullptr || out_plugin == nullptr) {
        return 1;
    }

    void* ctx = host->context_alloc("core-log", 0);
    if (ctx == nullptr) {
        return 2;
    }
    const char* data_dir = host->data_dir(ctx);
    if (data_dir == nullptr) {
        return 3;
    }

    g.host = host;
    g.ctx = ctx;
    g.log_dir = data_root_of(data_dir) / "logs";
    std::error_code ec;
    std::filesystem::create_directories(g.log_dir, ec);

    const struct {
        const char* name;
        int32_t (*fn)(void*, const uint8_t*, size_t, uint8_t**, size_t*);
    } methods[] = {
        {"core-log.write", &on_write},
        {"core-log.tail", &on_tail},
        {"core-log.query", &on_query},
    };
    for (const auto& method : methods) {
        if (host->register_method(ctx, method.name, reinterpret_cast<void*>(method.fn), nullptr,
                                  1u) != 0) {
            return 4;
        }
    }

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "core-log";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
