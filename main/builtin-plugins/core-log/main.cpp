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
#include <deque>
#include <filesystem>
#include <fstream>
#include <string>
#include <utility>
#include <vector>

#include "ling/plugin_api.h"
#include "ling/sys.hpp"

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
    const std::tm local = ling::sys::local_time(&stamp);
    char buffer[16] = {};
    std::strftime(buffer, sizeof(buffer), "%Y-%m-%d", &local);
    return buffer;
}

std::string stamp_text() {
    const std::time_t stamp = std::time(nullptr);
    const std::tm local = ling::sys::local_time(&stamp);
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

// 级别是枚举，理由同 §8：查询按级别过滤，随手传的字符串只会让过滤失效。
bool valid_level(const std::string& level) {
    return level == "debug" || level == "info" || level == "warn" || level == "error";
}

// 日期只接受 YYYY-MM-DD：它要拿去拼文件名，放任输入就等于开了目录穿越的口子。
bool valid_date(const std::string& text) {
    if (text.size() != 10 || text[4] != '-' || text[7] != '-') {
        return false;
    }
    for (std::size_t i = 0; i < text.size(); ++i) {
        if (i == 4 || i == 7) {
            continue;
        }
        if (std::isdigit(static_cast<unsigned char>(text[i])) == 0) {
            return false;
        }
    }
    const int month = std::atoi(text.substr(5, 2).c_str());
    const int day = std::atoi(text.substr(8, 2).c_str());
    return month >= 1 && month <= 12 && day >= 1 && day <= 31;
}

// 按行首的「<时间> [级别] 」结构取级别，而不是在整行里搜 "[info]"：
// 否则正文里带一个 [info] 就会被误当成该级别的日志。
bool line_has_level(const std::string& line, const std::string& level) {
    // 行格式是「<日期 时间> [级别] 正文」，时间戳本身含空格，不能拿第一个空格定位。
    // 级别结束的「] 」一定在正文之前，取第一个「] 」，再往前找最近的「[」。
    const auto close = line.find("] ");
    if (close == std::string::npos) {
        return false;
    }
    const auto open = line.rfind('[', close);
    if (open == std::string::npos) {
        return false;
    }
    return line.compare(open + 1, close - open - 1, level) == 0;
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

// plugin 字段必须是字符串：json::value 在类型不符时会抛 type_error，
// 那会被路由层当成 E_INTERNAL，变成一条看不懂的内部错误。
std::string plugin_arg(const nlohmann::json& parsed) {
    if (!parsed.contains("plugin") || !parsed["plugin"].is_string()) {
        return {};
    }
    return parsed["plugin"].get<std::string>();
}

// 每页行数，夹在 1..1000：负数会转成一个巨大的无符号数。
bool read_count(const nlohmann::json& parsed, int fallback, std::size_t* out) {
    *out = static_cast<std::size_t>(fallback);
    if (!parsed.contains("n")) {
        return true;
    }
    if (!parsed["n"].is_number()) {
        return false;
    }
    const double raw = parsed["n"].get<double>();
    if (raw >= 1000) {
        *out = 1000;
    } else if (raw >= 1) {
        *out = static_cast<std::size_t>(raw);
    } else {
        *out = 1;
    }
    return true;
}

// 上翻偏移：要跳过最新的 before 行。上限 10000，与内核的夹取范围一致。
bool read_before(const nlohmann::json& parsed, std::size_t* out) {
    *out = 0;
    if (!parsed.contains("before")) {
        return true;
    }
    if (!parsed["before"].is_number()) {
        return false;
    }
    const double raw = parsed["before"].get<double>();
    if (raw <= 0) {
        *out = 0;
    } else if (raw >= 10000) {
        *out = 10000;
    } else {
        *out = static_cast<std::size_t>(raw);
    }
    return true;
}

// 窗口是日志末尾的 count+before 行。去掉最新的 before 行，再取紧挨着的 count 行。
// 分页在插件里做完，返回的永远是一页，不会把整份日志塞进一次调用。
void append_page(const std::vector<std::string>& window, std::size_t count,
                 std::size_t before, nlohmann::json* lines) {
    const std::size_t end = window.size() > before ? window.size() - before : 0;
    const std::size_t start = end > count ? end - count : 0;
    for (std::size_t i = start; i < end; ++i) {
        lines->push_back(window[i]);
    }
}

// 写失败不让业务失败：日志是诊断手段，不是流程的一环（04/07 §7.3）。
int32_t on_write(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string plugin = plugin_arg(parsed);
    if (!valid_plugin_id(plugin)) {
        return 2;
    }
    if (!parsed.contains("message") || !parsed["message"].is_string()) {
        return 2;
    }
    const std::string message = parsed["message"].get<std::string>();
    std::string level = "info";
    if (parsed.contains("level")) {
        if (!parsed["level"].is_string() ||
            !valid_level(parsed["level"].get<std::string>())) {
            return 2;
        }
        level = parsed["level"].get<std::string>();
    }

    // 单行封顶 8 KB：一个插件写一行几十 MB 会把查询拖垮（02-设计与接口.md §8）。
    constexpr std::size_t kMaxLineBytes = 8 * 1024;
    std::string body = message;
    // 正文里的换行会把一行日志拆成两行，后面那一行可以伪造级别和时间，必须压平。
    for (auto& ch : body) {
        if (ch == '\r' || ch == '\n') {
            ch = ' ';
        }
    }
    bool truncated = false;
    if (body.size() > kMaxLineBytes) {
        body.resize(kMaxLineBytes);
        truncated = true;
    }

    std::error_code ec;
    std::filesystem::create_directories(g.log_dir, ec);
    if (ec) {
        std::fprintf(stderr, "[core-log] 建日志目录失败：%s\n", g.log_dir.string().c_str());
        return reply_json({{"written", false}}, out, out_len);
    }
    const auto path = log_path_of(plugin, today_text());
    std::ofstream output(path, std::ios::app | std::ios::binary);
    if (!output) {
        // 环境问题不报给调用方：它改不了，报了只会让业务失败。往 stderr 留一句供排查。
        std::fprintf(stderr, "[core-log] 无法写入 %s\n", path.string().c_str());
        return reply_json({{"written", false}}, out, out_len);
    }
    output << stamp_text() << " [" << level << "] " << body;
    if (truncated) {
        output << "（已截断，原长 " << message.size() << " 字节）";
    }
    output << "\n";
    output.close();
    if (!output) {
        std::fprintf(stderr, "[core-log] 写日志失败：%s\n", path.string().c_str());
        return reply_json({{"written", false}}, out, out_len);
    }
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
    const std::string plugin = plugin_arg(parsed);
    if (!valid_plugin_id(plugin)) {
        return 2;
    }
    // 行数、上翻偏移都是给人看的分页参数，越界一律夹取。
    std::size_t count = 0;
    std::size_t before = 0;
    if (!read_count(parsed, 40, &count) || !read_before(parsed, &before)) {
        return 2;
    }

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
        // 先读「本页 + 已翻过的行」，再在插件里切出这一页。
        const std::vector<std::string> window = tail_lines(latest, count + before);
        append_page(window, count, before, &result["lines"]);
    }
    return reply_json(result, out, out_len);
}

int32_t on_query(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string plugin = plugin_arg(parsed);
    if (!valid_plugin_id(plugin)) {
        return 2;
    }
    std::string date = today_text();
    if (parsed.contains("date")) {
        if (!parsed["date"].is_string() || !valid_date(parsed["date"].get<std::string>())) {
            return 2;
        }
        date = parsed["date"].get<std::string>();
    }
    std::string level;
    if (parsed.contains("level")) {
        if (!parsed["level"].is_string()) {
            return 2;
        }
        level = parsed["level"].get<std::string>();
        if (!level.empty() && !valid_level(level)) {
            return 2;
        }
    }

    // 逐行读过就丢：内存只与命中行数有关，与文件大小无关。
    std::size_t count = 0;
    std::size_t before = 0;
    if (!read_count(parsed, 200, &count) || !read_before(parsed, &before)) {
        return 2;
    }

    nlohmann::json result;
    result["plugin"] = plugin;
    result["date"] = date;
    result["found"] = false;
    result["lines"] = nlohmann::json::array();

    std::ifstream input(log_path_of(plugin, date), std::ios::binary);
    if (!input) {
        return reply_json(result, out, out_len);
    }
    result["found"] = true;

    // 只保留末尾 count+before 行；更早的行已经翻不到，没必要留在内存里。
    std::deque<std::string> window;
    const std::size_t limit = count + before;
    std::string line;
    while (std::getline(input, line)) {
        if (!line.empty() && line.back() == '\r') {
            line.pop_back();
        }
        if (!level.empty() && !line_has_level(line, level)) {
            continue;
        }
        window.push_back(std::move(line));
        if (window.size() > limit) {
            window.pop_front();
        }
    }
    const std::vector<std::string> page(window.begin(), window.end());
    append_page(page, count, before, &result["lines"]);
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
    if (ec) {
        // 目录建不出来时照样装载：写日志会返回 written:false，别把整个插件拖下水。
        std::fprintf(stderr, "[core-log] 建日志目录失败：%s\n", g.log_dir.string().c_str());
    }

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
