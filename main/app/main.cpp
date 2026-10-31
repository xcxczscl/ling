// 命令行入口。serve 之外的所有子命令都经管理端口走 kernel.* 方法（04/06 §4.1），
// 只有 --version、--help 与 logs --kernel 在本地直接完成。
#include <atomic>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include "ling/admin.hpp"
#include "ling/errors.hpp"
#include "ling/kernel.hpp"
#include "ling/version.hpp"

#include <nlohmann/json.hpp>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

namespace {

std::atomic<bool> g_stop_requested{false};

// 终端里汉字占两列，按字节补空格会错位，所以先算显示宽度再补。
std::size_t display_width(const std::string& text) {
    std::size_t width = 0;
    for (const unsigned char ch : text) {
        if ((ch & 0xC0) == 0x80) {
            continue;  // UTF-8 续字节不单独计数
        }
        width += (ch < 0x80) ? 1 : 2;
    }
    return width;
}

std::string pad_right(const std::string& text, std::size_t width) {
    const std::size_t current = display_width(text);
    if (current >= width) {
        return text;
    }
    return text + std::string(width - current, ' ');
}

// 装载表里的英文状态对使用者没意义，输出时翻成中文（--json 保持英文原值）。
std::string state_text(const std::string& state) {
    if (state == "activated") {
        return "已启用";
    }
    if (state == "deactivated") {
        return "已停用";
    }
    if (state == "failed") {
        return "启动失败";
    }
    if (state == "installed") {
        return "已安装";
    }
    return state;
}

std::string track_text(const std::string& track) {
    if (track == "process") {
        return "进程内";
    }
    if (track == "sidecar") {
        return "独立进程";
    }
    return track;
}

#ifdef _WIN32
// 控制台代码页临时切到 UTF-8。
//
// 程序内部一律用 UTF-8：源码带 /utf-8 编译、日志文件写 UTF-8 字节。
// 但 Windows 控制台默认代码页是 936（GBK），把 UTF-8 字节按 GBK 解读就会
// 把「数据根」显示成「鏁版嵁鏍?」。这里在进程启动时切到 65001，退出时还原——
// 不还原会污染同一个控制台里后续运行的其他程序。
class ConsoleUtf8Scope {
public:
    ConsoleUtf8Scope() {
        previous_input_ = ::GetConsoleCP();
        previous_output_ = ::GetConsoleOutputCP();
        if (previous_output_ != CP_UTF8) {
            ::SetConsoleOutputCP(CP_UTF8);
        }
        if (previous_input_ != CP_UTF8) {
            ::SetConsoleCP(CP_UTF8);
        }
    }
    ~ConsoleUtf8Scope() {
        if (previous_output_ != 0 && previous_output_ != CP_UTF8) {
            ::SetConsoleOutputCP(previous_output_);
        }
        if (previous_input_ != 0 && previous_input_ != CP_UTF8) {
            ::SetConsoleCP(previous_input_);
        }
    }

    ConsoleUtf8Scope(const ConsoleUtf8Scope&) = delete;
    ConsoleUtf8Scope& operator=(const ConsoleUtf8Scope&) = delete;

private:
    UINT previous_input_ = 0;
    UINT previous_output_ = 0;
};
#endif

void print_usage(std::FILE* out) {
    std::fprintf(out, "ling %s —— 插件宿主。装插件、用插件，自己不带功能。\n\n",
                 ling::version_string());
    std::fprintf(out, "用法：ling <命令> [参数] [选项]\n\n");
    std::fprintf(out, "命令：\n");
    std::fprintf(out, "  serve                启动 ling，前台运行（保持这个窗口开着）\n");
    std::fprintf(out, "  plugins              列出已安装的插件\n");
    std::fprintf(out, "  install <插件目录>   安装一个插件，装完立即可用\n");
    std::fprintf(out, "  uninstall <插件名>   卸载插件，它的数据默认保留\n");
    std::fprintf(out, "  enable <插件名>      启用插件\n");
    std::fprintf(out, "  disable <插件名>     停用插件，数据保留，随时能再启用\n");
    std::fprintf(out, "  logs [插件名]        查看日志；不给插件名时看 ling 自己的日志\n");
    std::fprintf(out, "  reload <插件名>      重新加载插件（换了新版本之后用）\n");
    std::fprintf(out, "  restart <插件名>     停一下再起来（不换版本，用于清状态）\n");
    std::fprintf(out, "  job list             列出定时任务\n");
    std::fprintf(out, "  job add <任务名>     建一条定时任务，要配 --cron 与 --call\n");
    std::fprintf(out, "  job remove <任务名>  删掉一条定时任务\n");
    std::fprintf(out, "  shutdown             停止正在运行的 ling\n\n");
    std::fprintf(out, "选项：\n");
    std::fprintf(out, "  --purge              卸载时连插件的数据一起删除\n");
    std::fprintf(out, "  --json               输出机器可读的 JSON，方便脚本处理\n");
    std::fprintf(out, "  --version            显示版本\n");
    std::fprintf(out, "  --help               显示本说明\n\n");
    std::fprintf(out, "进阶选项（一般用不到）：\n");
#ifdef _WIN32
    std::fprintf(out, "  --data-dir <目录>    指定数据目录，默认 %%APPDATA%%\\ling\n");
#else
    std::fprintf(out, "  --data-dir <目录>    指定数据目录，默认 ~/.local/share/ling\n");
#endif
    std::fprintf(out, "  --port <端口>        启动时用哪个端口，默认 7900\n\n");
    std::fprintf(out, "第一次用，按这个顺序：\n");
    std::fprintf(out, "  1. 开一个窗口跑        ling serve\n");
    std::fprintf(out, "  2. 另开一个窗口装插件    ling install <插件目录>\n");
    std::fprintf(out, "  3. 看装上了没有        ling plugins\n\n");
    std::fprintf(out, "插件名就是 ling plugins 第一列显示的那个，比如 demo-a。\n");
}

// 读环境变量。MSVC 把 getenv 标为不安全（C4996），而 /WX 会把它当错误，
// 所以在 Windows 上用 _dupenv_s，其它平台仍用 getenv。
std::string read_env(const char* name) {
#ifdef _WIN32
    char* value = nullptr;
    size_t length = 0;
    if (_dupenv_s(&value, &length, name) != 0 || value == nullptr) {
        return {};
    }
    std::string result(value);
    std::free(value);
    return result;
#else
    const char* value = std::getenv(name);
    return value == nullptr ? std::string() : std::string(value);
#endif
}

std::filesystem::path default_data_root() {
#ifdef _WIN32
    const std::string appdata = read_env("APPDATA");
    if (!appdata.empty()) {
        return std::filesystem::path(appdata) / "ling";
    }
    return std::filesystem::path(".") / "data";
#else
    const std::string home = read_env("HOME");
    if (!home.empty()) {
        return std::filesystem::path(home) / ".local" / "share" / "ling";
    }
    return std::filesystem::path(".") / "data";
#endif
}

struct Args {
    std::string command;
    std::vector<std::string> positional;
    std::filesystem::path data_dir;
    bool purge = false;
    bool json = false;
    std::uint16_t port = 7900;

    // ling job add 用到的三项，其余子命令不读。
    std::string cron;
    std::string call_method;
    std::string call_args;
    bool catch_up = false;
};

// 打印内核日志末 40 行。这条路径不依赖任何插件（LING-REQ-077、LING-REQ-119）。
int print_kernel_logs(const std::filesystem::path& data_dir) {
    const auto log_dir = data_dir / "logs";
    std::filesystem::path latest;
    std::error_code ec;
    for (const auto& entry : std::filesystem::directory_iterator(log_dir, ec)) {
        if (!entry.is_regular_file()) {
            continue;
        }
        const std::string name = entry.path().filename().string();
        if (name.rfind("kernel-", 0) != 0) {
            continue;
        }
        // 日志按天分文件，文件名里的日期可字典序比较，取最大的那个即最新。
        if (latest.empty() || name > latest.filename().string()) {
            latest = entry.path();
        }
    }
    if (latest.empty()) {
        std::printf("还没有日志。ling 启动之后才会写第一行。\n");
        return 0;
    }
    std::ifstream input(latest);
    std::vector<std::string> lines;
    std::string line;
    while (std::getline(input, line)) {
        lines.push_back(line);
    }
    const std::size_t begin = lines.size() > 40 ? lines.size() - 40 : 0;
    for (std::size_t i = begin; i < lines.size(); ++i) {
        std::printf("%s\n", lines[i].c_str());
    }
    return 0;
}

void print_plugins(const std::string& result_json) {
    const auto document = nlohmann::json::parse(result_json, nullptr, false);
    if (document.is_discarded() || !document.contains("plugins")) {
        std::printf("%s\n", result_json.c_str());
        return;
    }
    if (document["plugins"].empty()) {
        std::printf("还没有安装任何插件。用 ling install <插件目录> 装一个。\n");
        return;
    }

    // 列宽按显示宽度算，汉字才算得准。
    std::printf("%s%s%s%s\n", pad_right("插件名", 26).c_str(), pad_right("版本", 10).c_str(),
                pad_right("状态", 12).c_str(), "类型");
    for (const auto& plugin : document["plugins"]) {
        std::printf("%s%s%s%s\n", pad_right(plugin.value("id", ""), 26).c_str(),
                    pad_right(plugin.value("version", ""), 10).c_str(),
                    pad_right(state_text(plugin.value("state", "")), 12).c_str(),
                    track_text(plugin.value("track", "")).c_str());
        if (plugin.contains("reason") && !plugin.value("reason", "").empty()) {
            std::printf("  说明：%s\n", plugin.value("reason", "").c_str());
        }
    }
}

void print_log_lines(const std::string& result_json) {
    const auto document = nlohmann::json::parse(result_json, nullptr, false);
    if (document.is_discarded() || !document.contains("lines")) {
        std::printf("%s\n", result_json.c_str());
        return;
    }
    if (document["lines"].empty()) {
        const std::string path = document.value("path", "");
        if (path.empty()) {
            std::printf("这个插件还没有写过日志。\n");
        } else {
            std::printf("日志文件是空的：%s\n", path.c_str());
        }
        return;
    }
    for (const auto& line : document["lines"]) {
        std::printf("%s\n", line.get<std::string>().c_str());
    }
}

// 把错误类别映射成退出码（04/06 §4.2）。
// 错误码是给日志和排查用的，不该直接甩给使用者。这里给每个码配一句人话和一条建议。
struct ErrorHint {
    const char* code;
    const char* plain;
    const char* advice;  // 空字符串表示没有比「看说明」更好的建议
};

const ErrorHint kErrorHints[] = {
    {"E_PLUGIN_MISSING", "找不到这个插件", "用 ling plugins 看看装了哪些插件，第一列就是插件名"},
    {"E_PLUGIN_EXISTS", "这个插件已经装过了", "要重装就先 ling uninstall <插件名>，再装一次"},
    {"E_METHOD_MISSING", "这个插件没有提供要调用的功能", ""},
    {"E_MANIFEST_INVALID", "插件包有问题，装不了", "确认插件目录里有 plugin.toml，且内容完整"},
    {"E_API_MISMATCH", "这个插件与当前版本的 ling 不兼容", "换一个版本的插件，或者升级 ling"},
    {"E_HOST_TOO_OLD", "这个插件要求更高版本的 ling", "升级 ling 之后再装"},
    {"E_CAPABILITY_DENIED", "这个操作超出了插件申请到的权限", ""},
    {"E_NAMESPACE_VIOLATION", "名字前缀不合规", "插件的方法名和事件名都要以自己的插件名开头"},
    {"E_UNINSTALL_INCOMPLETE", "卸载没做完，插件还留在磁盘上", "处理掉下面列出的条目，再试一次"},
    {"E_IO", "读写文件失败", "检查目录权限和磁盘空间"},
    {"E_PAYLOAD_TOO_LARGE", "数据太大，超过了 1 MB 上限", ""},
    {"E_TIMEOUT", "操作超时", "ling 可能卡住了，用 ling logs 看看有没有线索"},
    {"E_PROTOCOL", "和 ling 的通信格式对不上", "命令行与 ling 可能不是同一份安装"},
    {"E_AUTH", "口令不正确", "ling 可能重启过，重试一次即可"},
    {"E_LEDGER_FULL", "插件登记的改动太多，到了上限", "重启 ling 可以清空"},
    {"E_INTERNAL", "ling 内部出错", "把 logs 里对应的那几行贴出来，才好定位"},
    {"E_DEPENDENCY_CYCLE", "插件之间的依赖绕成了环", "卸掉环上任意一个插件，或者改掉它的依赖声明"},
    {"E_UNDO_FAILED", "卸载时有些改动没能退回去", "看 logs 里的失败条目，处理完再卸一次"},
    {"E_CAPABILITY_UNKNOWN", "插件要了一个不认识的能力", "检查插件清单里的 capabilities 写法"},
    {"E_DUPLICATE_METHOD", "这个插件注册了两个同名功能", "改掉其中一个，再重新装"},
    {"E_RELOAD_FAILED", "换个版本没换成，旧版本已经恢复", "确认新版本的插件目录完整，再试一次"},
    {"E_SHUTTING_DOWN", "ling 正在退出", "等它退完再操作"},
};

const ErrorHint* find_hint(const std::string& code) {
    for (const auto& hint : kErrorHints) {
        if (code == hint.code) {
            return &hint;
        }
    }
    return nullptr;
}

std::string trim(const std::string& text) {
    const auto begin = text.find_first_not_of(" \t\r\n");
    if (begin == std::string::npos) {
        return {};
    }
    const auto end = text.find_last_not_of(" \t\r\n");
    return text.substr(begin, end - begin + 1);
}

// 结果里常有「E_XXX 说明」这种拼好的串，去掉码只留说明。
std::string strip_code(const std::string& text, const std::string& code) {
    std::string result = text;
    if (!code.empty()) {
        const auto position = result.find(code);
        if (position != std::string::npos) {
            result.erase(position, code.size());
        }
    }
    return trim(result);
}

void print_failure(const ling::AdminReply& reply) {
    if (reply.kind == ling::AdminReply::Kind::kUnreachable) {
        std::fprintf(stderr, "%s\n", reply.message.c_str());
        std::fprintf(stderr, "提示：先在另一个窗口运行 ling serve 启动 ling。\n");
        return;
    }

    // 屏幕上只说人话，不贴错误码：错误码是给脚本与日志看的，人看到 E_XXX 只会更糊涂。
    const std::string detail = strip_code(reply.message, reply.code);
    const ErrorHint* hint = find_hint(reply.code);
    if (hint != nullptr) {
        if (detail.empty()) {
            std::fprintf(stderr, "%s。\n", hint->plain);
        } else {
            std::fprintf(stderr, "%s：%s\n", hint->plain, detail.c_str());
        }
        if (hint->advice[0] != '\0') {
            std::fprintf(stderr, "提示：%s。\n", hint->advice);
        }
        return;
    }

    std::fprintf(stderr, "%s\n",
                 detail.empty() ? "这一步没成功，原因不明；换 ling --json 看详细信息" : detail.c_str());
}

int exit_code_of(const ling::AdminReply& reply) {
    switch (reply.kind) {
        case ling::AdminReply::Kind::kOk:
            return 0;
        case ling::AdminReply::Kind::kUnreachable:
            return 3;
        case ling::AdminReply::Kind::kAuth:
        case ling::AdminReply::Kind::kRemote:
        case ling::AdminReply::Kind::kProtocol:
            return 4;
    }
    return 4;
}

#ifdef _WIN32
BOOL WINAPI on_console_ctrl(DWORD type) {
    (void)type;
    g_stop_requested.store(true);
    return TRUE;
}
#else
// Linux 上没有 Ctrl+C 回调，用信号。处理函数里只置标志，真正的收尾在主循环里做。
void on_posix_signal(int) { g_stop_requested.store(true); }
#endif

int run_serve(const Args& args, const std::filesystem::path& program_dir) {
#ifdef _WIN32
    ::SetConsoleCtrlHandler(on_console_ctrl, TRUE);
#else
    std::signal(SIGINT, on_posix_signal);
    std::signal(SIGTERM, on_posix_signal);
#endif
    ling::Kernel kernel(args.data_dir);
    // 界面进程还没起，这时 registry.json 里的 ui_port 一定是上一次留下的陈旧值。
    kernel.clear_stale_ui_port();
    // 先预装内置插件，再统一装载（04/07 §8.3）。
    kernel.install_builtins(program_dir);
    const std::size_t failures = kernel.load_all();
    if (failures > 0) {
        std::fprintf(stderr, "注意：有 %zu 个插件没能启动，用 ling plugins 看是哪些。\n", failures);
    }

    ling::AdminServer server(&kernel);
    // 从 --port 起向上找，验收脚本用 8020 起的段也一样适用。
    const std::uint16_t last_port =
        static_cast<std::uint16_t>(args.port + (args.port < 8000 ? 99 : 79));
    const std::string error = server.start(args.port, last_port);
    if (!error.empty()) {
        std::fprintf(stderr, "启动失败：%s\n", error.c_str());
        return 3;
    }

    std::error_code shown_ec;
    const auto shown_root = std::filesystem::absolute(args.data_dir, shown_ec);
    std::printf("ling %s 已启动。\n", ling::version_string());
    std::printf("  数据目录  %s\n", (shown_ec ? args.data_dir : shown_root).string().c_str());
    std::printf("  管理端口  127.0.0.1:%u（本机内部通信，不用管）\n",
                static_cast<unsigned>(server.port()));
    std::printf("保持这个窗口开着。要停止：按 Ctrl+C，或在另一个窗口运行 ling shutdown。\n");
    std::fflush(stdout);

    while (!g_stop_requested.load()) {
        if (!server.poll(200)) {
            break;
        }
        // 心跳挂在同一轮循环里，跟着已有的 200 毫秒节拍走，不另起唤醒。
        kernel.publish_tick();
    }
    server.stop();
    std::printf("ling 已停止。\n");
    return 0;
}

}  // namespace

#ifdef _WIN32
// Windows 把命令行参数按「系统当前代码页」交给程序（中文系统是 936）。
// 程序内部一律 UTF-8，所以入口处转一次：不转的话，带中文的插件目录与
// `--args '{"message":"到点了"}'` 到这里就已经是坏字节了。
std::string from_ansi(const char* text) {
    if (text == nullptr || *text == '\0') {
        return {};
    }
    const int wide_len = ::MultiByteToWideChar(CP_ACP, 0, text, -1, nullptr, 0);
    if (wide_len <= 1) {
        return std::string(text);
    }
    std::wstring wide(static_cast<std::size_t>(wide_len), L'\0');
    ::MultiByteToWideChar(CP_ACP, 0, text, -1, wide.data(), wide_len);

    const int utf8_len =
        ::WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), -1, nullptr, 0, nullptr, nullptr);
    if (utf8_len <= 1) {
        return {};
    }
    std::string utf8(static_cast<std::size_t>(utf8_len), '\0');
    ::WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), -1, utf8.data(), utf8_len, nullptr, nullptr);
    utf8.resize(static_cast<std::size_t>(utf8_len - 1));
    return utf8;
}
#endif

// `ling job` 的三种动作。任务定义存在 core-settings 里，由 core-job 解释；
// 这三条命令只是把参数整理成一次 kernel.call（04/06 §4.5、§5.7）。
int run_job(const Args& args) {
    const auto print_job_usage = [](std::FILE* out) {
        std::fprintf(out, "ling job 用来管定时任务。任务存在配置里，重启宿主也不丢。\n\n");
        std::fprintf(out, "  ling job list                     列出所有任务\n");
        std::fprintf(out, "  ling job add <任务名> --cron \"<时间表>\" --call <插件方法> [--args <JSON>] [--catch-up]\n");
        std::fprintf(out, "  ling job remove <任务名>          删掉一个任务\n\n");
        std::fprintf(out, "时间表是五段：分 时 日 月 周，最短一分钟。例：\"0 3 * * *\" 是每天 03:00，\"*/10 * * * *\" 是每 10 分钟。\n");
        std::fprintf(out, "插件方法写成「插件名.方法名」，例如 core-log.write。\n");
        std::fprintf(out, "--args 是传给那个方法的参数，JSON 对象，例如 '{\"plugin\":\"core-job\",\"message\":\"到点了\"}'。\n");
        std::fprintf(out, "--catch-up 表示宿主停机期间错过的那一次，启动后补跑。\n");
    };

    if (args.positional.empty()) {
        print_job_usage(stderr);
        return 2;
    }

    const std::string action = args.positional[0];
    std::string inner_method;
    nlohmann::json inner_args = nlohmann::json::object();

    if (action == "list") {
        inner_method = "core-job.list";
    } else if (action == "remove" || action == "rm") {
        if (args.positional.size() != 2) {
            std::fprintf(stderr, "要删哪个任务？写成：ling job remove <任务名>\n");
            std::fprintf(stderr, "任务名用 ling job list 看。\n");
            return 2;
        }
        inner_method = "core-job.cancel";
        inner_args["name"] = args.positional[1];
    } else if (action == "add") {
        if (args.positional.size() != 2) {
            std::fprintf(stderr, "要建哪条任务？写成：ling job add <任务名> --cron ... --call ...\n");
            return 2;
        }
        if (args.cron.empty()) {
            std::fprintf(stderr, "缺 --cron。时间表是五段：分 时 日 月 周，例如 --cron \"0 3 * * *\" 表示每天 03:00。\n");
            return 2;
        }
        if (args.call_method.empty()) {
            std::fprintf(stderr, "缺 --call。要说明到点调哪个插件的方法，例如 --call core-log.write。\n");
            return 2;
        }
        // 先让 core-job 自己校验一遍时间表。它的错误码只有数字，只有这个接口
        // 能把「「时」段取值 25 超出范围 0 至 23」这种原因原样带回来。
        const nlohmann::json probe_args = {{"method", "core-job.validate"},
                                           {"args", {{"cron", args.cron}}}};
        const ling::AdminReply probe = ling::admin::call(args.data_dir, "kernel.call",
                                                         probe_args.dump(), 30000);
        if (!probe.ok()) {
            print_failure(probe);
            return exit_code_of(probe);
        }
        const auto checked = nlohmann::json::parse(probe.result, nullptr, false);
        if (!checked.is_discarded() && checked.contains("ok") && !checked.value("ok", false)) {
            std::fprintf(stderr, "时间表写错了：%s\n", checked.value("reason", "").c_str());
            std::fprintf(stderr, "格式是五段：分 时 日 月 周。例：\"0 3 * * *\" 每天 03:00，"
                                 "\"*/10 * * * *\" 每 10 分钟。\n");
            return 2;
        }

        inner_method = "core-job.schedule";
        inner_args["name"] = args.positional[1];
        inner_args["cron"] = args.cron;
        inner_args["method"] = args.call_method;
        if (!args.call_args.empty()) {
            const auto parsed = nlohmann::json::parse(args.call_args, nullptr, false);
            if (parsed.is_discarded() || !parsed.is_object()) {
                std::fprintf(stderr, "--args 只收 JSON 对象，例如 '{\"message\":\"hi\"}'。\n");
                return 2;
            }
            inner_args["args"] = parsed;
        }
        if (args.catch_up) {
            inner_args["catch_up"] = true;
        }
    } else {
        std::fprintf(stderr, "job 后面只能是 list、add、remove，不认识：%s\n\n", action.c_str());
        print_job_usage(stderr);
        return 2;
    }

    const nlohmann::json payload = {{"method", inner_method}, {"args", inner_args}};
    const ling::AdminReply reply = ling::admin::call(args.data_dir, "kernel.call",
                                                     payload.dump(), 30000);
    if (!reply.ok()) {
        if (args.json) {
            nlohmann::json error;
            error["ok"] = false;
            error["error"]["code"] = reply.code.empty() ? "E_INTERNAL" : reply.code;
            error["error"]["message"] = strip_code(reply.message, reply.code);
            std::printf("%s\n", error.dump().c_str());
            return exit_code_of(reply);
        }
        print_failure(reply);
        if (reply.code == ling::err::kPluginMissing) {
            std::fprintf(stderr, "提示：定时任务要靠 core-job 这个内置插件，用 ling plugins 看它在不在。\n");
        }
        return exit_code_of(reply);
    }

    if (args.json) {
        std::printf("%s\n", reply.result.c_str());
        return 0;
    }

    if (action == "list") {
        const auto document = nlohmann::json::parse(reply.result, nullptr, false);
        if (document.is_discarded() || !document.contains("jobs") || document["jobs"].empty()) {
            std::printf("现在没有定时任务。\n");
            std::printf("要建一条：ling job add 每天备份 --cron \"0 3 * * *\" --call core-log.write\n");
            return 0;
        }
        std::printf("%-20s %-16s %-24s %s\n", "任务名", "时间表", "到点调用", "上次触发");
        for (const auto& job : document["jobs"]) {
            std::string last = job.contains("last_fired") ? job.value("last_fired", "") : "";
            if (last.empty()) {
                last = "还没跑过";
            }
            std::printf("%-20s %-16s %-24s %s\n", job.value("name", "").c_str(),
                        job.value("cron", "").c_str(), job.value("method", "").c_str(),
                        last.c_str());
        }
        return 0;
    }

    if (action == "add") {
        std::printf("已建任务 %s：%s 调用 %s。\n", args.positional[1].c_str(), args.cron.c_str(),
                    args.call_method.c_str());
        std::printf("想确认：ling job list。想删掉：ling job remove %s。\n",
                    args.positional[1].c_str());
        return 0;
    }

    std::printf("已删任务 %s。\n", args.positional[1].c_str());
    return 0;
}

int main(int argc, char** argv) {
#ifdef _WIN32
    // 必须放在任何输出之前：Kernel 构造时就会往 stderr 打日志。
    ConsoleUtf8Scope console_utf8;
#endif
    Args args;
    args.data_dir = default_data_root();

    std::vector<std::string> raw_args;
    raw_args.reserve(static_cast<std::size_t>(argc > 0 ? argc - 1 : 0));
    for (int i = 1; i < argc; ++i) {
#ifdef _WIN32
        raw_args.push_back(from_ansi(argv[i]));
#else
        raw_args.push_back(argv[i]);
#endif
    }
    const int arg_count = static_cast<int>(raw_args.size());

    for (int i = 0; i < arg_count; ++i) {
        const std::string& token = raw_args[i];
        if (token == "--version") {
            std::printf("ling %s\n", ling::version_string());
            return 0;
        }
        if (token == "--help") {
            print_usage(stdout);
            return 0;
        }
        if (token == "--data-dir" && i + 1 < arg_count) {
            args.data_dir = raw_args[++i];
            continue;
        }
        if (token == "--port" && i + 1 < arg_count) {
            args.port = static_cast<std::uint16_t>(std::atoi(raw_args[++i].c_str()));
            continue;
        }
        if (token == "--purge") {
            args.purge = true;
            continue;
        }
        if (token == "--json") {
            args.json = true;
            continue;
        }
        if (token == "--cron" && i + 1 < arg_count) {
            args.cron = raw_args[++i];
            continue;
        }
        if (token == "--call" && i + 1 < arg_count) {
            args.call_method = raw_args[++i];
            continue;
        }
        if (token == "--args" && i + 1 < arg_count) {
            args.call_args = raw_args[++i];
            continue;
        }
        if (token == "--catch-up") {
            args.catch_up = true;
            continue;
        }
        if (args.command.empty()) {
            args.command = token;
            continue;
        }
        args.positional.push_back(token);
    }

    if (args.command.empty()) {
        print_usage(stderr);
        return 2;
    }

    if (args.command == "serve") {
        // 内置插件的产物在 <程序目录>/plugins/ 下，serve 用这个位置做首次预装。
        std::error_code ec;
#ifdef _WIN32
        const auto self_path = std::filesystem::u8path(from_ansi(argc > 0 ? argv[0] : ""));
#else
        const auto self_path = std::filesystem::path(argc > 0 ? argv[0] : "");
#endif
        const auto program_dir = std::filesystem::absolute(self_path, ec).parent_path();
        return run_serve(args, ec ? std::filesystem::current_path() : program_dir);
    }
    if (args.command == "logs" && args.positional.empty()) {
        return print_kernel_logs(args.data_dir);
    }

    if (args.command == "job") {
        return run_job(args);
    }

    // 其余子命令都经管理端口。方法名与子命令的对应见 04/06 §4.1。
    struct Plan {
        const char* command;
        const char* method;
        int timeout_ms;
        bool needs_id;
    };
    const Plan plans[] = {
        {"shutdown", "kernel.shutdown", 10000, false},
        {"plugins", "kernel.plugins", 30000, false},
        {"install", "kernel.install", 60000, true},
        {"uninstall", "kernel.uninstall", 60000, true},
        {"enable", "kernel.enable", 30000, true},
        {"disable", "kernel.disable", 30000, true},
        {"logs", "kernel.logs", 30000, true},
        {"reload", "kernel.reload", 60000, true},
        {"restart", "kernel.restart", 60000, true},
    };

    const Plan* plan = nullptr;
    for (const auto& item : plans) {
        if (args.command == item.command) {
            plan = &item;
            break;
        }
    }
    if (plan == nullptr) {
        std::fprintf(stderr, "不认识这个命令：%s\n\n", args.command.c_str());
        print_usage(stderr);
        return 2;
    }
    if (plan->needs_id && args.positional.size() != 1) {
        if (args.command == "install") {
            std::fprintf(stderr, "install 要指定插件目录，例如：ling install D:////plugins////my-plugin//n");
        } else if (args.command == "logs") {
            std::fprintf(stderr, "logs 后面要么不写（看 ling 自己的日志），要么写一个插件名。\n");
        } else {
            std::fprintf(stderr, "%s 要指定插件名，例如：ling %s demo-a\n",
                         args.command.c_str(), args.command.c_str());
        }
        std::fprintf(stderr, "插件名就是 ling plugins 第一列显示的那个。\n");
        return 2;
    }

    nlohmann::json args_object = nlohmann::json::object();
    if (args.command == "install") {
        args_object["path"] = args.positional[0];
    } else if (args.command == "uninstall") {
        args_object["id"] = args.positional[0];
        args_object["purge"] = args.purge;
    } else if (args.command == "logs") {
        args_object["plugin"] = args.positional[0];
    } else if (plan->needs_id) {
        args_object["id"] = args.positional[0];
    }

    const ling::AdminReply reply = ling::admin::call(args.data_dir, plan->method,
                                                     args_object.dump(), plan->timeout_ms);
    if (!reply.ok()) {
        if (args.json) {
            // 加 --json 的是脚本。屏幕给人看人话，脚本要的是错误码，两者分开给。
            nlohmann::json error;
            error["ok"] = false;
            error["error"]["code"] = reply.code.empty() ? "E_INTERNAL" : reply.code;
            error["error"]["message"] = strip_code(reply.message, reply.code);
            std::printf("%s\n", error.dump().c_str());
        } else {
            print_failure(reply);
        }
        return exit_code_of(reply);
    }

    if (args.json) {
        std::printf("%s\n", reply.result.c_str());
        return 0;
    }

    const auto document = nlohmann::json::parse(reply.result, nullptr, false);
    const auto field = [&document](const char* key) {
        if (document.is_discarded() || !document.contains(key)) {
            return std::string();
        }
        return document.value(key, "");
    };
    const std::string plugin_id = field("id");
    const std::string version = field("version");

    if (args.command == "plugins") {
        print_plugins(reply.result);
    } else if (args.command == "logs") {
        print_log_lines(reply.result);
    } else if (args.command == "shutdown") {
        std::printf("已通知 ling 退出。\n");
    } else if (args.command == "install") {
        const std::string plugin_state = field("state");
        const bool ready = plugin_state.empty() || plugin_state == "activated";
        if (ready) {
            if (version.empty()) {
                std::printf("已安装 %s，现在可以用。\n", plugin_id.c_str());
            } else {
                std::printf("已安装 %s %s，现在可以用。\n", plugin_id.c_str(), version.c_str());
            }
        } else {
            // 装上了但没跑起来，两种情况：等依赖，或者启动时就失败了。回显分开说，
            // 因为下一步完全不同——前者去装缺的，后者去看日志。
            const std::string reason = field("reason");
            const bool waiting = reason.find("缺 ") != std::string::npos ||
                                 reason.find("依赖") != std::string::npos ||
                                 reason.find("版本不符") != std::string::npos;
            if (waiting) {
                std::printf("已安装 %s %s，但现在还用不了。\n", plugin_id.c_str(),
                            version.c_str());
                if (!reason.empty()) {
                    std::printf("  原因：%s\n", reason.c_str());
                }
                std::printf("  把缺的装上就会自动启用，不用重启。\n");
            } else {
                std::printf("已安装 %s %s，但它没能启动。\n", plugin_id.c_str(), version.c_str());
                if (!reason.empty()) {
                    std::printf("  原因：%s\n", reason.c_str());
                }
                std::printf("  用 ling logs %s 看详细的报错。\n", plugin_id.c_str());
            }
        }
    } else if (args.command == "uninstall") {
        if (args.purge) {
            std::printf("已卸载 %s，插件和它的数据都删掉了。\n", plugin_id.c_str());
        } else {
            const auto data_dir = args.data_dir / "plugins" / plugin_id / "data";
            // 相对路径直接打印会混用 / 与 \，换成绝对路径更好认。
            std::error_code ec;
            const auto shown = std::filesystem::absolute(data_dir, ec);
            std::printf("已卸载 %s，它的数据还留在：\n", plugin_id.c_str());
            std::printf("  %s\n", (ec ? data_dir : shown).string().c_str());
            std::printf("下次装回来会用上这份数据。要连它一起删，加 --purge。\n");
        }
    } else if (args.command == "reload" || args.command == "restart") {
        const std::string plugin_state = field("state");
        if (plugin_state == "activated") {
            std::printf("已重新加载 %s，现在可以用。\n", plugin_id.c_str());
        } else {
            std::printf("已重新加载 %s，但它现在没在跑。\n", plugin_id.c_str());
            const std::string reason = field("reason");
            if (!reason.empty()) {
                std::printf("  原因：%s\n", reason.c_str());
            }
        }
    } else if (args.command == "enable") {
        std::printf("已启用 %s。\n", plugin_id.c_str());
    } else if (args.command == "disable") {
        std::printf("已停用 %s，数据还在，随时能再启用。\n", plugin_id.c_str());
    } else {
        std::printf("%s 完成。\n", args.command.c_str());
    }
    return 0;
}
