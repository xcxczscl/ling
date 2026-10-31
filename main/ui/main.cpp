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
#include <iostream>
#include <thread>
#include <fstream>
#include <string>

#include "ling/admin.hpp"
#include "ling/errors.hpp"
#include "ling/ui_server.hpp"
#include "ling/version.hpp"

#include <nlohmann/json.hpp>

#include "ling/sidecar_proto.hpp"

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

// ---------- 侧车模式 ----------
//
// 被宿主拉起时，环境变量里有 LING_TOKEN 与 LING_PLUGIN_ID。这时这个进程就是个
// 侧车插件：先握手，再开 HTTP 服务，收到 deactivate / shutdown 就自己退出。
// 手动调试（用户直接跑这个 exe）没有这两个变量，走原来的「读 session.json」路径。

// 侧车的 LING_DATA_DIR 是「自己的数据目录」（<数据根>/plugins/core-ui/data），
// 而界面要连宿主的管理端口，那里需要数据根。回退三级，不另加一个环境变量：
// 宿主对每种插件都按这个布局给目录，推导是确定的。
std::filesystem::path data_root_from_sidecar(const std::string& data_dir) {
    return std::filesystem::path(data_dir).parent_path().parent_path().parent_path();
}

// 按协议发一帧。侧车的 stdout 只许写帧，调试信息一律走 stderr。
void send_frame(const nlohmann::json& frame) {
    std::cout << frame.dump() << "\n" << std::flush;
}

void send_hello(const std::string& plugin_id, const std::string& token) {
    nlohmann::json frame;
    frame["v"] = ling::sidecar::kProtocolVersion;
    frame["op"] = "hello";
    frame["plugin_id"] = plugin_id;
    frame["plugin_version"] = "1.0.0";
    frame["token"] = token;
    send_frame(frame);
}

// 宿主发过来的指令帧。读到 deactivate / shutdown 就停。
std::atomic<bool> g_host_shutdown{false};

// stdin 由宿主独占写，我们起一个线程读，避免和 HTTP 的 select 循环搅在一起。
void read_host_frames() {
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty()) {
            continue;
        }
        ling::sidecar::Frame frame;
        std::string error;
        if (!ling::sidecar::parse(line, &frame, &error)) {
            std::fprintf(stderr, "[core-ui] 收到一帧没法解析：%s\n", error.c_str());
            continue;
        }
        if (frame.op == "deactivate") {
            nlohmann::json ack;
            ack["v"] = ling::sidecar::kProtocolVersion;
            ack["op"] = "deactivated";
            send_frame(ack);
            g_host_shutdown.store(true);
            return;
        }
        if (frame.op == "shutdown") {
            g_host_shutdown.store(true);
            return;
        }
        // 其余（activate）在主流程里处理；这里只关心退出。
    }
    // stdin 关了，说明宿主走了。
    g_host_shutdown.store(true);
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

    // 侧车模式：有 LING_TOKEN 就说明是宿主拉起的，先握手再干别的。
    const std::string sidecar_token = env_or_empty("LING_TOKEN");
    const std::string plugin_id = env_or_empty("LING_PLUGIN_ID");
    const bool sidecar = !sidecar_token.empty();
    if (sidecar) {
        const std::string own_data = env_or_empty("LING_DATA_DIR");
        if (!own_data.empty()) {
            data_root = data_root_from_sidecar(own_data);
        }
        send_hello(plugin_id, sidecar_token);
        // stdin 读线程负责收 deactivate / shutdown；主线程只处理 activate。
        std::thread reader(read_host_frames);
        reader.detach();
    }

    // 界面的口令用 session.json 里那个：浏览器地址栏里能填的只有它。
    // 握手用的侧车口令另有一份，只用来证明「是宿主拉起我的」，别拿它去认证页面。
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
    //
    // 但第一次连不上不一定是坏事：宿主是先 load_all 再开管理端口，所以装载阶段
    // 拉起的界面，自检必然早到一步。给它 15 秒等宿主把端口开起来，别上来就退出。
    ling::AdminReply probe;
    bool host_ready = false;
    for (int attempt = 0; attempt < 15; ++attempt) {
        probe = ling::admin::call(data_root, "kernel.plugins", "{}", 2000);
        if (probe.ok()) {
            host_ready = true;
            break;
        }
        if (probe.kind != ling::AdminReply::Kind::kUnreachable) {
            break;  // 口令不对这类问题，等再久也不会自己好
        }
        std::this_thread::sleep_for(std::chrono::seconds(1));
    }
    if (!host_ready) {
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

    // 手动调试时打一行就绪信息，方便脚本知道端口。侧车模式下不能往 stdout 打
    // 非帧文本：宿主把 stdout 当帧流，一行普通文本会被判成坏帧。侧车的就绪
    // 由握手表达，不需要额外一行。
    if (!sidecar) {
        nlohmann::json ready;
        ready["ready"] = true;
        ready["port"] = server.port();
        std::printf("%s\n", ready.dump().c_str());
        std::fflush(stdout);
    }

    // 宿主退出后界面留在后台没有意义，探测三次连不上就自己收尾。
    // 每 2 秒探一次，不是每轮：一次探测要开套接字、读口令文件，按 200 毫秒来做纯属浪费。
    int unreachable = 0;
    auto last_probe = std::chrono::steady_clock::now();
    while (!g_stop_requested.load() && !g_host_shutdown.load()) {
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
