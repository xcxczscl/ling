// core-log：收集插件日志，写入 <数据根>/logs/<编号>-YYYY-MM-DD.log 并提供查询。
// 语义见 docs/ling/04-详细设计/07-内置插件.md §7。
//
// 行格式与内核日志一致：<ISO 时间> [级别] <正文>，同一套脚本可以解析两边。
#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstddef>
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
    if (!valid_plugin_id(plugin)) {
        return 2;
    }
    if (!parsed.contains("message") || !parsed["message"].is_string()) {
        return 2;
    }
    const std::string message = parsed["message"].get<std::string>();
    const std::string level = parsed.value("level", "info");

    // 单行封顶 8 KB：一个插件写一行几十 MB 会把查询拖垮（02-设计与接口.md §8）。
    constexpr std::size_t kMaxLineBytes = 8 * 1024;
    std::string body = message;
    bool truncated = false;
    if (body.size() > kMaxLineBytes) {
        body.resize(kMaxLineBytes);
        truncated = true;
    }

    std::error_code ec;
    std::filesystem::create_directories(g.log_dir, ec);
    const auto path = log_path_of(plugin, today_text());
    std::ofstream output(path, std::ios::app | std::ios::binary);
    if (!output) {
        // 环境问题不报给调用方：它改不了，报了只会让业务失败。往 stderr 留一句供排查。
        std::fprintf(stderr, "[core-log] 无法写入 %s\n", path.string().c_str());
        return 0;
    }
    output << stamp_text() << " [" << level << "] " << body;
    if (truncated) {
        output << "（已截断，原长 " << message.size() << " 字节）";
    }
    output << "\n";
    return reply_json({{"written", true}}, out, out_len);
}

// 从文件末尾往前分块读，凑够 count 行就停。内存与行数成正比，与文件大小无关。
std::vector<std::string> tail_lines(const std::filesystem::path& path, std::size_t count) {
    std::vector<std::string> lines;
    if (count == 0) {
        return lines;
    }
    std::ifstream input(path, std::ios::binary | std::ios::ate);
    if (!input) {
        return lines;
    }
    const auto end_position = input.tellg();
    if (end_position <= 0) {
        return lines;
    }

    std::size_t window_size = 64 * 1024;
    std::string window;
    for (;;) {
        const auto start = std::max<std::streamoff>(
            0, end_position - static_cast<std::streamoff>(window_size));
        const auto length = static_cast<std::size_t>(end_position - start);
        window.assign(length, '\0');
        input.seekg(start);
        input.read(window.data(), static_cast<std::streamsize>(length));

        lines.clear();
        std::size_t cursor = 0;
        if (start > 0) {
            // 从中间读起时第一行是半截，跳过它。
            const auto first_newline = window.find('\n');
            cursor = first_newline == std::string::npos ? window.size() : first_newline + 1;
        }
        while (cursor < window.size()) {
            const auto newline = window.find('\n', cursor);
            const auto end = newline == std::string::npos ? window.size() : newline;
            std::string line = window.substr(cursor, end - cursor);
            if (!line.empty() && line.back() == '\r') {
                line.pop_back();
            }
            if (!line.empty()) {
                lines.push_back(std::move(line));
            }
            cursor = end + 1;
        }

        if (lines.size() >= count || start == 0) {
            break;
        }
        window_size *= 4;  // 不够就扩大窗口，最多几次就到文件开头
    }

    if (lines.size() > count) {
        lines.erase(lines.begin(), lines.end() - static_cast<std::ptrdiff_t>(count));
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
        for (const auto& line : tail_lines(latest, count)) {
            result["lines"].push_back(line);
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

    // 逐行读过就丢：内存只与命中的行数有关，与文件大小无关。
    nlohmann::json result;
    result["plugin"] = plugin;
    result["date"] = date;
    result["lines"] = nlohmann::json::array();

    std::ifstream input(log_path_of(plugin, date), std::ios::binary);
    std::string line;
    const std::string tag = "[" + level + "]";
    while (std::getline(input, line)) {
        if (!line.empty() && line.back() == '\r') {
            line.pop_back();
        }
        if (!level.empty() && line.find(tag) == std::string::npos) {
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
