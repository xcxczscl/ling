// 「带面板的插件」内存表。
//
// 依据 docs/ling/builtin-plugins/core-ui/04-详细设计/01-服务端与接口.md §4.1：
// 界面不能自己去读插件目录判断有没有面板（那要在浏览器里再实现一遍清单解析），
// 所以服务端读一次全部清单，把 [ui] 段有效的插件记在内存里，`panels` 直接返回这张表。
//
// 失效有两条路，都不依赖宿主回调：
//   1. 服务端自己发出的写动作（install/uninstall/enable/disable）返回成功 → 立刻重读清单；
//   2. 别处改了状态（命令行、插件崩溃）→ 2 秒 TTL 兜底，只在有请求时触发。
#pragma once

#include <chrono>
#include <filesystem>
#include <string>
#include <vector>

#include <nlohmann/json.hpp>

namespace ling {
namespace ui {

struct PanelInfo {
    std::string id;
    std::string title;
    std::string icon_url;    // 没有图标时为空
    std::string entry_url;   // /panel/<id>/<entry 去掉 ui/ 前缀>
    std::string state;       // running / stopped / failed
    std::string reason;      // 仅 failed 时非空
};

class PanelRegistry {
public:
    explicit PanelRegistry(std::filesystem::path plugins_dir);

    // 重扫磁盘上的 plugin.toml，重建清单部分（状态保留到下一次 apply）。
    void reload_manifests();

    // 把 kernel.plugins 的结果套到清单上，并记下这次刷新的时刻。
    void apply_states(const nlohmann::json& plugins);

    // 距上次刷新是否已超过 TTL（2 秒）。有请求才问，界面没开就不扫盘。
    bool stale(std::chrono::steady_clock::time_point now) const;

    // 清单被写动作改过：下一次请求必须重读。
    void mark_dirty();

    const std::vector<PanelInfo>& panels() const { return panels_; }
    const PanelInfo* find(const std::string& id) const;

private:
    std::filesystem::path plugins_dir_;
    std::vector<PanelInfo> panels_;
    std::chrono::steady_clock::time_point refreshed_at_{};
    bool dirty_ = true;
};

// `entry` 到 URL 的换算：`ui/index.html` → `index.html`。
// entry 必须在 ui/ 下（装载期已校验），去掉前缀后不会出现越界的相对路径。
bool entry_to_url_path(const std::string& entry, std::string* out);

}  // namespace ui
}  // namespace ling
