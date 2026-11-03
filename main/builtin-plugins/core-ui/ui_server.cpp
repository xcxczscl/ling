// 界面进程的 HTTP 服务端实现。
//
// 四条约束决定了这里的写法：
//   1. 只绑回环，任何情况下都不对外网开放（core-ui/04-详细设计/01 §1）。
//   2. 每个动作都转成一次管理端口调用，界面自己不做业务，也不碰内核（§4）。
//   3. 单线程、select 轮询，与宿主同一套风格，不引网络库。
//   4. 免认证的地址只有「GET 一个文件或一段常量」，凡有副作用的一律要 token（§2.4）。
#include "ui_server.hpp"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <exception>
#include <filesystem>
#include <fstream>
#include <string>
#include <utility>
#include <vector>

#include "ling/admin.hpp"
#include "ling/errors.hpp"
#include "ling/sys.hpp"
#include "ling/version.hpp"

#include <nlohmann/json.hpp>

#include "src/assets.hpp"
#include "src/error_copy.hpp"
#include "src/panel_assets.hpp"
#include "src/panel_registry.hpp"
#include "src/theme.hpp"

namespace ling {
namespace ui {
namespace {

constexpr std::size_t kMaxRequestBytes = 1024 * 1024;
constexpr int kRequestTimeoutSeconds = 10;
// 两档超时分开：静态页面只是读文件，慢了就是真卡住；安装插件要解压、要跑装载检查，
// 5 秒会把它先掐掉（§9 的取舍）。
constexpr int kStaticTimeoutMs = 5000;
constexpr int kApiTimeoutMs = 30000;
constexpr int kProbeTimeoutMs = 5000;

// 外壳页面的 CSP（06 §5.1）：不引外部资源，也不许被任何页面嵌入。
constexpr const char* kShellCsp =
    "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; "
    "connect-src 'self'; frame-src 'self'; frame-ancestors 'none'; base-uri 'none'; "
    "form-action 'none'";

// 面板资源的 CSP（06 §5.2）：面板多半没配构建链，所以放行内联；
// connect-src 取 none 是第三重保险，即使浏览器放行跨源它也发不出请求。
constexpr const char* kPanelCsp =
    "default-src 'self' data: blob:; script-src 'self' 'unsafe-inline'; "
    "style-src 'self' 'unsafe-inline'; connect-src 'none'; form-action 'none'; "
    "frame-ancestors 'self'";

std::string status_text(int status) {
    switch (status) {
        case 200: return "OK";
        case 301: return "Moved Permanently";
        case 304: return "Not Modified";
        case 400: return "Bad Request";
        case 401: return "Unauthorized";
        case 403: return "Forbidden";
        case 404: return "Not Found";
        case 405: return "Method Not Allowed";
        case 409: return "Conflict";
        case 413: return "Payload Too Large";
        case 429: return "Too Many Requests";
        case 500: return "Internal Server Error";
        case 502: return "Bad Gateway";
        case 503: return "Service Unavailable";
        case 504: return "Gateway Timeout";
        default: return "OK";
    }
}

struct Request {
    std::string method;
    std::string path;     // 不含查询串
    std::string query;    // 不含问号
    std::string body;
    std::string bearer;   // Authorization: Bearer 后面的部分
    std::string if_none_match;
};

struct Response {
    int status = 200;
    std::string content_type = "application/json; charset=utf-8";
    std::string body;
    std::vector<std::pair<std::string, std::string>> headers;
    bool head_only = false;
};

void add_header(Response* response, std::string name, std::string value) {
    response->headers.emplace_back(std::move(name), std::move(value));
}

std::string build_response(const Response& response) {
    std::string out = "HTTP/1.1 " + std::to_string(response.status) + " " +
                      status_text(response.status) + "\r\n";
    out += "Content-Type: " + response.content_type + "\r\n";
    // HEAD 与 GET 的响应头必须一致，只是不带正文（§5.4 的「同 GET 的检查」）。
    out += "Content-Length: " + std::to_string(response.body.size()) + "\r\n";
    // 每次响应都断开：请求量很小，省掉连接状态机的复杂度。
    out += "Connection: close\r\n";
    out += "X-Content-Type-Options: nosniff\r\n";
    for (const auto& header : response.headers) {
        out += header.first + ": " + header.second + "\r\n";
    }
    out += "\r\n";
    if (!response.head_only) {
        out += response.body;
    }
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

std::string query_value(const std::string& query, const std::string& key) {
    std::size_t at = 0;
    while (at <= query.size()) {
        const auto end = query.find('&', at);
        const auto piece = query.substr(at, end == std::string::npos ? std::string::npos : end - at);
        const auto eq = piece.find('=');
        if (eq != std::string::npos && piece.substr(0, eq) == key) {
            return url_decode_once(piece.substr(eq + 1));
        }
        if (end == std::string::npos) {
            break;
        }
        at = end + 1;
    }
    return {};
}

std::string now_iso8601() {
    const auto now = std::chrono::system_clock::now();
    const std::time_t stamp = std::chrono::system_clock::to_time_t(now);
    const std::tm utc = sys::utc_time(&stamp);
    char buffer[32] = {};
    std::strftime(buffer, sizeof(buffer), "%Y-%m-%dT%H:%M:%SZ", &utc);
    return buffer;
}

Response fail(int status, const std::string& code, const std::string& message,
              const std::string& detail = std::string()) {
    Response response;
    response.status = status;
    response.body = error_json(code, message, detail);
    return response;
}

Response ok(const nlohmann::json& result) {
    nlohmann::json body;
    body["ok"] = true;
    body["result"] = result;
    Response response;
    response.body = body.dump();
    return response;
}

// 值的 JSON 类型名，供配置页选控件（§4 的 config.list 一行）。
const char* value_type(const nlohmann::json& value) {
    if (value.is_boolean()) {
        return "bool";
    }
    if (value.is_number()) {
        return "number";
    }
    if (value.is_string()) {
        return "string";
    }
    if (value.is_null()) {
        return "null";
    }
    return "json";
}

// 日志行切成 {ts, level, text}。core-log 存的是原文，界面要的是三段，
// 所以在这里切一次，别让每个页签各写一遍正则。
nlohmann::json split_log_line(const std::string& line) {
    nlohmann::json item;
    std::string rest = line;
    const auto space = rest.find(' ');
    if (space != std::string::npos && rest.size() > 10 && rest[4] == '-') {
        item["ts"] = rest.substr(0, space);
        rest = rest.substr(space + 1);
    } else {
        item["ts"] = std::string();
    }
    std::string level;
    if (rest.size() > 2 && rest.front() == '[') {
        const auto close = rest.find(']');
        if (close != std::string::npos) {
            level = rest.substr(1, close - 1);
            rest = rest.substr(close + 1);
            while (!rest.empty() && rest.front() == ' ') {
                rest.erase(rest.begin());
            }
        }
    }
    item["level"] = level;
    item["text"] = rest;
    return item;
}

// 宿主自己的日志没有对应插件方法（`kernel.logs` 要一个 plugin），命令行是直读
// `<数据根>/logs/kernel-*.log` 的，界面照做，保持两边看到的是同一份东西。
std::vector<std::string> tail_kernel_log(const std::filesystem::path& data_root, int lines) {
    std::vector<std::string> picked;
    const auto log_dir = data_root / "logs";
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
        // 按天分文件，文件名里的日期字典序可比，取最大的那个就是最新。
        if (latest.empty() || name > latest.filename().string()) {
            latest = entry.path();
        }
    }
    if (latest.empty()) {
        return picked;
    }
    std::ifstream input(latest, std::ios::binary);
    if (!input) {
        return picked;
    }
    std::string line;
    while (std::getline(input, line)) {
        if (!line.empty() && line.back() == '\r') {
            line.pop_back();
        }
        picked.push_back(std::move(line));
    }
    if (lines > 0 && picked.size() > static_cast<std::size_t>(lines)) {
        picked.erase(picked.begin(), picked.end() - lines);
    }
    return picked;
}

}  // namespace

struct UiServer::Impl {
    sys::socket_handle listen_fd = sys::bad_socket();
    std::uint16_t port = 0;
    Options options;
    std::string token;

    PanelRegistry registry{std::filesystem::path()};

    // 主题名有 2 秒缓存：theme.css 每个面板加载时都会拉一次，
    // 每次都去问宿主没必要；但缓存久了会让外观页切换后回退成旧值。
    std::string theme = "light";
    std::chrono::steady_clock::time_point theme_at{};
    bool theme_loaded = false;

    struct Conn {
        sys::socket_handle fd = sys::bad_socket();
        std::string buffer;
        std::chrono::steady_clock::time_point opened;
    };
    std::vector<Conn> conns;

    Response handle_request(const Request& request);

    // —— 路由的六个分支，顺序与 §3 的匹配表一致 ——
    Response handle_shell_file(const Request& request);
    Response handle_theme_css(const Request& request);
    Response handle_views(const Request& request);
    Response handle_panel(const Request& request);
    Response handle_api(const Request& request);
    Response handle_action(const std::string& action, const nlohmann::json& args);
    Response handle_logs(const nlohmann::json& args);
    Response handle_panel_call(const nlohmann::json& args);

    // 面板内存表：stale 时才刷新，只在有请求时触发。
    void ensure_panels(bool force = false);
    std::string current_theme();

    // 一次管理端口调用。成功时 result 是原文，失败时 body 已经拼好错误体。
    struct HostCall {
        bool ok = false;
        Response response;
        std::string result;
    };
    HostCall call_host(const std::string& method, const nlohmann::json& args, int timeout_ms);

    void close_conn(std::size_t index);
    // 返回 true 表示已经应答完（可以断开了）；false 表示请求还没收全，等下一轮。
    bool service_conn(Conn& conn);
};

// ---------------- 宿主适配 ----------------

UiServer::Impl::HostCall UiServer::Impl::call_host(const std::string& method,
                                                   const nlohmann::json& args,
                                                   int timeout_ms) {
    HostCall call;
    const AdminReply reply = admin::call(options.data_root, method, args.dump(), timeout_ms);
    switch (reply.kind) {
        case AdminReply::Kind::kOk:
            call.ok = true;
            call.result = reply.result;
            return call;
        case AdminReply::Kind::kUnreachable:
            call.response =
                fail(503, err::kIo, "连不上 ling 宿主，它可能已经退出");
            return call;
        case AdminReply::Kind::kAuth:
            call.response = fail(401, err::kAuth, "界面的口令与宿主不一致");
            return call;
        case AdminReply::Kind::kTimeout:
            call.response = fail(504, err::kTimeout, "");
            return call;
        case AdminReply::Kind::kProtocol:
        case AdminReply::Kind::kRemote:
        default: {
            // 面板机制那几个错误码自带语义，HTTP 状态跟着走：外壳与脚本按状态码分流，
            // 按错误码取文案（文案表见 src/error_copy.cpp）。
            int status = 400;
            if (reply.code == err::kUiForbidden) {
                status = 403;
            } else if (reply.code == err::kUiNotReady) {
                status = 409;
            } else if (reply.code == err::kUiBusy) {
                status = 429;
            } else if (reply.code == err::kUiPanelNotFound) {
                status = 404;
            }
            call.response =
                fail(status, reply.code.empty() ? err::kInternal : reply.code, reply.message);
            return call;
        }
    }
}

void UiServer::Impl::ensure_panels(bool force) {
    const auto now = std::chrono::steady_clock::now();
    if (!force && !registry.stale(now)) {
        return;
    }
    // 清单先重读（写动作之后必须），状态再套上去。
    registry.reload_manifests();
    const auto call = call_host("kernel.plugins", nlohmann::json::object(), kProbeTimeoutMs);
    if (!call.ok) {
        return;  // 宿主没响应时保留旧表，界面照旧能用
    }
    const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
    if (parsed.is_discarded() || !parsed.is_object() || !parsed.contains("plugins")) {
        return;
    }
    registry.apply_states(parsed["plugins"]);
}

std::string UiServer::Impl::current_theme() {
    const auto now = std::chrono::steady_clock::now();
    if (theme_loaded && now - theme_at < std::chrono::seconds(2)) {
        return theme;
    }
    theme_loaded = true;
    theme_at = now;

    nlohmann::json inner = {{"method", "core-settings.get"},
                            {"args", {{"key", "core-ui.theme"}}}};
    const auto call = call_host("kernel.call", inner, kProbeTimeoutMs);
    if (!call.ok) {
        return theme;
    }
    const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
    if (parsed.is_discarded() || !parsed.is_object() || !parsed.contains("value")) {
        return theme;
    }
    // 只认 light 与 dark，其余一律当 light：配置可手改，值不能信（04 §5.2）。
    const auto& value = parsed["value"];
    theme = value.is_string() && valid_theme(value.get<std::string>())
                ? value.get<std::string>()
                : "light";
    return theme;
}

// ---------------- 路由 ----------------

Response UiServer::Impl::handle_shell_file(const Request& request) {
    const bool head = request.method == "HEAD";
    const char* text = nullptr;
    const char* type = nullptr;
    if (request.path == "/") {
        text = shell_html();
        type = "text/html; charset=utf-8";
    } else if (request.path == "/shell.css") {
        text = shell_css();
        type = "text/css; charset=utf-8";
    } else if (request.path == "/shell.js") {
        text = shell_js();
        type = "text/javascript; charset=utf-8";
    }
    if (text == nullptr) {
        return fail(404, err::kProtocol, "没有这个地址：" + request.path);
    }
    Response response;
    response.content_type = type;
    response.body = text;
    response.head_only = head;
    add_header(&response, "Cache-Control", "no-cache");
    add_header(&response, "Content-Security-Policy", kShellCsp);
    // 外壳页面不许被任何页面嵌入；面板资源不加这条，否则外壳自己也嵌不进去。
    add_header(&response, "X-Frame-Options", "DENY");
    return response;
}

Response UiServer::Impl::handle_views(const Request& request) {
    const std::string rest = request.path.substr(7);  // 去掉 "/views/"
    if (rest.size() <= 3 || rest.compare(rest.size() - 3, 3, ".js") != 0) {
        return fail(404, err::kProtocol, "没有这个地址：" + request.path);
    }
    const std::string name = rest.substr(0, rest.size() - 3);
    const char* text = view_js(name);
    if (text == nullptr) {
        // 名称只允许那六个：这条防止「按名称拼路径」变成任意文件读取。
        return fail(404, err::kProtocol, "没有这个视图：" + name);
    }
    Response response;
    response.content_type = "text/javascript; charset=utf-8";
    response.body = text;
    response.head_only = request.method == "HEAD";
    add_header(&response, "Cache-Control", "no-cache");
    add_header(&response, "Content-Security-Policy", kShellCsp);
    add_header(&response, "X-Frame-Options", "DENY");
    return response;
}

Response UiServer::Impl::handle_theme_css(const Request& request) {
    Response response;
    response.content_type = "text/css; charset=utf-8";
    response.body = render_theme_css(current_theme(), now_iso8601());
    response.head_only = request.method == "HEAD";
    // 主题会变，缓存了就会用旧值。
    add_header(&response, "Cache-Control", "no-store");
    return response;
}

Response UiServer::Impl::handle_panel(const Request& request) {
    const std::string rest = request.path.substr(7);  // 去掉 "/panel/"
    const auto slash = rest.find('/');
    const std::string plugin_id = slash == std::string::npos ? rest : rest.substr(0, slash);
    std::string rel = slash == std::string::npos ? std::string() : rest.substr(slash + 1);

    // `/panel/<id>` 301 到带斜杠的形式，相对路径才算得对。
    if (slash == std::string::npos) {
        Response response;
        response.status = 301;
        add_header(&response, "Location", "/panel/" + plugin_id + "/");
        response.body = "";
        return response;
    }

    ensure_panels();
    const PanelInfo* panel = registry.find(plugin_id);
    if (panel == nullptr) {
        Response response = fail(404, err::kUiPanelNotFound, "这个插件没有面板");
        add_header(&response, "Content-Security-Policy", kPanelCsp);
        return response;
    }
    std::string default_file;
    {
        const auto cut = panel->entry_url.find_last_of('/');
        default_file = cut == std::string::npos ? panel->entry_url : panel->entry_url.substr(cut + 1);
    }

    const Asset asset = load_panel_asset(options.data_root / "plugins", plugin_id, rel,
                                        default_file, request.if_none_match, kStaticTimeoutMs);
    Response response;
    response.status = asset.status;
    response.head_only = request.method == "HEAD";
    add_header(&response, "Content-Security-Policy", kPanelCsp);
    if (asset.status == 200 || asset.status == 304) {
        response.content_type = asset.content_type;
        response.body = asset.body;
        add_header(&response, "Cache-Control", "no-cache");
        add_header(&response, "ETag", asset.etag);
        return response;
    }
    if (asset.status == 301) {
        return response;
    }
    // 404 用 JSON 体：外壳在预检里读得到原因，用户看得到「面板文件不存在：<路径>」。
    response.status = asset.status;
    response.content_type = "application/json; charset=utf-8";
    response.body = error_json(asset.error_code, asset.error_message, "");
    return response;
}

Response UiServer::Impl::handle_api(const Request& request) {
    if (request.method != "POST") {
        Response response = fail(405, err::kProtocol, "接口只接受 POST");
        add_header(&response, "Allow", "POST");
        return response;
    }
    if (options.require_auth && !token_equal(request.bearer, token)) {
        // 401 单独给，页面据此提示「口令已变，请重新从主程序打开」。
        return fail(401, err::kAuth, "口令不对或已过期");
    }

    nlohmann::json args = nlohmann::json::object();
    if (!request.body.empty()) {
        args = nlohmann::json::parse(request.body, nullptr, false);
        if (args.is_discarded() || !args.is_object()) {
            return fail(400, err::kProtocol, "请求体必须是 JSON 对象");
        }
    }

    const std::string action = request.path.substr(5);  // 去掉 "/api/"
    // 这两个在动作表之前判，否则会被当成动作名（§3 的匹配顺序）。
    if (action == "panel/call") {
        return handle_panel_call(args);
    }
    return handle_action(action, args);
}

Response UiServer::Impl::handle_panel_call(const nlohmann::json& args) {
    const std::string plugin = args.value("plugin", std::string());
    const std::string method = args.value("method", std::string());
    if (plugin.empty() || method.empty()) {
        return fail(400, err::kProtocol, "panel/call 需要 plugin 与 method");
    }
    ensure_panels();
    // 第 2 步：插件必须已装载且带 [ui]。停用后它的面板即使还开着也调不动。
    if (registry.find(plugin) == nullptr) {
        return fail(403, err::kUiForbidden, "插件 " + plugin + " 没有面板或没有装载");
    }
    // 第 3 步：能力边界的第二道闸，第一道在外壳。
    if (method.rfind(plugin + ".", 0) != 0) {
        return fail(403, err::kUiForbidden, "面板不能调用 " + method);
    }
    nlohmann::json inner = {{"plugin", plugin}, {"method", method}};
    if (args.contains("params") && !args["params"].is_null()) {
        if (!args["params"].is_object()) {
            return fail(400, err::kProtocol, "params 必须是对象");
        }
        inner["params"] = args["params"];
    }
    const auto call = call_host("kernel.panel_call", inner, kApiTimeoutMs);
    if (!call.ok) {
        return call.response;
    }
    const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
    return ok(parsed.is_discarded() ? nlohmann::json(call.result) : parsed);
}

Response UiServer::Impl::handle_action(const std::string& action, const nlohmann::json& args) {
    auto require = [&args](const char* key, std::string* out) {
        if (!args.contains(key) || !args[key].is_string()) {
            return false;
        }
        *out = args[key].get<std::string>();
        return !out->empty();
    };

    // 每个动作都转成一次管理端口调用，与命令行走同一条路（§4）。
    if (action == "plugins") {
        const auto call = call_host("kernel.plugins", nlohmann::json::object(), kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
        // 内核给的是 {plugins:[...]}，界面要的是数组。
        if (parsed.is_object() && parsed.contains("plugins")) {
            return ok(parsed["plugins"]);
        }
        return ok(parsed.is_discarded() ? nlohmann::json::array() : parsed);
    }

    if (action == "panels") {
        ensure_panels();
        nlohmann::json list = nlohmann::json::array();
        for (const auto& panel : registry.panels()) {
            nlohmann::json item;
            item["id"] = panel.id;
            item["title"] = panel.title;
            item["icon"] = panel.icon_url.empty() ? nlohmann::json(nullptr)
                                                  : nlohmann::json(panel.icon_url);
            item["state"] = panel.state;
            if (panel.state == "failed" && !panel.reason.empty()) {
                item["reason"] = panel.reason;
            }
            item["entry"] = panel.entry_url;
            list.push_back(item);
        }
        return ok(list);
    }

    if (action == "meta") {
        nlohmann::json result;
        result["hostVersion"] = ling::version_string();
        result["uiVersion"] = ui_version();
        result["dataRoot"] = options.data_root.string();
#if defined(LING_CORE_UI_DESKTOP_BUILD)
        // 「关窗口时退出 ling」只在桌面版有效：浏览器标签页没有可靠的关闭事件。
        result["desktop"] = true;
#else
        result["desktop"] = false;
#endif
        return ok(result);
    }

    if (action == "install") {
        std::string path;
        if (!require("path", &path)) {
            return fail(400, err::kProtocol, "缺少参数 path");
        }
        const auto call = call_host("kernel.install", {{"path", path}}, kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        registry.mark_dirty();
        const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
        nlohmann::json result;
        if (parsed.is_object()) {
            result["id"] = parsed.value("id", std::string());
            result["version"] = parsed.value("version", std::string());
        }
        return ok(result);
    }

    if (action == "uninstall") {
        std::string id;
        if (!require("id", &id)) {
            return fail(400, err::kProtocol, "缺少参数 id");
        }
        const auto call = call_host("kernel.uninstall",
                                    {{"id", id}, {"purge", args.value("purge", false)}},
                                    kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        registry.mark_dirty();
        return ok({{"removed", true}});
    }

    if (action == "enable" || action == "disable") {
        std::string id;
        if (!require("id", &id)) {
            return fail(400, err::kProtocol, "缺少参数 id");
        }
        const auto call = call_host("kernel." + action, {{"id", id}}, kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        registry.mark_dirty();
        return ok({{"state", action == "enable" ? "running" : "stopped"}});
    }

    if (action == "logs") {
        return handle_logs(args);
    }

    if (action == "config.list") {
        std::string prefix;
        if (!require("prefix", &prefix)) {
            return fail(400, err::kProtocol, "缺少参数 prefix");
        }
        nlohmann::json inner = {{"method", "core-settings.list"}, {"args", {{"prefix", prefix}}}};
        const auto keys_call = call_host("kernel.call", inner, kApiTimeoutMs);
        if (!keys_call.ok) {
            return keys_call.response;
        }
        const auto keys = nlohmann::json::parse(keys_call.result, nullptr, false);
        if (keys.is_discarded() || !keys.is_object() || !keys.contains("keys")) {
            return ok({{"items", nlohmann::json::array()}});
        }
        // 逐键取一次值：list 只给键名，值按类型配控件要用。
        nlohmann::json items = nlohmann::json::array();
        for (const auto& key : keys["keys"]) {
            if (!key.is_string()) {
                continue;
            }
            nlohmann::json get_inner = {{"method", "core-settings.get"},
                                        {"args", {{"key", key.get<std::string>()}}}};
            const auto get_call = call_host("kernel.call", get_inner, kApiTimeoutMs);
            if (!get_call.ok) {
                continue;
            }
            const auto value = nlohmann::json::parse(get_call.result, nullptr, false);
            if (value.is_discarded() || !value.is_object()) {
                continue;
            }
            nlohmann::json item;
            item["key"] = key;
            item["value"] = value.contains("value") ? value["value"] : nlohmann::json(nullptr);
            item["type"] = value_type(item["value"]);
            items.push_back(item);
        }
        return ok({{"items", items}});
    }

    if (action == "config.get") {
        std::string key;
        if (!require("key", &key)) {
            return fail(400, err::kProtocol, "缺少参数 key");
        }
        nlohmann::json inner = {{"method", "core-settings.get"}, {"args", {{"key", key}}}};
        const auto call = call_host("kernel.call", inner, kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
        const auto value = parsed.is_object() && parsed.contains("value") ? parsed["value"]
                                                                         : nlohmann::json(nullptr);
        return ok({{"key", key}, {"value", value}, {"type", value_type(value)}});
    }

    if (action == "config.set") {
        std::string key;
        if (!require("key", &key)) {
            return fail(400, err::kProtocol, "缺少参数 key");
        }
        if (!args.contains("value")) {
            return fail(400, err::kProtocol, "缺少参数 value");
        }
        // plugin 由键前缀决定，不采信请求体里的 plugin：两处不一致时以键为准，
        // 否则界面传错名字就能写到别人的键空间里。
        const auto dot = key.find('.');
        if (dot == std::string::npos) {
            return fail(400, err::kNamespaceViolation, "键名要写成「插件名.键名」");
        }
        nlohmann::json inner = {{"plugin", key.substr(0, dot)},
                                {"key", key},
                                {"value", args["value"]}};
        const auto call = call_host("kernel.call",
                                    {{"method", "core-settings.write"}, {"args", inner}},
                                    kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        // 主题变了要让 theme.css 立刻反映出来。
        if (key == "core-ui.theme") {
            theme_loaded = false;
        }
        return ok({{"saved", true}});
    }

    if (action == "jobs.list") {
        nlohmann::json inner = {{"method", "core-job.list"}, {"args", nlohmann::json::object()}};
        const auto call = call_host("kernel.call", inner, kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
        nlohmann::json jobs = nlohmann::json::array();
        if (parsed.is_object() && parsed.contains("jobs") && parsed["jobs"].is_array()) {
            for (const auto& job : parsed["jobs"]) {
                nlohmann::json item = job;
                // 内核字段与界面字段对不上时在这一处换算，别让页签各写一套。
                if (job.contains("last_fired")) {
                    item["last_run"] = job["last_fired"];
                    item.erase("last_fired");
                }
                if (!item.contains("params")) {
                    item["params"] = nlohmann::json(nullptr);
                }
                jobs.push_back(item);
            }
        }
        return ok({{"jobs", jobs}});
    }

    if (action == "jobs.schedule") {
        std::string name;
        std::string cron;
        std::string method;
        if (!require("name", &name) || !require("cron", &cron) || !require("method", &method)) {
            return fail(400, err::kProtocol, "jobs.schedule 需要 name、cron、method");
        }
        nlohmann::json inner = {{"name", name}, {"cron", cron}, {"method", method}};
        if (args.contains("params")) {
            inner["args"] = args["params"];
        }
        if (args.contains("catch_up")) {
            inner["catch_up"] = args["catch_up"];
        }
        const auto call = call_host(
            "kernel.call", {{"method", "core-job.schedule"}, {"args", inner}}, kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
        return ok({{"next_run", parsed.is_object() && parsed.contains("next_run")
                                     ? parsed["next_run"]
                                     : nlohmann::json(nullptr)}});
    }

    if (action == "jobs.setEnabled") {
        std::string name;
        if (!require("name", &name) || !args.contains("enabled") || !args["enabled"].is_boolean()) {
            return fail(400, err::kProtocol, "jobs.setEnabled 需要 name 与 enabled");
        }
        const bool enabled = args["enabled"].get<bool>();
        const std::string method = enabled ? "core-job.enable" : "core-job.disable";
        const auto call = call_host(
            "kernel.call", {{"method", method}, {"args", {{"name", name}}}}, kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        return ok({{"enabled", enabled}});
    }

    if (action == "jobs.validate") {
        std::string cron;
        if (!require("cron", &cron)) {
            return fail(400, err::kProtocol, "缺少参数 cron");
        }
        const auto call = call_host("kernel.call",
                                    {{"method", "core-job.validate"}, {"args", {{"cron", cron}}}},
                                    kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
        return ok(parsed.is_discarded() ? nlohmann::json::object() : parsed);
    }

    if (action == "jobs.cancel") {
        std::string name;
        if (!require("name", &name)) {
            return fail(400, err::kProtocol, "缺少参数 name");
        }
        const auto call = call_host("kernel.call",
                                    {{"method", "core-job.cancel"}, {"args", {{"name", name}}}},
                                    kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        return ok({{"removed", true}});
    }

    if (action == "orphans") {
        const auto call = call_host(
            "kernel.call",
            {{"method", "core-plugin-manager.orphans"}, {"args", nlohmann::json::object()}},
            kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
        if (parsed.is_object() && parsed.contains("orphans")) {
            return ok(parsed["orphans"]);
        }
        return ok(parsed.is_discarded() ? nlohmann::json::array() : parsed);
    }

    if (action == "shutdown") {
        const auto call = call_host("kernel.shutdown", nlohmann::json::object(), kApiTimeoutMs);
        if (!call.ok) {
            return call.response;
        }
        return ok(nlohmann::json::object());
    }

    return fail(404, err::kProtocol, "没有这个接口：" + action);
}

Response UiServer::Impl::handle_logs(const nlohmann::json& args) {
    const std::string plugin = args.value("plugin", std::string());
    const std::string level = args.value("level", std::string());
    int lines = args.value("lines", 200);
    if (lines <= 0) {
        lines = 200;
    }
    if (lines > 1000) {
        lines = 1000;
    }
    const int before = std::clamp(args.value("before", 0), 0, 10000);

    // 不给 plugin 就是看 ling 自己的日志，跟命令行的 `ling logs` 一样。
    if (plugin.empty()) {
        auto raw = tail_kernel_log(options.data_root, lines + (before > 0 ? before : 0));
        if (before > 0 && static_cast<std::size_t>(before) < raw.size()) {
            raw.resize(raw.size() - static_cast<std::size_t>(before));
        }
        nlohmann::json out = nlohmann::json::array();
        for (const auto& line : raw) {
            nlohmann::json item = split_log_line(line);
            if (!level.empty() && item.value("level", std::string()) != level) {
                continue;
            }
            out.push_back(item);
        }
        return ok({{"lines", out}});
    }

    // 走内核的 kernel.logs，不直接调 core-log：管理端口只肯代理管理类的插件方法，
    // core-log 不在那张白名单上（docs/ling/main/04-详细设计/04 §4.5）。
    // 条数、级别过滤、上翻偏移都由内核转给 core-log，界面看到的还是同一份文本。
    nlohmann::json inner = {{"plugin", plugin}, {"lines", lines}};
    if (!level.empty()) {
        inner["level"] = level;
    }
    if (before > 0) {
        inner["before"] = before;
    }
    const auto call = call_host("kernel.logs", inner, kApiTimeoutMs);
    if (!call.ok) {
        return call.response;
    }
    const auto parsed = nlohmann::json::parse(call.result, nullptr, false);
    nlohmann::json out = nlohmann::json::array();
    if (!parsed.is_object() || !parsed.contains("lines") || !parsed["lines"].is_array()) {
        return ok({{"lines", out}});
    }
    for (const auto& line : parsed["lines"]) {
        if (line.is_string()) {
            out.push_back(split_log_line(line.get<std::string>()));
        }
    }
    return ok({{"lines", out}});
}

Response UiServer::Impl::handle_request(const Request& request) {
    const std::string& path = request.path;
    const bool get_like = request.method == "GET" || request.method == "HEAD";

    // 1. 三个固定文件。
    if (path == "/" || path == "/shell.css" || path == "/shell.js") {
        if (!get_like) {
            Response response = fail(405, err::kProtocol, "这个地址只接受 GET");
            add_header(&response, "Allow", "GET, HEAD");
            return response;
        }
        return handle_shell_file(request);
    }
    // 2. theme.css：在 /api/* 之前判，否则会被当成动作名 ui/theme.css。
    if (path == "/api/ui/theme.css") {
        if (!get_like) {
            Response response = fail(405, err::kProtocol, "这个地址只接受 GET");
            add_header(&response, "Allow", "GET, HEAD");
            return response;
        }
        return handle_theme_css(request);
    }
    // 3、4. /api/*（含 panel/call）。
    if (path.rfind("/api/", 0) == 0) {
        return handle_api(request);
    }
    // 5. /views/<名称>.js
    if (path.rfind("/views/", 0) == 0) {
        if (!get_like) {
            Response response = fail(405, err::kProtocol, "这个地址只接受 GET");
            add_header(&response, "Allow", "GET, HEAD");
            return response;
        }
        return handle_views(request);
    }
    // 6. /panel/<id>/<路径>
    if (path.rfind("/panel/", 0) == 0) {
        if (!get_like) {
            Response response = fail(405, err::kProtocol, "这个地址只接受 GET");
            add_header(&response, "Allow", "GET, HEAD");
            return response;
        }
        return handle_panel(request);
    }
    return fail(404, err::kProtocol, "没有这个地址：" + path);
}

// ---------------- 连接与服务 ----------------

void UiServer::Impl::close_conn(std::size_t index) {
    sys::close_socket(conns[index].fd);
    conns.erase(conns.begin() + static_cast<std::ptrdiff_t>(index));
}

bool UiServer::Impl::service_conn(Conn& conn) {
    const auto header_end = conn.buffer.find("\r\n\r\n");
    if (header_end == std::string::npos) {
        if (conn.buffer.size() > 16 * 1024) {
            const auto response =
                build_response(fail(413, err::kPayloadTooLarge, "请求头过长"));
            ::send(conn.fd, response.data(), static_cast<int>(response.size()), 0);
            conn.buffer.clear();
            return true;
        }
        return false;
    }

    // 先看长度，再决定这一轮能不能处理完。
    std::size_t content_length = 0;
    std::string head = conn.buffer.substr(0, header_end);
    {
        std::size_t at = 0;
        while (at < head.size()) {
            const auto end = head.find("\r\n", at);
            const auto line =
                head.substr(at, end == std::string::npos ? std::string::npos : end - at);
            const auto colon = line.find(':');
            if (colon != std::string::npos) {
                std::string name = line.substr(0, colon);
                for (auto& ch : name) {
                    ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
                }
                if (name == "content-length") {
                    try {
                        content_length = static_cast<std::size_t>(std::stoul(line.substr(colon + 1)));
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
        const auto response =
            build_response(fail(413, err::kPayloadTooLarge, "请求体超过 1 MB"));
        ::send(conn.fd, response.data(), static_cast<int>(response.size()), 0);
        conn.buffer.clear();
        return true;
    }
    if (conn.buffer.size() < header_end + 4 + content_length) {
        return false;  // 请求体还没收完
    }

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
            const auto response = build_response(fail(400, err::kProtocol, "请求行不合法"));
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
            const auto header =
                head.substr(at, end == std::string::npos ? std::string::npos : end - at);
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
                } else if (name == "if-none-match") {
                    request.if_none_match = value;
                }
            }
            if (end == std::string::npos) {
                break;
            }
            at = end + 2;
        }
    }

    // 外壳页面第一次打开时用 ?token= 带口令（浏览器导航发不出自定义头），
    // 之后由脚本存进 sessionStorage 并改成 Bearer 头。这里两种都收。
    if (request.bearer.empty()) {
        const std::string from_query = query_value(request.query, "token");
        if (!from_query.empty()) {
            request.bearer = from_query;
        }
    }

    Response response;
    try {
        response = handle_request(request);
    } catch (const std::exception&) {
        // 参数类型不符等解析异常只应影响这一条请求，不能让界面进程退出。
        response = fail(400, err::kProtocol, "请求参数不合法");
    }
    const std::string wire = build_response(response);
    ::send(conn.fd, wire.data(), static_cast<int>(wire.size()), 0);
    return true;
}

const char* ui_version() { return "1.1.0-b1"; }

UiServer::UiServer() : impl_(std::make_unique<Impl>()) {}

UiServer::~UiServer() { stop(); }

std::string UiServer::start(const Options& options) {
    sys::socket_startup();
    impl_->options = options;
    impl_->token = options.token;
    impl_->registry = PanelRegistry(options.data_root / "plugins");

    impl_->listen_fd = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (impl_->listen_fd == sys::bad_socket()) {
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
        sys::close_socket(impl_->listen_fd);
        impl_->listen_fd = sys::bad_socket();
        return "端口 " + std::to_string(options.start_port) + " 至 " +
               std::to_string(options.last_port) + " 全部被占用";
    }
    if (::listen(impl_->listen_fd, 8) != 0) {
        sys::close_socket(impl_->listen_fd);
        impl_->listen_fd = sys::bad_socket();
        return "监听失败";
    }
    return {};
}

std::uint16_t UiServer::port() const { return impl_->port; }

void UiServer::poll(int timeout_ms) {
    if (impl_->listen_fd == sys::bad_socket()) {
        return;
    }
    fd_set readable;
    FD_ZERO(&readable);
    FD_SET(impl_->listen_fd, &readable);
    sys::socket_handle max_fd = impl_->listen_fd;
    for (const auto& conn : impl_->conns) {
        FD_SET(conn.fd, &readable);
        if (conn.fd > max_fd) {
            max_fd = conn.fd;
        }
    }

    const int ready = sys::socket_select(max_fd, &readable, timeout_ms);
    if (ready < 0) {
        return;
    }

    if (FD_ISSET(impl_->listen_fd, &readable)) {
        sockaddr_in peer{};
        socklen_t peer_len = sizeof(peer);
        const sys::socket_handle fd =
            ::accept(impl_->listen_fd, reinterpret_cast<sockaddr*>(&peer), &peer_len);
        if (fd != sys::bad_socket()) {
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

void UiServer::set_require_auth(bool value) { impl_->options.require_auth = value; }

void UiServer::stop() {
    for (auto& conn : impl_->conns) {
        sys::close_socket(conn.fd);
    }
    impl_->conns.clear();
    if (impl_->listen_fd != sys::bad_socket()) {
        sys::close_socket(impl_->listen_fd);
        impl_->listen_fd = sys::bad_socket();
    }
}

}  // namespace ui
}  // namespace ling
