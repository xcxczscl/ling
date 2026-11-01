// 管理端口客户端：找端口、认证、发一帧、读一帧。
// 找不到宿主与认证失败要分开报，两者排查方向不同（04/06 §6.2）。
#include "ling/admin.hpp"

#include <cerrno>
#include <cstddef>
#include <fstream>
#include <string>

#include "ling/errors.hpp"
#include "ling/sys.hpp"

#include <nlohmann/json.hpp>

namespace ling {
namespace admin {
namespace {

bool send_all(sys::socket_handle fd, const std::string& data) {
    std::size_t sent = 0;
    while (sent < data.size()) {
        const auto remaining = static_cast<int>(data.size() - sent);
        const int written = ::send(fd, data.data() + sent, remaining, 0);
        if (written <= 0) {
            return false;
        }
        sent += static_cast<std::size_t>(written);
    }
    return true;
}

// 读到一行为止。返回空表示连接已断或超时，两者用 timed_out 区分：
// 「宿主忙到不应答」与「宿主断了」给使用者的下一步完全不同。
std::string read_line(sys::socket_handle fd, bool* timed_out = nullptr) {
    if (timed_out != nullptr) {
        *timed_out = false;
    }
    std::string buffer;
    char chunk[4096];
    for (;;) {
        const int received = ::recv(fd, chunk, sizeof(chunk), 0);
        if (received < 0) {
            // 出错：只有「超时/被阻塞」才归为超时；其余是连接错误。
            if (timed_out != nullptr && sys::socket_would_block()) {
                *timed_out = true;
            }
            return {};
        }
        if (received == 0) {
            // 对端正常关闭。errno 此时未定义，不能读它判断超时，直接按断开处理。
            return {};
        }
        buffer.append(chunk, static_cast<std::size_t>(received));
        const auto newline = buffer.find('\n');
        if (newline != std::string::npos) {
            return buffer.substr(0, newline);
        }
        if (buffer.size() > 1024 * 1024) {
            return {};
        }
    }
}

}  // namespace

bool read_port(const std::filesystem::path& data_root, std::uint16_t* out_port,
               std::string* error) {
    const auto path = data_root / "registry.json";
    std::ifstream input(path);
    if (!input) {
        *error = "宿主未运行（找不到 registry.json）";
        return false;
    }
    try {
        nlohmann::json document;
        input >> document;
        if (!document.contains("port") || !document["port"].is_number_unsigned()) {
            *error = "宿主未运行（registry.json 里没有端口）";
            return false;
        }
        *out_port = static_cast<std::uint16_t>(document["port"].get<unsigned>());
        return true;
    } catch (const std::exception&) {
        *error = "宿主未运行（registry.json 读不出来）";
        return false;
    }
}

bool read_token(const std::filesystem::path& data_root, std::string* out_token,
                std::string* error) {
    const auto path = data_root / "session.json";
    std::ifstream input(path);
    if (!input) {
        *error = "宿主正在启动或已退出（找不到 session.json）";
        return false;
    }
    try {
        nlohmann::json document;
        input >> document;
        if (!document.contains("token") || !document["token"].is_string()) {
            *error = "宿主正在启动或已退出（session.json 里没有口令）";
            return false;
        }
        *out_token = document["token"].get<std::string>();
        return true;
    } catch (const std::exception&) {
        *error = "宿主正在启动或已退出（session.json 读不出来）";
        return false;
    }
}

AdminReply call(const std::filesystem::path& data_root, const std::string& method,
                const std::string& args_json, int timeout_ms) {
    AdminReply reply;
    sys::socket_startup();

    std::uint16_t port = 0;
    std::string error;
    if (!read_port(data_root, &port, &error)) {
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = error;
        return reply;
    }
    std::string token;
    if (!read_token(data_root, &token, &error)) {
        // 没拿到口令算「宿主不可达」（退出码 3），拿到但不对才算认证失败（退出码 4）。
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = error;
        return reply;
    }

    const sys::socket_handle fd = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (fd == sys::bad_socket()) {
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = "无法创建套接字";
        return reply;
    }

    sys::set_socket_timeout(fd, timeout_ms);

    sockaddr_in address{};
    address.sin_family = AF_INET;
    ::inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
    address.sin_port = ::htons(port);
    if (::connect(fd, reinterpret_cast<sockaddr*>(&address), sizeof(address)) != 0) {
        sys::close_socket(fd);
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = "宿主未运行（端口 " + std::to_string(port) + " 无响应）";
        return reply;
    }

    nlohmann::json auth;
    auth["v"] = 1;
    auth["id"] = 0;
    auth["op"] = "auth";
    auth["token"] = token;
    if (!send_all(fd, auth.dump() + "\n")) {
        sys::close_socket(fd);
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = "发送认证帧失败";
        return reply;
    }
    const std::string auth_line = read_line(fd);
    if (auth_line.empty()) {
        sys::close_socket(fd);
        reply.kind = AdminReply::Kind::kAuth;
        reply.message = "认证无应答（宿主可能正在启动）";
        return reply;
    }
    try {
        const auto document = nlohmann::json::parse(auth_line);
        if (!document.value("ok", false)) {
            sys::close_socket(fd);
            reply.kind = AdminReply::Kind::kAuth;
            reply.message = "口令不匹配，宿主已重启过";
            return reply;
        }
    } catch (const std::exception&) {
        sys::close_socket(fd);
        reply.kind = AdminReply::Kind::kProtocol;
        reply.message = "认证应答不是合法 JSON";
        return reply;
    }

    nlohmann::json request;
    request["v"] = 1;
    request["id"] = 1;
    request["op"] = "call";
    request["method"] = method;
    request["args"] = args_json.empty() ? nlohmann::json::object()
                                        : nlohmann::json::parse(args_json, nullptr, false);
    if (!send_all(fd, request.dump() + "\n")) {
        sys::close_socket(fd);
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = "发送调用帧失败";
        return reply;
    }

    bool timed_out = false;
    const std::string line = read_line(fd, &timed_out);
    sys::close_socket(fd);
    if (line.empty()) {
        if (timed_out) {
            reply.kind = AdminReply::Kind::kTimeout;
            reply.message = "宿主 " + std::to_string(timeout_ms / 1000) + " 秒内没有应答";
            return reply;
        }
        reply.kind = AdminReply::Kind::kProtocol;
        reply.message = "宿主在应答前断开";
        return reply;
    }
    try {
        const auto document = nlohmann::json::parse(line);
        if (document.value("ok", false)) {
            reply.kind = AdminReply::Kind::kOk;
            reply.result = document.contains("result") ? document["result"].dump() : "null";
            return reply;
        }
        reply.kind = AdminReply::Kind::kRemote;
        if (document.contains("error")) {
            reply.code = document["error"].value("code", "E_INTERNAL");
            reply.message = document["error"].value("message", "");
        }
        return reply;
    } catch (const std::exception&) {
        reply.kind = AdminReply::Kind::kProtocol;
        reply.message = "应答不是合法 JSON";
        return reply;
    }
}

}  // namespace admin
}  // namespace ling
