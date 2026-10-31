// core-ui 公共「起服务」逻辑的实现。详见 app.h。
#include "app.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <csignal>
#include <fstream>
#include <iostream>
#include <thread>

#include "ling/admin.hpp"
#include "ling/errors.hpp"
#include "ling/sidecar_proto.hpp"

#include <nlohmann/json.hpp>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

namespace ling {
namespace {

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

// 侧车的 LING_DATA_DIR 是「自己的数据目录」（<数据根>/plugins/core-ui/data），
// 而界面要连宿主的管理端口，那里需要数据根。回退三级。
std::filesystem::path data_root_from_sidecar(const std::string& data_dir) {
    return std::filesystem::path(data_dir).parent_path().parent_path().parent_path();
}

// 往 stdout 写一帧。侧车的 stdout 只许写帧，调试信息一律走 stderr。
void send_frame(const nlohmann::json& frame) {
    std::printf("%s\n", frame.dump().c_str());
    std::fflush(stdout);
}

void send_hello(const std::string& plugin_id, const std::string& token) {
    nlohmann::json frame;
    frame["v"] = sidecar::kProtocolVersion;
    frame["op"] = "hello";
    frame["plugin_id"] = plugin_id;
    frame["plugin_version"] = "1.0.0";
    frame["token"] = token;
    send_frame(frame);
}

// stdin 由宿主独占写，起一个线程读，避免和 HTTP 的 select 循环搅在一起。
void read_host_frames(UiApp* app) {
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty()) {
            continue;
        }
        sidecar::Frame frame;
        std::string error;
        if (!sidecar::parse(line, &frame, &error)) {
            continue;
        }
        if (frame.op == "deactivate") {
            nlohmann::json ack;
            ack["v"] = sidecar::kProtocolVersion;
            ack["op"] = "deactivated";
            send_frame(ack);
            app->stop.store(true);
            return;
        }
        if (frame.op == "shutdown") {
            app->stop.store(true);
            return;
        }
    }
    app->stop.store(true);  // stdin 关了，宿主走了
}

// 读 core-ui.auth：关掉口令校验时浏览器直接开就能用。读不到按「要」处理。
bool ui_requires_auth(const std::filesystem::path& data_root) {
    nlohmann::json request;
    request["method"] = "core-settings.get";
    request["args"] = {{"key", "core-ui.auth"}};
    const AdminReply reply = admin::call(data_root, "kernel.call", request.dump(), 3000);
    if (!reply.ok()) {
        return true;
    }
    const auto parsed = nlohmann::json::parse(reply.result, nullptr, false);
    if (parsed.is_discarded() || !parsed.contains("found") || !parsed["found"].get<bool>()) {
        return true;
    }
    return !(parsed["value"].is_string() && parsed["value"].get<std::string>() == "off");
}

// 把界面端口写进 registry.json。读-改-写，只动 ui_port 一个字段。
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

#ifdef _WIN32
BOOL WINAPI on_console_ctrl(DWORD type) {
    (void)type;
    return TRUE;
}
#else
void on_posix_signal(int) {}
#endif

}  // namespace

int ui_app_setup(int argc, char** argv, UiApp* app) {
#ifdef _WIN32
    ::SetConsoleCtrlHandler(on_console_ctrl, TRUE);
#else
    std::signal(SIGINT, on_posix_signal);
    std::signal(SIGTERM, on_posix_signal);
#endif

    const Args args = parse_args(argc, argv);
    if (args.help) {
        return 2;
    }

    std::filesystem::path data_root = args.data_dir;
    const std::string env_root = env_or_empty("LING_UI_ROOT");
    if (!env_root.empty()) {
        data_root = env_root;
    }

    // 侧车模式：有 LING_TOKEN 就说明是宿主拉起的，先握手。
    const std::string sidecar_token = env_or_empty("LING_TOKEN");
    const std::string plugin_id = env_or_empty("LING_PLUGIN_ID");
    app->sidecar = !sidecar_token.empty();
    if (app->sidecar) {
        const std::string own_data = env_or_empty("LING_DATA_DIR");
        if (!own_data.empty()) {
            data_root = data_root_from_sidecar(own_data);
        }
        send_hello(plugin_id, sidecar_token);
        std::thread reader(read_host_frames, app);
        reader.detach();
    }

    // 口令用 session.json 里那个（浏览器能填的只有它）。握手用的侧车口令另算。
    std::string token = env_or_empty("LING_UI_TOKEN");
    if (token.empty()) {
        std::string error;
        if (!admin::read_token(data_root, &token, &error)) {
            std::fprintf(stderr, "拿不到口令：%s\n", error.c_str());
            std::fprintf(stderr, "这个程序要连上一个正在运行的 ling 宿主。先跑 ling serve。\n");
            return 3;
        }
        std::fprintf(stderr, "没有 LING_UI_TOKEN，改读 session.json 拿口令（调试用）。\n");
    }

    // 自检：绑上端口不代表能连上宿主。装载阶段拉起的界面自检会早到，
    // 连不上就等，最多 15 秒；口令不对这类等也不会好的错误才立即退出。
    AdminReply probe;
    bool host_ready = false;
    for (int attempt = 0; attempt < 15; ++attempt) {
        probe = admin::call(data_root, "kernel.plugins", "{}", 2000);
        if (probe.ok()) {
            host_ready = true;
            break;
        }
        if (probe.kind != AdminReply::Kind::kUnreachable) {
            break;
        }
        std::this_thread::sleep_for(std::chrono::seconds(1));
    }
    if (!host_ready) {
        std::fprintf(stderr, "连不上 ling 宿主：%s\n", probe.message.c_str());
        std::fprintf(stderr, "先确认 ling serve 在跑；如果刚重启过宿主，口令会变，重新起本程序。\n");
        return 3;
    }

    app->data_root = data_root;
    app->token = token;
    app->server = std::make_unique<UiServer>();
    UiServer::Options options;
    options.data_root = data_root;
    options.token = token;
    options.start_port = args.port;
    options.last_port = static_cast<std::uint16_t>(args.port + 89);
    const std::string error = app->server->start(options);
    if (!error.empty()) {
        std::fprintf(stderr, "界面起不来：%s\n", error.c_str());
        return 3;
    }

    app->port = app->server->port();
    note_ui_port(data_root, app->port);
    app->url = "http://127.0.0.1:" + std::to_string(app->port) + "/?token=" + token;

    // 手动调试时打一行就绪信息。侧车模式下不能往 stdout 打非帧文本。
    if (!app->sidecar) {
        nlohmann::json ready;
        ready["ready"] = true;
        ready["port"] = app->port;
        std::printf("%s\n", ready.dump().c_str());
        std::fflush(stdout);
    }
    return 0;
}

bool ui_app_tick(UiApp* app) {
    if (app->stop.load() || app->host_down.load()) {
        return false;
    }
    app->server->poll(50);

    const auto now = std::chrono::steady_clock::now();
    if (app->last_probe.time_since_epoch().count() != 0 &&
        std::chrono::duration_cast<std::chrono::seconds>(now - app->last_probe).count() < 2) {
        return true;
    }
    app->last_probe = now;
    app->server->set_require_auth(ui_requires_auth(app->data_root));
    const AdminReply alive = admin::call(app->data_root, "kernel.plugins", "{}", 1000);
    if (alive.kind == AdminReply::Kind::kUnreachable) {
        if (++app->unreachable >= 3) {
            std::fprintf(stderr, "宿主已经退出，界面跟着关。\n");
            return false;
        }
    } else {
        app->unreachable = 0;
    }
    return true;
}

void ui_app_cleanup(UiApp* app) {
    if (app->server) {
        app->server->stop();
    }
    if (!app->data_root.empty()) {
        clear_ui_port(app->data_root);
    }
}

}  // namespace ling
