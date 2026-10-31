// 命令行入口。serve 之外的所有子命令都经管理端口走 kernel.* 方法（04/06 §4.1），
// 只有 --version、--help 与 logs --kernel 在本地直接完成。
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include "ling/admin.hpp"
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
    std::fprintf(out, "  shutdown             停止正在运行的 ling\n\n");
    std::fprintf(out, "选项：\n");
    std::fprintf(out, "  --purge              卸载时连插件的数据一起删除\n");
    std::fprintf(out, "  --json               输出机器可读的 JSON，方便脚本处理\n");
    std::fprintf(out, "  --version            显示版本\n");
    std::fprintf(out, "  --help               显示本说明\n\n");
    std::fprintf(out, "进阶选项（一般用不到）：\n");
    std::fprintf(out, "  --data-dir <目录>    指定数据目录，默认 %%APPDATA%%\\ling\n");
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

    const std::string detail = strip_code(reply.message, reply.code);
    const ErrorHint* hint = find_hint(reply.code);
    if (hint != nullptr) {
        if (detail.empty()) {
            std::fprintf(stderr, "%s（%s）\n", hint->plain, reply.code.c_str());
        } else {
            std::fprintf(stderr, "%s：%s（%s）\n", hint->plain, detail.c_str(),
                         reply.code.c_str());
        }
        if (hint->advice[0] != '\0') {
            std::fprintf(stderr, "提示：%s。\n", hint->advice);
        }
        return;
    }

    std::fprintf(stderr, "%s（%s）\n", detail.empty() ? reply.message.c_str() : detail.c_str(),
                 reply.code.empty() ? "未知错误" : reply.code.c_str());
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
#endif

int run_serve(const Args& args, const std::filesystem::path& program_dir) {
#ifdef _WIN32
    ::SetConsoleCtrlHandler(on_console_ctrl, TRUE);
#endif
    ling::Kernel kernel(args.data_dir);
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
    }
    server.stop();
    std::printf("ling 已停止。\n");
    return 0;
}

}  // namespace

int main(int argc, char** argv) {
#ifdef _WIN32
    // 必须放在任何输出之前：Kernel 构造时就会往 stderr 打日志。
    ConsoleUtf8Scope console_utf8;
#endif
    Args args;
    args.data_dir = default_data_root();

    for (int i = 1; i < argc; ++i) {
        const std::string token = argv[i];
        if (token == "--version") {
            std::printf("ling %s\n", ling::version_string());
            return 0;
        }
        if (token == "--help") {
            print_usage(stdout);
            return 0;
        }
        if (token == "--data-dir" && i + 1 < argc) {
            args.data_dir = argv[++i];
            continue;
        }
        if (token == "--port" && i + 1 < argc) {
            args.port = static_cast<std::uint16_t>(std::atoi(argv[++i]));
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
        const auto program_dir =
            std::filesystem::absolute(std::filesystem::path(argv[0]), ec).parent_path();
        return run_serve(args, ec ? std::filesystem::current_path() : program_dir);
    }
    if (args.command == "logs" && args.positional.empty()) {
        return print_kernel_logs(args.data_dir);
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
        print_failure(reply);
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
        if (version.empty()) {
            std::printf("已安装 %s，现在可以用。\n", plugin_id.c_str());
        } else {
            std::printf("已安装 %s %s，现在可以用。\n", plugin_id.c_str(), version.c_str());
        }
        if (document.contains("reason")) {
            std::printf("但它启动失败：%s\n", document.value("reason", "").c_str());
            std::printf("用 ling logs %s 看原因。\n", plugin_id.c_str());
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
    } else if (args.command == "enable") {
        std::printf("已启用 %s。\n", plugin_id.c_str());
    } else if (args.command == "disable") {
        std::printf("已停用 %s，数据还在，随时能再启用。\n", plugin_id.c_str());
    } else {
        std::printf("%s 完成。\n", args.command.c_str());
    }
    return 0;
}
