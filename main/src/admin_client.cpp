// 管理端口客户端：找端口、认证、发一帧、读一帧。
// 找不到宿主与认证失败要分开报，两者排查方向不同（04/06 §6.2）。
#include "ling/admin.hpp"

#include <cstddef>
#include <fstream>
#include <string>

#include "ling/errors.hpp"

#include <nlohmann/json.hpp>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
#endif

namespace ling {
namespace admin {
namespace {

#ifdef _WIN32
using socket_t = SOCKET;
constexpr socket_t kBadSocket = INVALID_SOCKET;
void close_socket(socket_t fd) { ::closesocket(fd); }
void ensure_winsock() {
    static const bool ready = [] {
        WSADATA data{};
        return ::WSAStartup(MAKEWORD(2, 2), &data) == 0;
    }();
    (void)ready;
}
#else
using socket_t = int;
constexpr socket_t kBadSocket = -1;
void close_socket(socket_t fd) { ::close(fd); }
void ensure_winsock() {}
#endif

bool send_all(socket_t fd, const std::string& data) {
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

// 读到一行为止。返回空表示连接已断或超时。
std::string read_line(socket_t fd) {
    std::string buffer;
    char chunk[4096];
    for (;;) {
        const int received = ::recv(fd, chunk, sizeof(chunk), 0);
        if (received <= 0) {
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
    ensure_winsock();

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

    const socket_t fd = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (fd == kBadSocket) {
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = "无法创建套接字";
        return reply;
    }

#ifdef _WIN32
    const DWORD recv_timeout = static_cast<DWORD>(timeout_ms);
    ::setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO,
                 reinterpret_cast<const char*>(&recv_timeout), sizeof(recv_timeout));
    ::setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO,
                 reinterpret_cast<const char*>(&recv_timeout), sizeof(recv_timeout));
#else
    timeval tv{};
    tv.tv_sec = timeout_ms / 1000;
    tv.tv_usec = (timeout_ms % 1000) * 1000;
    ::setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    ::setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
#endif

    sockaddr_in address{};
    address.sin_family = AF_INET;
    ::inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
    address.sin_port = ::htons(port);
    if (::connect(fd, reinterpret_cast<sockaddr*>(&address), sizeof(address)) != 0) {
        close_socket(fd);
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
        close_socket(fd);
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = "发送认证帧失败";
        return reply;
    }
    const std::string auth_line = read_line(fd);
    if (auth_line.empty()) {
        close_socket(fd);
        reply.kind = AdminReply::Kind::kAuth;
        reply.message = "认证无应答（宿主可能正在启动）";
        return reply;
    }
    try {
        const auto document = nlohmann::json::parse(auth_line);
        if (!document.value("ok", false)) {
            close_socket(fd);
            reply.kind = AdminReply::Kind::kAuth;
            reply.message = "口令不匹配，宿主已重启过";
            return reply;
        }
    } catch (const std::exception&) {
        close_socket(fd);
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
        close_socket(fd);
        reply.kind = AdminReply::Kind::kUnreachable;
        reply.message = "发送调用帧失败";
        return reply;
    }

    const std::string line = read_line(fd);
    close_socket(fd);
    if (line.empty()) {
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
