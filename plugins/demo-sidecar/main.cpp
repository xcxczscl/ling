// 侧车示例：一个独立进程，用 jsonl 帧与宿主通信。
//
// 它验证四件事：握手（hello 带口令）、代登记（宿主替它注册方法）、
// 事件（订阅 kernel.tick 并计数）、调用（invoke → result）。
//
// 协议见 docs/ling/04-详细设计/02-插件装载与接口.md §8。
#include <cstdio>
#include <iostream>
#include <cstdlib>
#include <cstring>
#include <string>

#include <nlohmann/json.hpp>

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

void send_line(const nlohmann::json& frame) {
    std::fputs(frame.dump().c_str(), stdout);
    std::fputc('\n', stdout);
    std::fflush(stdout);
}

void send_ready(const std::string& plugin_id) {
    nlohmann::json frame;
    frame["v"] = 1;
    frame["op"] = "hello";
    frame["plugin_id"] = plugin_id;
    frame["plugin_version"] = "1.0.0";
    frame["token"] = env_or_empty("LING_TOKEN");
    send_line(frame);
}

void send_register(const std::string& name) {
    nlohmann::json frame;
    frame["v"] = 1;
    frame["op"] = "register_method";
    frame["name"] = name;
    send_line(frame);
}

void send_subscribe(const std::string& pattern) {
    nlohmann::json frame;
    frame["v"] = 1;
    frame["op"] = "subscribe";
    frame["pattern"] = pattern;
    send_line(frame);
}

void send_result(int id, const nlohmann::json& result) {
    nlohmann::json frame;
    frame["v"] = 1;
    frame["id"] = id;
    frame["op"] = "result";
    frame["ok"] = true;
    frame["result"] = result;
    send_line(frame);
}

void send_error(int id, const std::string& code, const std::string& message) {
    nlohmann::json frame;
    frame["v"] = 1;
    frame["id"] = id;
    frame["op"] = "error";
    frame["ok"] = false;
    frame["error"] = {{"code", code}, {"message", message}};
    send_line(frame);
}

void append_log(const std::string& line) {
    const std::string dir = env_or_empty("LING_DATA_DIR");
    if (dir.empty()) {
        return;
    }
    const std::string path = dir + "/demo-sidecar.log";
#ifdef _WIN32
    std::FILE* file = nullptr;
    if (::fopen_s(&file, path.c_str(), "ab") == 0 && file != nullptr) {
#else
    std::FILE* file = std::fopen(path.c_str(), "ab");
    if (file != nullptr) {
#endif
        std::fputs(line.c_str(), file);
        std::fputc('\n', file);
        std::fclose(file);
    }
}

}  // namespace

int main() {
    const std::string plugin_id = env_or_empty("LING_PLUGIN_ID");
    if (plugin_id.empty()) {
        std::fputs("缺少 LING_PLUGIN_ID：这个程序只能由 ling 宿主拉起\n", stderr);
        return 2;
    }

    append_log("启动");
    send_ready(plugin_id);

    int ticks = 0;
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty()) {
            continue;
        }
        nlohmann::json frame = nlohmann::json::parse(line, nullptr, false);
        if (frame.is_discarded()) {
            continue;
        }
        const std::string op = frame.value("op", "");

        if (op == "activate") {
            send_register("demo-sidecar.ping");
            send_subscribe("kernel.tick");
            append_log("已激活");
            continue;
        }
        if (op == "invoke") {
            const std::string method = frame.value("method", "");
            const int id = frame.value("id", 0);
            if (method == "demo-sidecar.ping") {
                append_log("收到 invoke id=" + std::to_string(id));
                send_result(id, "pong from sidecar");
                append_log("已回应 id=" + std::to_string(id));
            } else {
                append_log("方法名不匹配：" + method);
                send_error(id, "E_METHOD_MISSING", "这个侧车只提供 demo-sidecar.ping");
                append_log("已回应错误 id=" + std::to_string(id));
            }
            continue;
        }
        if (op == "event") {
            ++ticks;
            if (ticks % 10 == 0) {
                append_log("收到事件 " + std::to_string(ticks) + " 次");
            }
            continue;
        }
        if (op == "deactivate") {
            append_log("收到 deactivate，退出");
            nlohmann::json ack;
            ack["v"] = 1;
            ack["op"] = "deactivated";
            send_line(ack);
            return 0;
        }
        if (op == "shutdown") {
            append_log("收到 shutdown，退出");
            return 0;
        }
    }
    return 0;
}
