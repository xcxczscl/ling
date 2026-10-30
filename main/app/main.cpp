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

void print_usage(std::FILE* out) {
    std::fprintf(out, "ling %s\n", ling::version_string());
    std::fprintf(out, "用法：ling <子命令> [选项] [--data-dir <路径>] [--json]\n");
    std::fprintf(out, "  serve               前台启动宿主，监听 127.0.0.1:7900 起\n");
    std::fprintf(out, "  shutdown            让运行中的宿主退出\n");
    std::fprintf(out, "  plugins             列出插件与状态\n");
    std::fprintf(out, "  install <目录>      安装并激活插件\n");
    std::fprintf(out, "  uninstall <编号>    卸载插件，--purge 连 data/ 一起删\n");
    std::fprintf(out, "  enable <编号>       启用插件\n");
    std::fprintf(out, "  disable <编号>      停用插件\n");
    std::fprintf(out, "  logs [编号]         不带编号时打印内核日志末 40 行\n");
    std::fprintf(out, "  --version           打印版本\n");
    std::fprintf(out, "  --help              打印本说明\n");
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
        std::printf("还没有日志\n");
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
    for (const auto& plugin : document["plugins"]) {
        std::printf("%-24s %-10s %-12s %s", plugin.value("id", "").c_str(),
                    plugin.value("version", "").c_str(), plugin.value("state", "").c_str(),
                    plugin.value("track", "").c_str());
        if (plugin.contains("reason")) {
            std::printf("  %s", plugin.value("reason", "").c_str());
        }
        std::printf("\n");
    }
}

void print_log_lines(const std::string& result_json) {
    const auto document = nlohmann::json::parse(result_json, nullptr, false);
    if (document.is_discarded() || !document.contains("lines")) {
        std::printf("%s\n", result_json.c_str());
        return;
    }
    if (document["lines"].empty()) {
        std::printf("没有日志（%s）\n", document.value("path", "").c_str());
        return;
    }
    for (const auto& line : document["lines"]) {
        std::printf("%s\n", line.get<std::string>().c_str());
    }
}

// 把错误类别映射成退出码（04/06 §4.2）。
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
        std::fprintf(stderr, "提示：%zu 个插件激活失败，见日志\n", failures);
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

    std::printf("ling %s 已启动，管理端口 127.0.0.1:%u，数据根 %s\n", ling::version_string(),
                static_cast<unsigned>(server.port()), args.data_dir.string().c_str());
    std::fflush(stdout);

    while (!g_stop_requested.load()) {
        if (!server.poll(200)) {
            break;
        }
    }
    server.stop();
    std::printf("宿主已退出\n");
    return 0;
}

}  // namespace

int main(int argc, char** argv) {
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
        std::fprintf(stderr, "未知子命令：%s\n", args.command.c_str());
        print_usage(stderr);
        return 2;
    }
    if (plan->needs_id && args.positional.size() != 1) {
        std::fprintf(stderr, "子命令 %s 需要一个参数\n", args.command.c_str());
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
        std::fprintf(stderr, "%s%s%s\n", reply.code.empty() ? "" : reply.code.c_str(),
                     reply.code.empty() ? "" : " ", reply.message.c_str());
        return exit_code_of(reply);
    }

    if (args.json) {
        std::printf("%s\n", reply.result.c_str());
        return 0;
    }

    if (args.command == "plugins") {
        print_plugins(reply.result);
    } else if (args.command == "logs") {
        print_log_lines(reply.result);
    } else if (args.command == "shutdown") {
        std::printf("已请求宿主退出\n");
    } else {
        std::printf("%s 成功\n", args.command.c_str());
    }
    return 0;
}
