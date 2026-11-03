// 面板静态资源。见 panel_assets.hpp。
#include "panel_assets.hpp"

#include <chrono>
#include <cctype>
#include <cstdio>
#include <fstream>
#include <system_error>
#include <vector>

#include "ling/errors.hpp"

namespace ling {
namespace ui {
namespace {

// 后缀白名单（§5.2）。白名单之外的文件即使存在也不返回：
// 这条挡的是「把插件目录里的 plugin.toml、数据文件、日志当静态资源读出去」。
struct TypeRow {
    const char* suffix;
    const char* mime;
};

const TypeRow kTypes[] = {
    {".html", "text/html; charset=utf-8"},   {".css", "text/css; charset=utf-8"},
    {".js", "text/javascript; charset=utf-8"}, {".mjs", "text/javascript; charset=utf-8"},
    {".json", "application/json; charset=utf-8"},
    {".svg", "image/svg+xml"},               {".png", "image/png"},
    {".jpg", "image/jpeg"},                  {".jpeg", "image/jpeg"},
    {".webp", "image/webp"},                 {".gif", "image/gif"},
    {".ico", "image/x-icon"},                {".woff", "font/woff"},
    {".woff2", "font/woff2"},                {".ttf", "font/ttf"},
};

const char* mime_for(const std::string& path) {
    const auto dot = path.find_last_of('.');
    if (dot == std::string::npos) {
        return nullptr;
    }
    std::string suffix = path.substr(dot);
    for (auto& ch : suffix) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    for (const auto& row : kTypes) {
        if (suffix == row.suffix) {
            return row.mime;
        }
    }
    return nullptr;
}

// 单个面板文件的上限。面板是静态资源，超过这个量级多半是放错了东西（日志、数据库、构建产物），
// 与其把它整个读进内存，不如直接回 413。
constexpr std::size_t kMaxPanelAssetBytes = 8 * 1024 * 1024;

// If-None-Match 可能带弱校验前缀、引号外空白或一串候选值（RFC 9110 §8.8.3）。
// 我们自己的 ETag 是强校验，取第一个候选并去掉 W/ 前缀后按原文比较即可。
std::string normalize_etag(std::string value) {
    const auto comma = value.find(',');
    if (comma != std::string::npos) {
        value.resize(comma);
    }
    while (!value.empty() && (value.front() == ' ' || value.front() == '\t')) {
        value.erase(value.begin());
    }
    while (!value.empty() && (value.back() == ' ' || value.back() == '\t')) {
        value.pop_back();
    }
    if (value.rfind("W/", 0) == 0) {
        value.erase(0, 2);
    }
    return value;
}

// 真实路径的目录前缀比较。只做字符串前缀挡不住符号链接，
// 所以两侧都先用 weakly_canonical 解析（ui/ 里可能放着指向 ../../ 的链接）。
bool inside(const std::filesystem::path& root, const std::filesystem::path& target) {
    auto it_root = root.begin();
    auto it_target = target.begin();
    for (; it_root != root.end(); ++it_root, ++it_target) {
        if (it_target == target.end() || *it_root != *it_target) {
            return false;
        }
    }
    return true;
}

Asset not_found(const std::string& message) {
    Asset asset;
    asset.status = 404;
    asset.error_code = err::kUiPanelNotFound;
    asset.error_message = message;
    return asset;
}

}  // namespace

bool valid_plugin_id(const std::string& id) {
    if (id.empty()) {
        return false;
    }
    if (!(id[0] >= 'a' && id[0] <= 'z') && !(id[0] >= '0' && id[0] <= '9')) {
        return false;
    }
    for (const char ch : id) {
        const bool ok = (ch >= 'a' && ch <= 'z') || (ch >= '0' && ch <= '9') || ch == '-';
        if (!ok) {
            return false;
        }
    }
    return true;
}

std::string url_decode_once(const std::string& text) {
    std::string out;
    out.reserve(text.size());
    for (std::size_t i = 0; i < text.size(); ++i) {
        if (text[i] == '%' && i + 2 < text.size()) {
            const auto hex = text.substr(i + 1, 2);
            const bool ok = hex.find_first_not_of("0123456789abcdefABCDEF") == std::string::npos;
            if (ok) {
                out.push_back(static_cast<char>(std::stoi(hex, nullptr, 16)));
                i += 2;
                continue;
            }
        }
        out.push_back(text[i]);
    }
    return out;
}

Asset load_panel_asset(const std::filesystem::path& plugins_dir, const std::string& plugin_id,
                       const std::string& rel_path, const std::string& default_file,
                       const std::string& if_none_match, int timeout_ms) {
    // 插件 id 先校验：用 id 段做穿越是最省事的一种攻击。
    if (!valid_plugin_id(plugin_id)) {
        return not_found("插件名不合法");
    }

    // 第 1 道：解码一次后不许有 % 残留（挡二次编码）。
    std::string path = url_decode_once(rel_path);
    if (path.find('%') != std::string::npos) {
        return not_found("路径里有无法解码的字符");
    }
    // 目录请求补上 entry 的文件名；`/panel/<id>/` 与 `/panel/<id>` 都归到这里。
    if (path.empty() || path.back() == '/') {
        if (default_file.empty()) {
            return not_found("这个插件没有面板");
        }
        path += default_file;
    }

    // 第 2 道：按 / 切段，不许 `..`、不许空段、不许以 `.` 开头。
    {
        std::size_t at = 0;
        while (at <= path.size()) {
            const auto end = path.find('/', at);
            const std::string segment =
                path.substr(at, end == std::string::npos ? std::string::npos : end - at);
            if (segment.empty() || segment == ".." || segment.front() == '.') {
                return not_found("路径不合法");
            }
            if (end == std::string::npos) {
                break;
            }
            at = end + 1;
        }
    }

    // 第 3 道：不以 / 或 \ 开头、不含 :（挡 Windows 盘符）。
    if (path.front() == '/' || path.front() == '\\' || path.find(':') != std::string::npos) {
        return not_found("路径不合法");
    }

    const auto plugin_dir = plugins_dir / plugin_id;
    std::error_code ec;
    if (!std::filesystem::is_directory(plugin_dir, ec)) {
        return not_found("插件没有安装");
    }

    // 第 4 道：解析真实路径后比前缀。
    const auto ui_root = std::filesystem::weakly_canonical(plugin_dir / "ui", ec);
    if (ec) {
        return not_found("这个插件没有面板");
    }
    const auto target = std::filesystem::weakly_canonical(plugin_dir / "ui" / path, ec);
    if (ec || !inside(ui_root, target)) {
        return not_found("路径不在插件的 ui/ 目录里");
    }

    // 后缀白名单在取文件之前判：白名单外的文件一律不碰。
    const char* mime = mime_for(path);
    if (mime == nullptr) {
        return not_found("这个后缀不作为静态资源提供");
    }

    // 第 5 道：普通文件。目录、设备、管道都在这里被挡下。
    if (!std::filesystem::is_regular_file(target, ec)) {
        return not_found("面板文件不存在：" + path);
    }

    const auto size = std::filesystem::file_size(target, ec);
    if (ec) {
        return not_found("读不到面板文件：" + path);
    }
    if (size > kMaxPanelAssetBytes) {
        Asset too_large;
        too_large.status = 413;
        too_large.error_code = err::kPayloadTooLarge;
        too_large.error_message = "面板文件超过 8 MB 上限：" + path;
        return too_large;
    }
    const auto write_time = std::filesystem::last_write_time(target, ec);
    const auto stamp = ec ? 0 : static_cast<long long>(write_time.time_since_epoch().count());

    char etag[64] = {};
    std::snprintf(etag, sizeof(etag), "\"%llx-%llx\"", static_cast<unsigned long long>(size),
                  static_cast<unsigned long long>(stamp));

    Asset asset;
    asset.content_type = mime;
    asset.etag = etag;
    if (!if_none_match.empty() && normalize_etag(if_none_match) == asset.etag) {
        asset.status = 304;
        return asset;
    }

    // 分块读，每块之间看一次表。注意：阻塞的 read 没法从这个单线程里打断，
    // 所以这个上限只能保证「到点后最多再等一次阻塞读」，不是硬性的 504。
    // 慢盘（断开的网络共享）上仍可能长时间停在这里，这是当前实现的已知限制（§9）。
    std::ifstream input(target, std::ios::binary);
    if (!input) {
        Asset failed;
        failed.status = 500;
        failed.error_code = err::kIo;
        failed.error_message = "读不到面板文件：" + path;
        return failed;
    }
    const auto started = std::chrono::steady_clock::now();
    std::vector<char> buffer(64 * 1024);
    std::string body;
    body.reserve(static_cast<std::size_t>(size));
    while (input) {
        input.read(buffer.data(), static_cast<std::streamsize>(buffer.size()));
        const auto got = input.gcount();
        if (got > 0) {
            body.append(buffer.data(), static_cast<std::size_t>(got));
        }
        const auto spent = std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now() - started);
        if (spent.count() > timeout_ms) {
            Asset failed;
            failed.status = 504;
            failed.error_code = err::kTimeout;
            failed.error_message = "读面板文件超时：" + path;
            return failed;
        }
    }
    asset.status = 200;
    asset.body = std::move(body);
    return asset;
}

}  // namespace ui
}  // namespace ling
