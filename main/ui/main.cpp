// 界面进程入口。
//
// 现阶段它只做服务端：HTTP + 认证 + /api/* 转管理端口，页面是一个占位页。
// 宿主侧的侧车轨道（M4）打通后，这个可执行文件原样装进插件目录就是 core-ui 的侧车产物，
// 只需要把页面换成真页面（docs/ling/builtin-plugins/core-ui/02-服务端与接口.md §3.1）。
#include <atomic>
#include <chrono>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>

#include "ling/admin.hpp"
#include "ling/errors.hpp"
#include "ling/ui_server.hpp"
#include "ling/version.hpp"

#include <nlohmann/json.hpp>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

namespace {

std::atomic<bool> g_stop_requested{false};

#ifdef _WIN32
BOOL WINAPI on_console_ctrl(DWORD type) {
    (void)type;
    g_stop_requested.store(true);
    return TRUE;
}

// 控制台按 GBK 解读 UTF-8 会出乱码，退出时还原，避免污染同一个窗口。
class ConsoleUtf8Scope {
public:
    ConsoleUtf8Scope() {
        in_ = ::GetConsoleCP();
        out_ = ::GetConsoleOutputCP();
        ::SetConsoleCP(CP_UTF8);
        ::SetConsoleOutputCP(CP_UTF8);
    }
    ~ConsoleUtf8Scope() {
        if (in_ != 0) {
            ::SetConsoleCP(in_);
        }
        if (out_ != 0) {
            ::SetConsoleOutputCP(out_);
        }
    }

private:
    UINT in_ = 0;
    UINT out_ = 0;
};
#else
void on_posix_signal(int) { g_stop_requested.store(true); }
class ConsoleUtf8Scope {
public:
    ConsoleUtf8Scope() = default;
};
#endif

// MSVC 把 getenv 标为不安全且开着 /WX，Windows 上走 _dupenv_s。
std::string env_or_empty(const char* name) {
#ifdef _WIN32
    char* value = nullptr;
    std::size_t size = 0;
    if (_dupenv_s(&value, &size, name) != 0 || value == nullptr) {
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
    const std::string appdata = env_or_empty("APPDATA");
    if (!appdata.empty()) {
        return std::filesystem::path(appdata) / "ling";
    }
    const std::string home = env_or_empty("HOME");
    if (!home.empty()) {
        return std::filesystem::path(home) / ".local" / "share" / "ling";
    }
    return std::filesystem::current_path() / "ling-data";
}

// 把界面端口写进 registry.json。读-改-写，只动 ui_port 一个字段：
// 端口是宿主写进去的（内核重写时会保留 ui_port），这里不该碰其他字段。
void note_ui_port(const std::filesystem::path& data_root, std::uint16_t port) {
    const auto path = data_root / "registry.json";
    nlohmann::json document = nlohmann::json::object();
    {
        std::ifstream input(path);
        if (input) {
            const auto parsed = nlohmann::json::parse(input, nullptr, false);
            if (!parsed.is_discarded() && parsed.is_object()) {
                document = parsed;
            }
        }
    }
    document["ui_port"] = port;
    std::ofstream output(path, std::ios::trunc);
    if (output) {
        output << document.dump(2) << "\n";
    }
}

void clear_ui_port(const std::filesystem::path& data_root) {
    const auto path = data_root / "registry.json";
    std::ifstream input(path);
    if (!input) {
        return;
    }
    auto parsed = nlohmann::json::parse(input, nullptr, false);
    input.close();
    if (parsed.is_discarded() || !parsed.is_object() || !parsed.contains("ui_port")) {
        return;
    }
    parsed.erase("ui_port");
    std::ofstream output(path, std::ios::trunc);
    if (output) {
        output << parsed.dump(2) << "\n";
    }
}

void print_usage() {
    std::printf("ling-ui %s\n", ling::version_string());
    std::printf("界面进程。平时由 ling serve 拉起，也可以手动起一个来调试接口。\n\n");
    std::printf("用法：ling-ui [选项]\n");
    std::printf("  --data-dir <目录>   数据目录，默认读环境变量 LING_UI_ROOT，再默认 %%APPDATA%%\\ling\n");
    std::printf("  --port <端口>       起始端口，默认 8010，占用则往后顺延到 8099\n");
    std::printf("  --help              打印本说明\n\n");
    std::printf("口令：优先读环境变量 LING_UI_TOKEN；没有则读 <数据目录>/session.json（仅调试用）。\n");
    std::printf("起不来时的排查顺序：宿主有没有在跑、端口段是不是被占满、口令与 session.json 是否一致。\n");
}

struct Args {
    std::filesystem::path data_dir;
    std::uint16_t port = 8010;
    bool help = false;
};

Args parse_args(int argc, char** argv) {
    Args args;
    args.data_dir = default_data_root();
    for (int i = 1; i < argc; ++i) {
        const std::string token = argv[i];
        if (token == "--help" || token == "-h") {
            args.help = true;
        } else if (token == "--data-dir" && i + 1 < argc) {
            args.data_dir = argv[++i];
        } else if (token == "--port" && i + 1 < argc) {
            const int value = std::atoi(argv[++i]);
            if (value > 0 && value < 65536) {
                args.port = static_cast<std::uint16_t>(value);
            }
        }
    }
    return args;
}

}  // namespace

int main(int argc, char** argv) {
    ConsoleUtf8Scope console_utf8;
    (void)console_utf8;

#ifdef _WIN32
    ::SetConsoleCtrlHandler(on_console_ctrl, TRUE);
#else
    std::signal(SIGINT, on_posix_signal);
    std::signal(SIGTERM, on_posix_signal);
#endif

    const Args args = parse_args(argc, argv);
    if (args.help) {
        print_usage();
        return 0;
    }

    // 环境变量优先：宿主拉起时会给，用户双击时没有。
    std::filesystem::path data_root = args.data_dir;
    const std::string env_root = env_or_empty("LING_UI_ROOT");
    if (!env_root.empty()) {
        data_root = env_root;
    }

    std::string token = env_or_empty("LING_UI_TOKEN");
    if (token.empty()) {
        std::string error;
        if (!ling::admin::read_token(data_root, &token, &error)) {
            std::fprintf(stderr, "拿不到口令：%s\n", error.c_str());
            std::fprintf(stderr, "这个程序要连上一个正在运行的 ling 宿主。先跑 ling serve。\n");
            return 3;
        }
        std::fprintf(stderr, "没有 LING_UI_TOKEN，改读 session.json 拿口令（调试用）。\n");
    }

    // 自检一步都不能省：绑上端口不代表能连上宿主。口令不对、宿主正在关，
    // 都应该在这里暴露，而不是等用户点了按钮才发现（core-ui/02 §4）。
    const ling::AdminReply probe = ling::admin::call(data_root, "kernel.plugins", "{}", 5000);
    if (!probe.ok()) {
        std::fprintf(stderr, "连不上 ling 宿主：%s\n", probe.message.c_str());
        std::fprintf(stderr, "先确认 ling serve 在跑；如果刚重启过宿主，口令会变，重新起本程序。\n");
        return 3;
    }

    ling::UiServer server;
    ling::UiServer::Options options;
    options.data_root = data_root;
    options.token = token;
    options.start_port = args.port;
    options.last_port = static_cast<std::uint16_t>(args.port + 89);
    const std::string error = server.start(options);
    if (!error.empty()) {
        std::fprintf(stderr, "界面起不来：%s\n", error.c_str());
        return 3;
    }

    note_ui_port(data_root, server.port());

    // 宿主读这一行判定界面就绪，不用「等两秒试试」（core-ui/04 §4.2）。
    nlohmann::json ready;
    ready["ready"] = true;
    ready["port"] = server.port();
    std::printf("%s\n", ready.dump().c_str());
    std::fflush(stdout);

    // 宿主退出后界面留在后台没有意义，探测三次连不上就自己收尾。
    // 每 2 秒探一次，不是每轮：一次探测要开套接字、读口令文件，按 200 毫秒来做纯属浪费。
    int unreachable = 0;
    auto last_probe = std::chrono::steady_clock::now();
    while (!g_stop_requested.load()) {
        server.poll(200);
        const auto now = std::chrono::steady_clock::now();
        if (std::chrono::duration_cast<std::chrono::seconds>(now - last_probe).count() < 2) {
            continue;
        }
        last_probe = now;
        const ling::AdminReply alive = ling::admin::call(data_root, "kernel.plugins", "{}", 1000);
        if (alive.kind == ling::AdminReply::Kind::kUnreachable) {
            if (++unreachable >= 3) {
                std::fprintf(stderr, "宿主已经退出，界面跟着关。\n");
                break;
            }
        } else {
            unreachable = 0;
        }
    }

    server.stop();
    clear_ui_port(data_root);
    return 0;
}
