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
    std::string executable;      // 相对插件目录的路径
    std::string restart = "on-crash";
    std::int32_t idle_timeout = 60;
    bool present = false;        // 清单里是否有 [sidecar] 段
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

}  // namespace ling
