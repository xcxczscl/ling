// 带面板插件的内存表。见 panel_registry.hpp。
#include "panel_registry.hpp"

#include <algorithm>
#include <cstdio>
#include <fstream>
#include <iterator>
#include <system_error>

#include <toml.hpp>

#include "panel_assets.hpp"

namespace ling {
namespace ui {
namespace {

constexpr auto kTtl = std::chrono::seconds(2);

// 内核状态到界面三值的映射。crashed 归入「启动失败」并带出 last_error。
std::string map_state(const std::string& kernel_state) {
    if (kernel_state == "activated") {
        return "running";
    }
    if (kernel_state == "failed" || kernel_state == "crashed") {
        return "failed";
    }
    return "stopped";
}

}  // namespace

bool entry_to_url_path(const std::string& entry, std::string* out) {
    constexpr const char* kPrefix = "ui/";
    if (entry.rfind(kPrefix, 0) != 0) {
        return false;
    }
    const std::string rest = entry.substr(3);
    if (rest.empty() || rest.find("..") != std::string::npos || rest.front() == '/') {
        return false;
    }
    *out = rest;
    return true;
}

PanelRegistry::PanelRegistry(std::filesystem::path plugins_dir)
    : plugins_dir_(std::move(plugins_dir)) {}

void PanelRegistry::reload_manifests() {
    std::vector<PanelInfo> fresh;
    std::error_code iter_ec;
    for (const auto& entry : std::filesystem::directory_iterator(plugins_dir_, iter_ec)) {
        std::error_code entry_ec;
        if (!entry.is_directory(entry_ec)) {
            continue;
        }
        const auto id = entry.path().filename().string();
        if (!valid_plugin_id(id)) {
            // 目录名不合规就是坏数据：记一笔，别让它悄悄变成另一张面板。
            std::fprintf(stderr, "[core-ui] 跳过插件目录 %s：目录名不是合法的插件编号\n",
                         id.c_str());
            continue;
        }
        const auto manifest_path = entry.path() / "plugin.toml";
        std::error_code exists_ec;
        if (!std::filesystem::exists(manifest_path, exists_ec)) {
            continue;
        }
        // 先读成字符串再解析：toml::parse_file(const char*) 在 Windows 上会把非 ASCII
        // 路径当成窄字符，中文数据根里的清单会读错。
        std::ifstream input(manifest_path, std::ios::binary);
        if (!input) {
            std::fprintf(stderr, "[core-ui] 读不到清单，跳过：%s\n", manifest_path.string().c_str());
            continue;
        }
        const std::string text((std::istreambuf_iterator<char>(input)),
                               std::istreambuf_iterator<char>());
        toml::table table;
        try {
            table = toml::parse(text);
        } catch (const toml::parse_error&) {
            // 清单坏了不阻断就绪：这个插件不进面板表，其他面板照常可用（§8 第 4 步）。
            std::fprintf(stderr, "[core-ui] 清单解析失败，跳过：%s\n",
                         manifest_path.string().c_str());
            continue;
        }
        const auto* ui = table.get("ui");
        if (ui == nullptr || !ui->is_table()) {
            continue;
        }
        PanelInfo info;
        info.id = id;
        const auto* title = ui->as_table()->get("title");
        info.title = title != nullptr && title->is_string()
                         ? title->value<std::string>().value_or(id)
                         : id;
        const auto* entry_node = ui->as_table()->get("entry");
        std::string url_path;
        if (entry_node == nullptr || !entry_node->is_string() ||
            !entry_to_url_path(entry_node->value<std::string>().value_or(""), &url_path)) {
            continue;
        }
        info.entry_url = "/panel/" + id + "/" + url_path;
        const auto* icon = ui->as_table()->get("icon");
        if (icon != nullptr && icon->is_string()) {
            std::string icon_path;
            if (entry_to_url_path(icon->value<std::string>().value_or(""), &icon_path)) {
                info.icon_url = "/panel/" + id + "/" + icon_path;
            }
        }
        info.state = "stopped";
        fresh.push_back(std::move(info));
    }
    if (iter_ec) {
        // 目录读到一半失败，fresh 只是半截表。不能拿它覆盖旧表，否则界面上
        // 的面板会凭空消失；保留旧表，按 TTL 等下一轮再试，避免每个请求都重读。
        std::fprintf(stderr, "[core-ui] 读插件目录失败：%s\n", plugins_dir_.string().c_str());
        dirty_ = false;
        refreshed_at_ = std::chrono::steady_clock::now();
        return;
    }
    std::sort(fresh.begin(), fresh.end(),
              [](const PanelInfo& a, const PanelInfo& b) { return a.id < b.id; });

    // 状态按 id 从旧表继承，等下一次 apply_states 覆盖。
    for (auto& panel : fresh) {
        for (const auto& old : panels_) {
            if (old.id == panel.id) {
                panel.state = old.state;
                panel.reason = old.reason;
                break;
            }
        }
    }
    panels_ = std::move(fresh);
    dirty_ = false;
}

void PanelRegistry::apply_states(const nlohmann::json& plugins) {
    if (!plugins.is_array()) {
        refreshed_at_ = std::chrono::steady_clock::now();
        return;
    }
    for (auto& panel : panels_) {
        panel.state = "stopped";
        panel.reason.clear();
        for (const auto& item : plugins) {
            if (!item.is_object() || !item.contains("id") || !item["id"].is_string() ||
                item["id"].get<std::string>() != panel.id) {
                continue;
            }
            panel.state = map_state(item.contains("state") && item["state"].is_string()
                                        ? item["state"].get<std::string>()
                                        : std::string());
            if (item.contains("reason") && item["reason"].is_string()) {
                panel.reason = item["reason"].get<std::string>();
            }
            break;
        }
        // 只有 failed 才带 reason（§4.1 的字段表）。
        if (panel.state != "failed") {
            panel.reason.clear();
        }
    }
    refreshed_at_ = std::chrono::steady_clock::now();
}

bool PanelRegistry::stale(std::chrono::steady_clock::time_point now) const {
    return dirty_ || now - refreshed_at_ >= kTtl;
}

void PanelRegistry::mark_dirty() { dirty_ = true; }

void PanelRegistry::mark_refreshed() { refreshed_at_ = std::chrono::steady_clock::now(); }

const PanelInfo* PanelRegistry::find(const std::string& id) const {
    for (const auto& panel : panels_) {
        if (panel.id == id) {
            return &panel;
        }
    }
    return nullptr;
}

}  // namespace ui
}  // namespace ling
