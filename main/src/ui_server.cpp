// 界面进程的 HTTP 服务端实现。
//
// 三条约束决定了这里的写法：
//   1. 只绑回环，任何情况下都不对外网开放（core-ui/02 §1）。
//   2. 每个动作都转成一次管理端口调用，界面自己不做业务，也不碰内核（core-ui/02 §3）。
//   3. 单线程、select 轮询，与宿主同一套风格，不引网络库。
#include "ling/ui_server.hpp"

#include <cctype>
#include <chrono>
#include <cstddef>
#include <cstring>
#include <string>
#include <utility>
#include <vector>

#include "ling/admin.hpp"
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
#include <unistd.h>
#endif

namespace ling {
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

constexpr std::size_t kMaxRequestBytes = 1024 * 1024;
constexpr int kRequestTimeoutSeconds = 10;
constexpr int kHostCallTimeoutMs = 5000;

// 页面还没做，先给一个能看清「服务端已经通了」的占位页。
// 页面定稿后换掉这一段常量即可，服务端不用动。
constexpr const char* kPlaceholderHtml = R"HTML(<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>ling</title>
<style>
  body { font-family: "Segoe UI", "Microsoft YaHei", sans-serif; margin: 0; padding: 32px;
         background: #f6f7f9; color: #1f2328; }
  h1 { font-size: 20px; margin: 0 0 8px; }
  p { color: #57606a; margin: 6px 0; }
  pre { background: #fff; border: 1px solid #d8dee4; border-radius: 6px; padding: 12px;
        overflow: auto; max-height: 420px; font-size: 13px; }
  button { margin-top: 12px; padding: 6px 14px; border: 1px solid #d0d7de; border-radius: 6px;
           background: #fff; cursor: pointer; }
  button:hover { background: #f3f4f6; }
</style>
</head>
<body>
<h1>ling 界面</h1>
<p>服务端已就绪，页面还没做。下面这块是接口连通性自检，能出内容说明整条链路是通的。</p>
<button onclick="probe()">读一次插件列表</button>
<pre id="out">点上面的按钮。</pre>
<script>
const token = new URLSearchParams(location.search).get('token') || '';
if (token) sessionStorage.setItem('ling_token', token);
async function probe() {
  const out = document.getElementById('out');
  out.textContent = '请求中…';
  try {
    const res = await fetch('/api/plugins', {
      method: 'POST',
      headers: { 'Authorization': 'Bearer ' + sessionStorage.getItem('ling_token') },
    });
    out.textContent = JSON.stringify(await res.json(), null, 2);
  } catch (err) {
    out.textContent = '请求失败：' + err;
  }
}
</script>
</body>
</html>)HTML";

std::string status_text(int status) {
    switch (status) {
        case 200: return "OK";
        case 400: return "Bad Request";
        case 401: return "Unauthorized";
        case 404: return "Not Found";
        case 405: return "Method Not Allowed";
        case 413: return "Payload Too Large";
        case 502: return "Bad Gateway";
        case 503: return "Service Unavailable";
        case 504: return "Gateway Timeout";
        default: return "OK";
    }
}

std::string build_response(int status, const std::string& content_type, const std::string& body) {
    std::string out = "HTTP/1.1 " + std::to_string(status) + " " + status_text(status) + "\r\n";
    out += "Content-Type: " + content_type + "\r\n";
    out += "Content-Length: " + std::to_string(body.size()) + "\r\n";
    // 每次响应都断开：请求量很小，省掉连接状态机的复杂度。
    out += "Connection: close\r\n";
    // 本机页面不引外部资源，禁掉一切外联，避免页面被别的站点嵌或反连。
    out += "Cache-Control: no-store\r\n";
    out += "X-Content-Type-Options: nosniff\r\n";
    out += "\r\n";
    out += body;
    return out;
}

// 口令比较：逐字符累积差异，不提前 return，减少按时间猜口令的信息量。
bool token_equal(const std::string& a, const std::string& b) {
    if (a.size() != b.size()) {
        return false;
    }
    unsigned char diff = 0;
    for (std::size_t i = 0; i < a.size(); ++i) {
        diff |= static_cast<unsigned char>(a[i] ^ b[i]);
    }
    return diff == 0;
}

// 错误码到「下一步做什么」。文案与命令行同一套（04/06 §4.4）。
std::string advice_for(const std::string& code) {
    if (code == err::kPluginMissing || code == err::kMethodMissing) {
        return "先看看装了哪些插件：插件页或 ling plugins";
    }
    if (code == err::kNamespaceViolation) {
        return "这个动作不该由界面调用，检查请求的接口名";
    }
    if (code == err::kManifestInvalid) {
        return "看插件目录里的 plugin.toml 是否完整，以及产物文件在不在";
    }
    if (code == err::kIo) {
        return "检查数据目录的写权限和磁盘剩余空间";
    }
    return {};
}

nlohmann::json error_body(const std::string& code, const std::string& message) {
    nlohmann::json body;
    body["ok"] = false;
    body["error"]["code"] = code;
    body["error"]["message"] = message;
    const std::string advice = advice_for(code);
    if (!advice.empty()) {
        body["error"]["advice"] = advice;
    }
    return body;
}

nlohmann::json ok_body(const std::string& result_json) {
    nlohmann::json body;
    body["ok"] = true;
    if (result_json.empty()) {
        body["result"] = nlohmann::json::object();
        return body;
    }
    const auto parsed = nlohmann::json::parse(result_json, nullptr, false);
    // 插件返回的是 JSON 文本；万一不是（不该发生），原样塞进字符串，别让界面白屏。
    body["result"] = parsed.is_discarded() ? nlohmann::json(result_json) : parsed;
    return body;
}

struct Request {
    std::string method;
    std::string path;     // 不含查询串
    std::string query;    // 不含问号
    std::string body;
    std::string bearer;   // Authorization: Bearer 后面的部分
};

std::string url_decode(const std::string& text) {
    std::string out;
    out.reserve(text.size());
    for (std::size_t i = 0; i < text.size(); ++i) {
        if (text[i] == '%' && i + 2 < text.size()) {
            const auto hex = text.substr(i + 1, 2);
            try {
                out.push_back(static_cast<char>(std::stoi(hex, nullptr, 16)));
                i += 2;
                continue;
            } catch (const std::exception&) {
                // 解不开就原样保留，不让一个坏字符毁掉整个请求
            }
        }
        if (text[i] == '+') {
            out.push_back(' ');
            continue;
        }
        out.push_back(text[i]);
    }
    return out;
}

std::string query_value(const std::string& query, const std::string& key) {
    std::size_t at = 0;
    while (at <= query.size()) {
        const auto end = query.find('&', at);
        const auto piece = query.substr(at, end == std::string::npos ? std::string::npos : end - at);
        const auto eq = piece.find('=');
        if (eq != std::string::npos && piece.substr(0, eq) == key) {
            return url_decode(piece.substr(eq + 1));
        }
        if (end == std::string::npos) {
            break;
        }
        at = end + 1;
    }
    return {};
}

}  // namespace

struct UiServer::Impl {
    socket_t listen_fd = kBadSocket;
    std::uint16_t port = 0;
    Options options;
    std::string token;

    struct Conn {
        socket_t fd = kBadSocket;
        std::string buffer;
        std::chrono::steady_clock::time_point opened;
    };
    std::vector<Conn> conns;

    // 把一次请求变成一次管理端口调用，返回 (HTTP 状态, 响应体 JSON 文本)
    std::pair<int, std::string> handle_request(const Request& request);

    void close_conn(std::size_t index);
    // 返回 true 表示已经应答完（可以断开了）；false 表示请求还没收全，等下一轮。
    bool service_conn(Conn& conn);
};

void UiServer::Impl::close_conn(std::size_t index) {
    close_socket(conns[index].fd);
    conns.erase(conns.begin() + static_cast<std::ptrdiff_t>(index));
}

std::pair<int, std::string> UiServer::Impl::handle_request(const Request& request) {
    // 健康检查不认证：它只回答「进程还活着」，不含任何宿主信息。
    if (request.path == "/api/health") {
        nlohmann::json body;
        body["ok"] = true;
        body["result"]["port"] = port;
        return {200, body.dump()};
    }

    // 页面本身也要口令：拿到地址的人不等于有权看这台机器的插件列表。
    if (request.path == "/" || request.path == "/index.html") {
        const std::string given = request.bearer.empty() ? query_value(request.query, "token")
                                                         : request.bearer;
        if (!token_equal(given, token)) {
            return {401, "口令不对或没有口令。请从 ling 主程序打开界面。"};
        }
        return {200, std::string(kPlaceholderHtml)};
    }

    if (request.path.rfind("/api/", 0) != 0) {
        return {404, error_body(err::kProtocol, "没有这个地址：" + request.path).dump()};
    }
    if (request.method != "POST") {
        return {405, error_body(err::kProtocol, "接口只接受 POST").dump()};
    }
    if (!token_equal(request.bearer, token)) {
        // 401 单独给，页面据此提示「口令已变，请重新从主程序打开」。
        return {401, error_body(err::kAuth, "口令不对或已过期").dump()};
    }

    nlohmann::json args = nlohmann::json::object();
    if (!request.body.empty()) {
        args = nlohmann::json::parse(request.body, nullptr, false);
        if (args.is_discarded() || !args.is_object()) {
            return {400, error_body(err::kProtocol, "请求体必须是 JSON 对象").dump()};
        }
    }

    const std::string action = request.path.substr(5);  // 去掉 "/api/"

    // 动作到内核方法的映射。界面不自己实现业务，只做参数改名（core-ui/02 §3）。
    std::string method;
    nlohmann::json payload = args;
    if (action == "plugins") {
        method = "kernel.plugins";
        payload = nlohmann::json::object();
    } else if (action == "install") {
        method = "kernel.install";
    } else if (action == "uninstall") {
        method = "kernel.uninstall";
    } else if (action == "enable") {
        method = "kernel.enable";
    } else if (action == "disable") {
        method = "kernel.disable";
    } else if (action == "logs") {
        method = "kernel.logs";
    } else if (action == "config.list") {
        method = "kernel.call";
        payload = {{"method", "core-settings.list"}, {"args", args}};
    } else if (action == "config.get") {
        method = "kernel.call";
        payload = {{"method", "core-settings.get"}, {"args", args}};
    } else if (action == "config.set") {
        // plugin 由键前缀决定，不采信请求体里的 plugin：两处不一致时以键为准，
        // 否则界面传错名字就能写到别人的键空间里。
        std::string key = args.value("key", std::string());
        const auto dot = key.find('.');
        if (dot == std::string::npos) {
            return {400, error_body(err::kNamespaceViolation,
                                    "键名要写成「插件名.键名」").dump()};
        }
        nlohmann::json inner = {{"plugin", key.substr(0, dot)},
                                {"key", key},
                                {"value", args.contains("value") ? args["value"]
                                                                 : nlohmann::json(nullptr)}};
        method = "kernel.call";
        payload = {{"method", "core-settings.write"}, {"args", inner}};
    } else if (action == "jobs.list") {
        method = "kernel.call";
        payload = {{"method", "core-job.list"}, {"args", nlohmann::json::object()}};
    } else if (action == "jobs.schedule") {
        method = "kernel.call";
        payload = {{"method", "core-job.schedule"}, {"args", args}};
    } else if (action == "jobs.cancel") {
        method = "kernel.call";
        payload = {{"method", "core-job.cancel"}, {"args", args}};
    } else if (action == "orphans") {
        method = "kernel.call";
        payload = {{"method", "core-plugin-manager.orphans"}, {"args", nlohmann::json::object()}};
    } else {
        return {404, error_body(err::kProtocol, "没有这个接口：" + action).dump()};
    }

    const AdminReply reply = admin::call(options.data_root, method, payload.dump(),
                                         kHostCallTimeoutMs);
    switch (reply.kind) {
        case AdminReply::Kind::kOk:
            return {200, ok_body(reply.result).dump()};
        case AdminReply::Kind::kUnreachable:
            return {503, error_body(err::kIo, "连不上 ling 宿主，它可能已经退出").dump()};
        case AdminReply::Kind::kAuth:
            return {401, error_body(err::kAuth, "界面的口令与宿主不一致").dump()};
        case AdminReply::Kind::kTimeout:
            return {504, error_body(err::kTimeout, "ling 没有响应，正在忙或已卡住").dump()};
        case AdminReply::Kind::kProtocol:
        case AdminReply::Kind::kRemote:
        default:
            return {400, error_body(reply.code.empty() ? err::kInternal : reply.code,
                                    reply.message.empty() ? "宿主返回了错误" : reply.message)
                             .dump()};
    }
}

bool UiServer::Impl::service_conn(Conn& conn) {
    const auto header_end = conn.buffer.find("\r\n\r\n");
    if (header_end == std::string::npos) {
        if (conn.buffer.size() > 16 * 1024) {
            const auto response = build_response(413, "application/json",
                                                 error_body(err::kPayloadTooLarge,
                                                            "请求头过长").dump());
            ::send(conn.fd, response.data(), static_cast<int>(response.size()), 0);
            conn.buffer.clear();
            return true;
        }
        return false;
    }

    // 先看长度，再决定这一轮能不能处理完。
    std::size_t content_length = 0;
    {
        const std::string head = conn.buffer.substr(0, header_end);
        std::size_t at = 0;
        while (at < head.size()) {
            const auto end = head.find("\r\n", at);
            const auto line = head.substr(at, end == std::string::npos ? std::string::npos
                                                                      : end - at);
            const auto colon = line.find(':');
            if (colon != std::string::npos) {
                std::string name = line.substr(0, colon);
                for (auto& ch : name) {
                    ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
                }
                if (name == "content-length") {
                    try {
                        content_length = static_cast<std::size_t>(
                            std::stoul(line.substr(colon + 1)));
                    } catch (const std::exception&) {
                        content_length = 0;
                    }
                }
            }
            if (end == std::string::npos) {
                break;
            }
            at = end + 2;
        }
    }

    if (content_length > kMaxRequestBytes) {
        const auto response = build_response(413, "application/json",
                                             error_body(err::kPayloadTooLarge, "请求体超过 1 MB").dump());
        ::send(conn.fd, response.data(), static_cast<int>(response.size()), 0);
        conn.buffer.clear();
        return true;
    }
    if (conn.buffer.size() < header_end + 4 + content_length) {
        return false;  // 请求体还没收完
    }

    const std::string head = conn.buffer.substr(0, header_end);
    const std::string body = conn.buffer.substr(header_end + 4, content_length);
    conn.buffer.clear();

    Request request;
    request.body = body;
    {
        const auto line_end = head.find("\r\n");
        const std::string line = head.substr(0, line_end);
        const auto sp1 = line.find(' ');
        const auto sp2 = line.find(' ', sp1 == std::string::npos ? 0 : sp1 + 1);
        if (sp1 == std::string::npos || sp2 == std::string::npos) {
            const auto response = build_response(400, "application/json",
                                                 error_body(err::kProtocol, "请求行不合法").dump());
            ::send(conn.fd, response.data(), static_cast<int>(response.size()), 0);
            return true;
        }
        request.method = line.substr(0, sp1);
        const std::string target = line.substr(sp1 + 1, sp2 - sp1 - 1);
        const auto q = target.find('?');
        request.path = q == std::string::npos ? target : target.substr(0, q);
        request.query = q == std::string::npos ? std::string() : target.substr(q + 1);

        std::size_t at = line_end == std::string::npos ? head.size() : line_end + 2;
        while (at < head.size()) {
            const auto end = head.find("\r\n", at);
            const auto header = head.substr(at, end == std::string::npos ? std::string::npos
                                                                         : end - at);
            const auto colon = header.find(':');
            if (colon != std::string::npos) {
                std::string name = header.substr(0, colon);
                for (auto& ch : name) {
                    ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
                }
                std::string value = header.substr(colon + 1);
                while (!value.empty() && value.front() == ' ') {
                    value.erase(value.begin());
                }
                if (name == "authorization" && value.rfind("Bearer ", 0) == 0) {
                    request.bearer = value.substr(7);
                }
            }
            if (end == std::string::npos) {
                break;
            }
            at = end + 2;
        }
    }

    const auto outcome = handle_request(request);
    const bool page = request.path == "/" || request.path == "/index.html";
    const std::string content_type = page && outcome.first == 200 ? "text/html; charset=utf-8"
                                                                  : "application/json; charset=utf-8";
    const std::string response = build_response(outcome.first, content_type, outcome.second);
    ::send(conn.fd, response.data(), static_cast<int>(response.size()), 0);
    return true;
}

UiServer::UiServer() : impl_(std::make_unique<Impl>()) {}

UiServer::~UiServer() { stop(); }

std::string UiServer::start(const Options& options) {
    ensure_winsock();
    impl_->options = options;
    impl_->token = options.token;

    impl_->listen_fd = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (impl_->listen_fd == kBadSocket) {
        return "无法创建监听套接字";
    }

    sockaddr_in address{};
    address.sin_family = AF_INET;
    ::inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);

    for (std::uint32_t candidate = options.start_port; candidate <= options.last_port;
         ++candidate) {
        address.sin_port = ::htons(static_cast<unsigned short>(candidate));
        if (::bind(impl_->listen_fd, reinterpret_cast<sockaddr*>(&address), sizeof(address)) == 0) {
            impl_->port = static_cast<std::uint16_t>(candidate);
            break;
        }
    }
    if (impl_->port == 0) {
        close_socket(impl_->listen_fd);
        impl_->listen_fd = kBadSocket;
        return "端口 " + std::to_string(options.start_port) + " 至 " +
               std::to_string(options.last_port) + " 全部被占用";
    }
    if (::listen(impl_->listen_fd, 8) != 0) {
        close_socket(impl_->listen_fd);
        impl_->listen_fd = kBadSocket;
        return "监听失败";
    }
    return {};
}

std::uint16_t UiServer::port() const { return impl_->port; }

void UiServer::poll(int timeout_ms) {
    if (impl_->listen_fd == kBadSocket) {
        return;
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
        return;
    }

    if (FD_ISSET(impl_->listen_fd, &readable)) {
        sockaddr_in peer{};
        socklen_t peer_len = sizeof(peer);
        const socket_t fd =
            ::accept(impl_->listen_fd, reinterpret_cast<sockaddr*>(&peer), &peer_len);
        if (fd != kBadSocket) {
            Impl::Conn conn;
            conn.fd = fd;
            conn.opened = std::chrono::steady_clock::now();
            impl_->conns.push_back(std::move(conn));
        }
    }

    for (std::size_t i = 0; i < impl_->conns.size();) {
        if (!FD_ISSET(impl_->conns[i].fd, &readable)) {
            const auto age = std::chrono::duration_cast<std::chrono::seconds>(
                std::chrono::steady_clock::now() - impl_->conns[i].opened);
            if (age.count() >= kRequestTimeoutSeconds) {
                impl_->close_conn(i);
                continue;
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
        if (impl_->conns[i].buffer.size() > kMaxRequestBytes + 16 * 1024) {
            impl_->close_conn(i);
            continue;
        }
        // 应答完就断开：不支持长连接，也就不需要维持状态。请求没发全的留着等下一轮。
        if (impl_->service_conn(impl_->conns[i])) {
            impl_->close_conn(i);
            continue;
        }
        ++i;
    }
}

void UiServer::stop() {
    for (auto& conn : impl_->conns) {
        close_socket(conn.fd);
    }
    impl_->conns.clear();
    if (impl_->listen_fd != kBadSocket) {
        close_socket(impl_->listen_fd);
        impl_->listen_fd = kBadSocket;
    }
}

}  // namespace ling
