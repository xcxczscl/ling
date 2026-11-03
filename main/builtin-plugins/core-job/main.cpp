// core-job：按五段 cron 触发对插件方法的调用。
// 设计见 docs/ling/builtin-plugins/core-job/02-设计与接口.md。
//
// 不起自己的线程：宿主是单线程内核，插件线程调 call 会与内核写表并发。
// 改为订阅内核每 200 毫秒广播一次的 kernel.tick，在回调里检查并执行。
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <map>
#include <set>
#include <sstream>
#include <string>
#include <vector>

#include "ling/plugin_api.h"
#include "ling/sys.hpp"

#include <nlohmann/json.hpp>

namespace {

// ---------------- cron ----------------

struct CronField {
    std::set<int> values;  // 空集合表示「任意」
    bool any() const { return values.empty(); }
    bool matches(int value) const { return values.empty() || values.count(value) != 0; }
};

struct Cron {
    CronField minute;
    CronField hour;
    CronField day;
    CronField month;
    CronField weekday;
};

// 只接受十进制数字，累加到 limit 之上就判失败。用有符号 64 位，避免 int 溢出后绕回合法区间。
bool parse_number(const std::string& text, std::int64_t limit, std::int64_t* out) {
    if (text.empty()) {
        return false;
    }
    std::int64_t value = 0;
    for (char ch : text) {
        if (std::isdigit(static_cast<unsigned char>(ch)) == 0) {
            return false;
        }
        value = value * 10 + (ch - '0');
        if (value > limit) {
            return false;
        }
    }
    *out = value;
    return true;
}

// 数值段故意给一个比所有上限都大的上限：先防溢出，再统一报「超出范围」。
constexpr std::int64_t kNumberLimit = 1000000;
// 步长上限 60：cron 的每一段都不超过 60 个取值，再大的步长没有意义。
constexpr std::int64_t kMaxStep = 60;

bool parse_field(const std::string& text, int low, int high, const char* name, CronField* out,
                 std::string* error) {
    if (text == "*") {
        return true;
    }
    std::size_t start = 0;
    while (start <= text.size()) {
        const auto comma = text.find(',', start);
        const std::string piece =
            text.substr(start, comma == std::string::npos ? std::string::npos : comma - start);
        if (piece.empty()) {
            *error = std::string("cron 的「") + name + "」段有空项";
            return false;
        }

        std::int64_t step = 1;
        std::string range = piece;
        const auto slash = piece.find('/');
        if (slash != std::string::npos) {
            range = piece.substr(0, slash);
            const std::string step_text = piece.substr(slash + 1);
            if (!parse_number(step_text, kMaxStep, &step) || step <= 0) {
                *error = std::string("cron 的「") + name + "」段步长必须是 1 至 60 的整数：" +
                         step_text;
                return false;
            }
        }

        std::int64_t first = low;
        std::int64_t last = high;
        if (range != "*") {
            const auto dash = range.find('-');
            if (dash != std::string::npos) {
                if (!parse_number(range.substr(0, dash), kNumberLimit, &first) ||
                    !parse_number(range.substr(dash + 1), kNumberLimit, &last)) {
                    *error = std::string("cron 的「") + name + "」段不是数字：" + range;
                    return false;
                }
            } else {
                if (!parse_number(range, kNumberLimit, &first)) {
                    *error = std::string("cron 的「") + name + "」段不是数字：" + range;
                    return false;
                }
                last = first;
            }
        }
        if (first < low || last > high || first > last) {
            *error = std::string("cron 的「") + name + "」段取值 " + range + " 超出范围 " +
                     std::to_string(low) + " 至 " + std::to_string(high);
            return false;
        }
        for (std::int64_t value = first; value <= last;) {
            out->values.insert(static_cast<int>(value));
            // 先判断下一次会不会越过 last，再累加：这样 step 再大也不会溢出。
            if (value > last - step) {
                break;
            }
            value += step;
        }

        if (comma == std::string::npos) {
            break;
        }
        start = comma + 1;
    }
    return true;
}

bool parse_cron(const std::string& text, Cron* out, std::string* error) {
    std::vector<std::string> parts;
    std::size_t start = 0;
    while (start <= text.size()) {
        const auto space = text.find(' ', start);
        const std::string piece =
            text.substr(start, space == std::string::npos ? std::string::npos : space - start);
        if (!piece.empty()) {
            parts.push_back(piece);
        }
        if (space == std::string::npos) {
            break;
        }
        start = space + 1;
    }
    if (parts.size() != 5) {
        *error = "cron 要五段（分 时 日 月 周），现在是 " + std::to_string(parts.size()) + " 段";
        return false;
    }

    *out = Cron{};
    return parse_field(parts[0], 0, 59, "分", &out->minute, error) &&
           parse_field(parts[1], 0, 23, "时", &out->hour, error) &&
           parse_field(parts[2], 1, 31, "日", &out->day, error) &&
           parse_field(parts[3], 1, 12, "月", &out->month, error) &&
           parse_field(parts[4], 0, 6, "周", &out->weekday, error);
}

// 「日」与「周」都限定时按「或」处理，与常见 cron 一致。
bool cron_matches(const Cron& cron, const std::tm& now) {
    if (!cron.minute.matches(now.tm_min) || !cron.hour.matches(now.tm_hour) ||
        !cron.month.matches(now.tm_mon + 1)) {
        return false;
    }
    const bool day_any = cron.day.any();
    const bool weekday_any = cron.weekday.any();
    if (day_any && weekday_any) {
        return true;
    }
    if (day_any) {
        return cron.weekday.matches(now.tm_wday);
    }
    if (weekday_any) {
        return cron.day.matches(now.tm_mday);
    }
    return cron.day.matches(now.tm_mday) || cron.weekday.matches(now.tm_wday);
}

// ---------------- 时间 ----------------

std::tm parse_now(const std::string& text) {
    // 用 std::get_time 解析，跨平台一致，不用 MSVC 的 sscanf_s 与 POSIX 的 sscanf 分叉。
    // get_time 已把 tm_year 存成「年 - 1900」、tm_mon 存成 0-based，直接返回即可。
    std::tm result{};
    std::istringstream input(text);
    input >> std::get_time(&result, "%Y-%m-%dT%H:%M:%S");
    return result;
}

std::string minute_key(const std::tm& now) {
    char buffer[32] = {};
    std::snprintf(buffer, sizeof(buffer), "%04d-%02d-%02dT%02d:%02d", now.tm_year + 1900,
                  now.tm_mon + 1, now.tm_mday, now.tm_hour, now.tm_min);
    return buffer;
}

// 今天 00:00 到「现在之前」有没有一个匹配点。用来判断「停机期间错过」。
bool missed_today(const Cron& cron, const std::tm& now) {
    std::tm probe = now;
    probe.tm_hour = 0;
    probe.tm_min = 0;
    probe.tm_sec = 0;
    const int elapsed = now.tm_hour * 60 + now.tm_min;
    for (int i = 0; i < elapsed; ++i) {
        if (cron_matches(cron, probe)) {
            return true;
        }
        ++probe.tm_min;
        if (probe.tm_min == 60) {
            probe.tm_min = 0;
            ++probe.tm_hour;
        }
    }
    return false;
}

// ---------------- 任务 ----------------

struct Job {
    std::string name;
    std::string cron_text;
    Cron cron;
    std::string method;
    std::string args_json;  // 原始 JSON 文本，调用时原样传出
    bool catch_up = false;
    bool enabled = true;
    bool running = false;
    std::string last_fired;
};

struct State {
    const ling_host_api* host = nullptr;
    void* ctx = nullptr;
    std::map<std::string, Job> jobs;
    std::string last_minute;
    bool loaded = false;
    int load_attempts = 0;
    bool catch_up_pending = true;
    // 触发记录落在插件自己的 data 目录，重启后 catch_up 不会重复补跑当天任务。
    std::filesystem::path state_path;
};

State g;

constexpr const char* kPrefix = "core-job.";

bool valid_name(const std::string& name) {
    if (name.empty() || name.size() > 64) {
        return false;
    }
    for (char ch : name) {
        const auto uch = static_cast<unsigned char>(ch);
        if (std::isalnum(uch) == 0 && ch != '-' && ch != '_') {
            return false;
        }
    }
    return true;
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

nlohmann::json call_json(const char* method, const nlohmann::json& args) {
    const std::string text = args.dump();
    uint8_t* out = nullptr;
    size_t out_len = 0;
    const int32_t code = g.host->call(g.ctx, method,
                                      reinterpret_cast<const uint8_t*>(text.data()), text.size(),
                                      &out, &out_len);
    nlohmann::json result;
    if (code == 0 && out != nullptr) {
        result = nlohmann::json::parse(std::string(reinterpret_cast<const char*>(out), out_len),
                                       nullptr, false);
    }
    if (out != nullptr) {
        g.host->free_buffer(g.ctx, out, out_len);
    }
    return result;
}

// 调一次目标方法。失败只记 stderr，不向上抛，也不中断其它任务。
void invoke(const Job& job) {
    const bool has_args = !job.args_json.empty() && job.args_json != "null";
    const auto* bytes = has_args ? reinterpret_cast<const uint8_t*>(job.args_json.data()) : nullptr;
    const std::size_t len = has_args ? job.args_json.size() : 0;

    uint8_t* out = nullptr;
    size_t out_len = 0;
    const int32_t code = g.host->call(g.ctx, job.method.c_str(), bytes, len, &out, &out_len);
    if (out != nullptr) {
        g.host->free_buffer(g.ctx, out, out_len);
    }
    if (code != 0) {
        std::fprintf(stderr, "[core-job] 任务 %s 调用 %s 失败，错误码 %d\n", job.name.c_str(),
                     job.method.c_str(), static_cast<int>(code));
    }
}

// ---------------- 存储 ----------------

// 写任务定义。走 Context::write_config 而不是直接调 core-settings.write：
// 前者由内核填归属插件，后者要调用方自报，等于自己给自己发身份证。
bool put_job(const Job& job) {
    nlohmann::json value;
    value["cron"] = job.cron_text;
    value["method"] = job.method;
    value["args"] = job.args_json.empty()
                        ? nlohmann::json(nullptr)
                        : nlohmann::json::parse(job.args_json, nullptr, false);
    if (value["args"].is_discarded()) {
        value["args"] = nlohmann::json(nullptr);
    }
    value["catch_up"] = job.catch_up;
    value["enabled"] = job.enabled;

    const std::string text = value.dump();
    const std::string key = std::string(kPrefix) + job.name;
    return g.host->write_config(g.ctx, key.c_str(), reinterpret_cast<const uint8_t*>(text.data()),
                                text.size()) == 0;
}

bool delete_job(const std::string& name) {
    const std::string key = std::string(kPrefix) + name;
    const char* null_text = "null";  // 配置值写 null 等于删键
    return g.host->write_config(g.ctx, key.c_str(),
                                reinterpret_cast<const uint8_t*>(null_text), 4) == 0;
}

// 把每个任务的 last_fired 落到 <data>/state.json。写失败只记日志：
// 丢了记录最多导致重启后多补跑一次，不该让心跳失败。
void save_fired() {
    if (g.state_path.empty()) {
        // 没有数据目录就没法落盘，补跑记录会随重启丢掉；至少留一行线索。
        std::fprintf(stderr, "[core-job] 没有数据目录，触发记录不会落盘\n");
        return;
    }
    nlohmann::json state = nlohmann::json::object();
    state["jobs"] = nlohmann::json::object();
    for (const auto& item : g.jobs) {
        if (!item.second.last_fired.empty()) {
            state["jobs"][item.first] = item.second.last_fired;
        }
    }
    std::error_code ec;
    std::filesystem::create_directories(g.state_path.parent_path(), ec);
    if (ec) {
        std::fprintf(stderr, "[core-job] 建状态目录失败：%s\n",
                     g.state_path.parent_path().string().c_str());
        return;
    }
    auto tmp_path = g.state_path;
    tmp_path += ".tmp";
    std::ofstream output(tmp_path, std::ios::trunc | std::ios::binary);
    if (!output) {
        std::fprintf(stderr, "[core-job] 状态文件写不了：%s\n", tmp_path.string().c_str());
        return;
    }
    output << state.dump();
    output.close();
    if (!output) {
        std::fprintf(stderr, "[core-job] 状态文件写入失败：%s\n", tmp_path.string().c_str());
        std::filesystem::remove(tmp_path, ec);
        return;
    }
    if (!ling::sys::atomic_replace(tmp_path, g.state_path)) {
        std::fprintf(stderr, "[core-job] 状态文件替换失败：%s\n", g.state_path.string().c_str());
        std::filesystem::remove(tmp_path, ec);
    }
}

void load_fired() {
    if (g.state_path.empty()) {
        return;
    }
    std::ifstream input(g.state_path, std::ios::binary);
    if (!input) {
        return;
    }
    const auto parsed = nlohmann::json::parse(input, nullptr, false);
    if (parsed.is_discarded() || !parsed.is_object() || !parsed.contains("jobs") ||
        !parsed["jobs"].is_object()) {
        // 坏了就当作没记录：下次启动可能补跑一次。记一笔，别让用户以为记录丢了没原因。
        std::fprintf(stderr, "[core-job] 状态文件格式不对，忽略触发记录：%s\n",
                     g.state_path.string().c_str());
        return;
    }
    const auto& fired = parsed["jobs"];
    for (auto& item : g.jobs) {
        if (fired.contains(item.first) && fired[item.first].is_string()) {
            item.second.last_fired = fired[item.first].get<std::string>();
        }
    }
}

// 返回 false 表示 core-settings 还没就绪，调用方下次心跳再试。
bool load_jobs() {
    g.jobs.clear();
    const auto listed = call_json("core-settings.list", {{"prefix", kPrefix}});
    if (listed.is_discarded() || !listed.contains("keys")) {
        return false;
    }
    for (const auto& key : listed["keys"]) {
        const std::string full_key = key.get<std::string>();
        const auto got = call_json("core-settings.get", {{"key", full_key}});
        if (got.is_discarded() || !got.value("found", false) || !got.contains("value")) {
            continue;
        }
        const auto& value = got["value"];
        if (!value.is_object()) {
            continue;
        }

        Job job;
        job.name = full_key.substr(std::strlen(kPrefix));
        job.cron_text = value.value("cron", "");
        job.method = value.value("method", "");
        job.catch_up = value.value("catch_up", false);
        job.enabled = value.value("enabled", true);
        if (value.contains("args") && !value["args"].is_null()) {
            job.args_json = value["args"].dump();
        }

        std::string error;
        if (!valid_name(job.name) || job.cron_text.empty() || job.method.empty() ||
            !parse_cron(job.cron_text, &job.cron, &error)) {
            std::fprintf(stderr, "[core-job] 跳过任务 %s：%s\n", full_key.c_str(),
                         error.empty() ? "定义不完整" : error.c_str());
            continue;
        }
        g.jobs[job.name] = job;
    }
    load_fired();
    return true;
}

// ---------------- 方法 ----------------

int32_t on_schedule(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string name = parsed.value("name", "");
    const std::string cron_text = parsed.value("cron", "");
    const std::string method = parsed.value("method", "");
    if (!valid_name(name)) {
        return 2;
    }
    if (method.find('.') == std::string::npos) {
        return 2;
    }

    Cron cron;
    std::string error;
    if (!parse_cron(cron_text, &cron, &error)) {
        std::fprintf(stderr, "[core-job] %s\n", error.c_str());
        return 2;
    }
    if (g.jobs.count(name) != 0) {
        return 3;  // 同名任务已存在
    }

    Job job;
    job.name = name;
    job.cron_text = cron_text;
    job.cron = cron;
    job.method = method;
    job.catch_up = parsed.value("catch_up", false);
    job.enabled = parsed.value("enabled", true);
    if (parsed.contains("args") && !parsed["args"].is_null()) {
        job.args_json = parsed["args"].dump();
    }

    if (!put_job(job)) {
        return 4;  // 写配置失败，多半是 core-settings 不在
    }
    g.jobs[name] = job;
    return reply_json({{"name", name}, {"key", std::string(kPrefix) + name}}, out, out_len);
}

// 只校验时间表，不落盘。存在的理由：插件的错误码只有数字，加载失败时调用方
// 拿不到「哪一段超范围」这种细节；把校验单独做成一个返回正文的方法，命令行与界面
// 就能在用户填表时当场给出原因，而不是等保存失败后去翻日志。
int32_t on_validate(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string cron_text = parsed.value("cron", "");
    Cron cron;
    std::string error;
    if (!parse_cron(cron_text, &cron, &error)) {
        return reply_json({{"ok", false}, {"reason", error}}, out, out_len);
    }
    return reply_json({{"ok", true}, {"cron", cron_text}}, out, out_len);
}

// 启用与停用：只改 enabled 一个字段并落盘，不动时间表与参数。
// 拆成两个方法而不是一个带布尔参数，是因为方法名本身就是文档
// （docs/ling/builtin-plugins/core-ui/04-详细设计/01 §4 的 jobs.setEnabled 一行）。
int32_t set_enabled(const uint8_t* args, size_t len, bool enabled, uint8_t** out,
                    size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string name = parsed.value("name", "");
    if (!valid_name(name)) {
        return 2;
    }
    const auto it = g.jobs.find(name);
    if (it == g.jobs.end()) {
        return 5;  // 没有这个任务
    }
    Job job = it->second;
    job.enabled = enabled;
    if (!put_job(job)) {
        return 4;
    }
    it->second = job;
    return reply_json({{"name", name}, {"enabled", enabled}}, out, out_len);
}

int32_t on_enable(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    return set_enabled(args, len, true, out, out_len);
}

int32_t on_disable(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    return set_enabled(args, len, false, out, out_len);
}

int32_t on_cancel(void*, const uint8_t* args, size_t len, uint8_t** out, size_t* out_len) {
    const auto parsed = parse_args(args, len);
    const std::string name = parsed.value("name", "");
    if (!valid_name(name)) {
        return 2;
    }
    // 取消不存在的任务算成功：重复取消不该报错。
    if (g.jobs.count(name) != 0) {
        if (!delete_job(name)) {
            return 4;
        }
        g.jobs.erase(name);
    }
    return reply_json({{"name", name}}, out, out_len);
}

int32_t on_list(void*, const uint8_t*, size_t, uint8_t** out, size_t* out_len) {
    nlohmann::json jobs = nlohmann::json::array();
    for (const auto& item : g.jobs) {
        const Job& job = item.second;
        jobs.push_back({{"name", job.name},
                        {"cron", job.cron_text},
                        {"method", job.method},
                        {"catch_up", job.catch_up},
                        {"enabled", job.enabled},
                        {"params", job.args_json.empty()
                                       ? nlohmann::json(nullptr)
                                       : nlohmann::json::parse(job.args_json, nullptr, false)},
                        {"last_fired", job.last_fired}});
    }
    return reply_json({{"jobs", jobs}}, out, out_len);
}

// ---------------- 心跳 ----------------

int32_t on_tick(void*, const char*, const uint8_t* payload, size_t len) {
    if (payload == nullptr || len == 0) {
        return 0;
    }
    const auto parsed = nlohmann::json::parse(
        std::string(reinterpret_cast<const char*>(payload), len), nullptr, false);
    if (parsed.is_discarded() || !parsed.contains("now")) {
        return 0;
    }
    // 装载顺序按插件名字母排，core-job 排在 core-settings 之前，激活时读不到任务。
    // 所以推迟到第一次心跳：那时所有内置插件都已激活。
    if (!g.loaded) {
        if (load_jobs()) {
            g.loaded = true;
            g.catch_up_pending = true;
        } else if (++g.load_attempts > 50) {
            g.loaded = true;  // 10 秒还读不到就放弃，免得每 200 毫秒刷一行日志
            std::fprintf(stderr, "[core-job] 读不到任务定义（core-settings 可能未启用）\n");
        }
        return 0;
    }

    const std::tm now = parse_now(parsed["now"].get<std::string>());
    const std::string minute = minute_key(now);

    // 200 毫秒一次心跳，一分钟会被叫 300 次；同一分钟只处理一次。
    if (minute == g.last_minute) {
        return 0;
    }
    g.last_minute = minute;

    // 补跑检查放在第一次心跳做：那时所有插件都已装载完，目标方法才调得到。
    bool fired = false;
    if (g.catch_up_pending) {
        g.catch_up_pending = false;
        const std::string today = minute.substr(0, 10);
        for (auto& item : g.jobs) {
            Job& job = item.second;
            if (!job.enabled || !job.catch_up) {
                continue;
            }
            // 今天已经跑过（记录落在 state.json）就不再补跑：重启后重复执行比漏跑更糟。
            if (!missed_today(job.cron, now) || job.last_fired.substr(0, 10) == today) {
                continue;
            }
            std::fprintf(stderr, "[core-job] 任务 %s 补跑一次（停机期间错过）\n",
                         job.name.c_str());
            job.last_fired = minute;
            fired = true;
            invoke(job);
        }
    }

    for (auto& item : g.jobs) {
        Job& job = item.second;
        if (!job.enabled || !cron_matches(job.cron, now)) {
            continue;
        }
        // 本分钟已经触发过（刚重启就落回同一分钟）：别再来一次。
        if (job.last_fired == minute) {
            continue;
        }
        // 同步模型下心跳是串行的，这个标志不会命中；留着是为了将来引入线程后仍然成立。
        if (job.running) {
            std::fprintf(stderr, "[core-job] 任务 %s 上一次还没结束，跳过本次\n", job.name.c_str());
            continue;
        }
        job.running = true;
        job.last_fired = minute;
        fired = true;
        invoke(job);
        job.running = false;
    }
    if (fired) {
        save_fired();
    }
    return 0;
}

}  // namespace

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc,
                                             void** out_plugin) {
    if (host == nullptr || out_desc == nullptr || out_plugin == nullptr) {
        return 1;
    }

    void* ctx = host->context_alloc("core-job", 0);
    if (ctx == nullptr) {
        return 2;
    }
    g.host = host;
    g.ctx = ctx;
    const char* data_dir = host->data_dir(ctx);
    if (data_dir != nullptr && data_dir[0] != '\0') {
        g.state_path = std::filesystem::path(data_dir) / "state.json";
    }

    // 先订阅心跳，再读任务：读的过程里来一次心跳也无妨，那个分钟会被记下。
    uint64_t sub_id = 0;
    if (host->subscribe(ctx, "kernel.tick", reinterpret_cast<void*>(&on_tick), nullptr,
                        &sub_id) != 0) {
        return 3;
    }

    const struct {
        const char* name;
        int32_t (*fn)(void*, const uint8_t*, size_t, uint8_t**, size_t*);
    } methods[] = {
        {"core-job.schedule", &on_schedule},
        {"core-job.validate", &on_validate},
        {"core-job.cancel", &on_cancel},
        {"core-job.list", &on_list},
        {"core-job.enable", &on_enable},
        {"core-job.disable", &on_disable},
    };
    for (const auto& method : methods) {
        if (host->register_method(ctx, method.name, reinterpret_cast<void*>(method.fn), nullptr,
                                  0) != 0) {
            return 4;
        }
    }

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "core-job";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
