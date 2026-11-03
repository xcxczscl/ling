// 清单解析与校验。字段与校验顺序依据
// docs/ling/04-详细设计/02-插件装载与接口.md §3.1 至 §3.4。
#pragma once

#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <vector>

namespace ling {

struct SidecarSpec {
    // runtime 决定用什么起这个进程：
    //   process（默认）—— 直接跑 executable
    //   python         —— 用解释器跑 entry 脚本
    // 两种都走同一条 jsonl 协议，插件侧看不出区别。
    std::string runtime = "process";
    std::string executable;      // 相对插件目录的路径（runtime = process）
    std::string entry;           // 相对插件目录的脚本路径（runtime = python）
    std::string restart = "on-crash";
    std::int32_t idle_timeout = 60;
    bool present = false;        // 清单里是否有 [sidecar] 段
};

// [ui] 段。一个插件最多一个面板，所以这里是单段而不是数组。
struct UiSpec {
    bool present = false;
    std::string entry;   // 相对插件目录，必须落在 ui/ 下，后缀 .html
    std::string title;   // 1 至 32 字符
    std::string icon;    // 可空；相对插件目录，落在 ui/ 下，.svg 或 .png
};

struct Manifest {
    std::string id;
    std::string version;
    std::string api;
    std::string name;
    std::string min_host;        // 空表示不限制
    std::string description;
    std::vector<std::string> capabilities;
    std::vector<std::string> requires_plugins;
    SidecarSpec sidecar;

    // [ui] 段：声明本插件贡献一个面板（docs/ling/builtin-plugins/core-ui/04-详细设计/03 §1）。
    // 校验通过只说明字段合法，entry 指向的文件不在这里检查存在性。
    UiSpec ui;

    // 产物路径（进程内轨道），相对插件目录。
    std::string artifact;
};

struct ManifestResult {
    std::optional<Manifest> manifest;
    std::string code;     // 失败时的错误码，成功为空
    std::string detail;   // 失败时的补充说明，直接给人看
    bool ok() const { return manifest.has_value(); }
};

// 读取并校验 <dir>/plugin.toml。dir 的目录名必须等于清单 id。
// host_version 形如 "0.0.1-b1"，用于 min_host 比较。
ManifestResult load_manifest(const std::filesystem::path& dir, const std::string& host_version);

// 独立暴露的纯函数，便于单元测试（05 §4.6 允许的例外）。
bool valid_plugin_id(const std::string& id);
bool valid_version(const std::string& version);          // 三段式，每段 0 至 65535
bool version_at_least(const std::string& candidate, const std::string& required);

// 内置插件编号表（04/07 §2.1）。core- 前缀保留给这张表里的编号。
bool is_builtin_plugin(const std::string& id);

}  // namespace ling
