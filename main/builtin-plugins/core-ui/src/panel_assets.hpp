// 面板静态资源：把 `/panel/<插件 id>/<路径>` 安全地变成一个文件响应。
//
// 依据 docs/ling/builtin-plugins/core-ui/04-详细设计/01-服务端与接口.md §5：
// 后缀白名单、五道路径检查、按后缀定 MIME、ETag 回 304。
// 任一检查不过都返回 404，不区分「不存在」与「不允许」——区分等于告诉探测者哪里有东西。
#pragma once

#include <filesystem>
#include <string>

namespace ling {
namespace ui {

struct Asset {
    int status = 404;
    std::string content_type;   // 200 与 304 时给
    std::string body;
    std::string etag;
    std::string error_code;     // 失败时填，供上层拼 JSON 体
    std::string error_message;
};

// plugins_dir 是 <数据根>/plugins。
// plugin_id 只允许 ^[a-z0-9][a-z0-9-]*$；rel_path 是 `/panel/<id>/` 后面的部分（已去掉查询串）。
// default_file 是该插件 `[ui].entry` 的文件名，用于 `/panel/<id>/` 这种目录请求。
// if_none_match 是请求头原文，命中时返回 304。
Asset load_panel_asset(const std::filesystem::path& plugins_dir, const std::string& plugin_id,
                       const std::string& rel_path, const std::string& default_file,
                       const std::string& if_none_match, int timeout_ms);

// URL 解码一次。解不开的 % 原样保留，由第 1 道检查拦下。
std::string url_decode_once(const std::string& text);

}  // namespace ui
}  // namespace ling
