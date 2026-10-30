// 管理端口服务端：回环监听、首帧认证、jsonl 帧转发到 kernel.* 方法。
// 帧格式与认证规则见 docs/ling/04-详细设计/06-管理端口与命令行.md §2、§3。
#include "ling/admin.hpp"

#include <chrono>
#include <cstddef>
#include <cstring>
#include <fstream>
#include <random>
#include <string>
#include <vector>

#include "ling/errors.hpp"
#include "ling/kernel.hpp"
#include "ling/log.hpp"

#include <nlohmann/json.hpp>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

namespace ling {
namespace {

#ifdef _WIN32
using socket_t = SOCKET;
constexpr socket_t kBadSocket = INVALID_SOCKET;
constexpr int kProtoVersion = 1;

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
constexpr int kProtoVersion = 1;

void close_socket(socket_t fd) { ::close(fd); }
void ensure_winsock() {}
#endif

constexpr std::size_t kMaxFrameBytes = 1024 * 1024;
constexpr int kAuthTimeoutSeconds = 5;

struct Conn {
    socket_t fd = kBadSocket;
    std::string buffer;
    bool authed = false;
    std::chrono::steady_clock::time_point opened;
};

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

std::string make_token() {
    // 32 字节随机数转十六进制。std::random_device 在 MSVC 上取系统熵源，够这一段用。
    std::random_device device;
    std::string hex;
    hex.reserve(64);
    const char* digits = "0123456789abcdef";
    for (int i = 0; i < 32; ++i) {
        const auto byte = static_cast<unsigned>(device() & 0xFFu);
        hex.push_back(digits[byte >> 4]);
        hex.push_back(digits[byte & 0x0Fu]);
    }
    return hex;
}

std::string error_frame(const nlohmann::json& id, const std::string& code,
                        const std::string& message) {
    nlohmann::json frame;
    frame["v"] = kProtoVersion;
    frame["id"] = id.is_null() ? nlohmann::json(0) : id;
    frame["ok"] = false;
    frame["error"]["code"] = code;
    frame["error"]["message"] = message;
    frame["error"]["detail"] = nlohmann::json::object();
    return frame.dump() + "\n";
}

std::string ok_frame(const nlohmann::json& id, const std::string& result_json) {
    std::string frame = "{\"v\":1,\"id\":" + (id.is_null() ? std::string("0") : id.dump()) +
                        ",\"ok\":true,\"result\":";
    frame += result_json.empty() ? "null" : result_json;
    frame += "}\n";
    return frame;
}

}  // namespace

struct AdminServer::Impl {
    Kernel* kernel = nullptr;
    socket_t listen_fd = kBadSocket;
    std::uint16_t port = 0;
    std::string token;
    std::filesystem::path data_root;
    std::vector<Conn> conns;
    bool stop_requested = false;

    void close_conn(std::size_t index) {
        close_socket(conns[index].fd);
        conns.erase(conns.begin() + static_cast<std::ptrdiff_t>(index));
    }

    // 处理一行帧。返回 false 表示这条连接要断开。
    bool handle_frame(Conn& conn, const std::string& line);

    void handle_call(Conn& conn, const nlohmann::json& frame, const nlohmann::json& id);
};

bool AdminServer::Impl::handle_frame(Conn& conn, const std::string& line) {
    nlohmann::json frame;
    try {
        frame = nlohmann::json::parse(line);
    } catch (const std::exception&) {
        send_all(conn.fd, error_frame(nullptr, err::kProtocol, "帧不是合法 JSON"));
        return false;
    }
    if (!frame.is_object()) {
        send_all(conn.fd, error_frame(nullptr, err::kProtocol, "帧必须是对象"));
        return false;
    }

    const nlohmann::json id = frame.contains("id") ? frame["id"] : nlohmann::json(0);
    const int version = frame.contains("v") && frame["v"].is_number_integer()
                            ? frame["v"].get<int>()
                            : 0;
    if (version != kProtoVersion) {
        send_all(conn.fd, error_frame(id, err::kApiMismatch,
                                     "协议版本 " + std::to_string(version) + " 与宿主 " +
                                         std::to_string(kProtoVersion) + " 不同"));
        return false;
    }

    const std::string op = frame.contains("op") && frame["op"].is_string()
                               ? frame["op"].get<std::string>()
                               : std::string();

    if (!conn.authed) {
        if (op != "auth") {
            // 不透露原因：未认证连接上任何非 auth 帧都按认证失败处理。
            send_all(conn.fd, error_frame(id, err::kAuth, "需要先认证"));
            return false;
        }
        const std::string provided = frame.contains("token") && frame["token"].is_string()
                                         ? frame["token"].get<std::string>()
                                         : std::string();
        if (provided != token) {
            log_warn("admin", "认证失败：口令不匹配");
            send_all(conn.fd, error_frame(id, err::kAuth, "口令不匹配"));
            return false;
        }
        conn.authed = true;
        send_all(conn.fd, "{\"v\":1,\"ok\":true}\n");
        return true;
    }

    if (op != "call") {
        send_all(conn.fd, error_frame(id, err::kProtocol, "不认识的 op：" + op));
        return true;
    }

    const std::string method = frame.contains("method") && frame["method"].is_string()
                                   ? frame["method"].get<std::string>()
                                   : std::string();
    if (method.rfind("kernel.", 0) != 0) {
        // 管理端口只走内核方法，业务方法由插件之间互相调（04/06 §3.4）。
        send_all(conn.fd, error_frame(id, err::kNamespaceViolation,
                                     "管理端口只接受 kernel.* 方法"));
        return true;
    }

    std::string args_json;
    if (frame.contains("args")) {
        args_json = frame["args"].dump();
    }

    const Kernel::KernelReply reply = kernel->handle_kernel(method, args_json);
    if (!reply.code.empty()) {
        send_all(conn.fd, error_frame(id, reply.code, reply.message));
        return true;
    }
    send_all(conn.fd, ok_frame(id, reply.result));
    if (reply.shutdown) {
        stop_requested = true;
    }
    return true;
}

AdminServer::AdminServer(Kernel* kernel) : impl_(std::make_unique<Impl>()) {
    impl_->kernel = kernel;
    impl_->data_root = kernel->data_root();
}

AdminServer::~AdminServer() { stop(); }

std::string AdminServer::start(std::uint16_t start_port, std::uint16_t last_port) {
    ensure_winsock();

    impl_->listen_fd = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (impl_->listen_fd == kBadSocket) {
        return "无法创建监听套接字";
    }

    // 只绑回环：ling 的威胁模型里没有任何一条覆盖局域网暴露（04/06 §2.1）。
    sockaddr_in address{};
    address.sin_family = AF_INET;
    ::inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);

    for (std::uint32_t candidate = start_port; candidate <= last_port; ++candidate) {
        address.sin_port = ::htons(static_cast<unsigned short>(candidate));
        if (::bind(impl_->listen_fd, reinterpret_cast<sockaddr*>(&address), sizeof(address)) == 0) {
            impl_->port = static_cast<std::uint16_t>(candidate);
            break;
        }
    }
    if (impl_->port == 0) {
        close_socket(impl_->listen_fd);
        impl_->listen_fd = kBadSocket;
        return "端口 " + std::to_string(start_port) + " 至 " + std::to_string(last_port) +
               " 全部被占用";
    }

    if (::listen(impl_->listen_fd, 8) != 0) {
        close_socket(impl_->listen_fd);
        impl_->listen_fd = kBadSocket;
        return "监听失败";
    }

    impl_->token = make_token();
    const auto session_path = impl_->data_root / "session.json";
    nlohmann::json session;
    session["token"] = impl_->token;
    std::ofstream output(session_path, std::ios::trunc);
    if (!output) {
        return "无法写入 " + session_path.string();
    }
    output << session.dump(2) << "\n";
    output.close();
    // 已知不足：Windows 上还没把 ACL 收紧到「仅当前用户」（04/06 §2.3）。
    // 文件落在用户自己的 %APPDATA% 下，其他账户默认读不到，但同机管理员可以。

    impl_->kernel->note_admin_port(impl_->port);
    log_info("admin", "监听 127.0.0.1:" + std::to_string(impl_->port));
    return {};
}

std::uint16_t AdminServer::port() const { return impl_->port; }

bool AdminServer::poll(int timeout_ms) {
    if (impl_->listen_fd == kBadSocket) {
        return false;
    }

    fd_set readable;
    FD_ZERO(&readable);
    FD_SET(impl_->listen_fd, &readable);
    socket_t max_fd = impl_->listen_fd;
    for (const auto& conn : impl_->conns) {
        FD_SET(conn.fd, &readable);
        if (conn.fd > max_fd) {
            max_fd = conn.fd;
        }
    }

    timeval timeout{};
    timeout.tv_sec = timeout_ms / 1000;
    timeout.tv_usec = (timeout_ms % 1000) * 1000;
#ifdef _WIN32
    const int ready = ::select(0, &readable, nullptr, nullptr, &timeout);
#else
    const int ready = ::select(max_fd + 1, &readable, nullptr, nullptr, &timeout);
#endif
    if (ready < 0) {
        log_warn("admin", "select 返回错误，停止监听");
        return false;
    }

    if (FD_ISSET(impl_->listen_fd, &readable)) {
        sockaddr_in peer{};
        socklen_t peer_len = sizeof(peer);
        const socket_t fd =
            ::accept(impl_->listen_fd, reinterpret_cast<sockaddr*>(&peer), &peer_len);
        if (fd != kBadSocket) {
            Conn conn;
            conn.fd = fd;
            conn.opened = std::chrono::steady_clock::now();
            impl_->conns.push_back(std::move(conn));
        }
    }

    for (std::size_t i = 0; i < impl_->conns.size();) {
        if (!FD_ISSET(impl_->conns[i].fd, &readable)) {
            // 认证超时：连上 5 秒还没通过认证就断开（04/06 §2.4）。
            if (!impl_->conns[i].authed) {
                const auto age = std::chrono::duration_cast<std::chrono::seconds>(
                    std::chrono::steady_clock::now() - impl_->conns[i].opened);
                if (age.count() >= kAuthTimeoutSeconds) {
                    log_warn("admin", "连接 5 秒内未认证，断开");
                    impl_->close_conn(i);
                    continue;
                }
            }
            ++i;
            continue;
        }

        char buffer[4096];
        const int received = ::recv(impl_->conns[i].fd, buffer, sizeof(buffer), 0);
        if (received <= 0) {
            impl_->close_conn(i);
            continue;
        }
        impl_->conns[i].buffer.append(buffer, static_cast<std::size_t>(received));
        if (impl_->conns[i].buffer.size() > kMaxFrameBytes) {
            send_all(impl_->conns[i].fd,
                     error_frame(nullptr, err::kPayloadTooLarge, "单帧超过 1 MB"));
            impl_->close_conn(i);
            continue;
        }

        bool keep = true;
        std::size_t newline = impl_->conns[i].buffer.find('\n');
        while (keep && newline != std::string::npos) {
            const std::string line = impl_->conns[i].buffer.substr(0, newline);
            impl_->conns[i].buffer.erase(0, newline + 1);
            if (!line.empty()) {
                keep = impl_->handle_frame(impl_->conns[i], line);
            }
            newline = impl_->conns[i].buffer.find('\n');
        }
        if (!keep) {
            impl_->close_conn(i);
            continue;
        }
        ++i;
    }

    if (impl_->stop_requested) {
        stop();
        return false;
    }
    return true;
}

void AdminServer::stop() {
    for (auto& conn : impl_->conns) {
        close_socket(conn.fd);
    }
    impl_->conns.clear();
    if (impl_->listen_fd != kBadSocket) {
        close_socket(impl_->listen_fd);
        impl_->listen_fd = kBadSocket;
    }
    if (!impl_->token.empty()) {
        std::error_code ec;
        std::filesystem::remove(impl_->data_root / "session.json", ec);
        impl_->token.clear();
    }
}

}  // namespace ling
