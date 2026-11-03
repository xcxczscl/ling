// 带面板插件的内存表。见 panel_registry.hpp。
#include "panel_registry.hpp"

#include <algorithm>
#include <system_error>

#include <toml.hpp>

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
    std::error_code ec;
    for (const auto& entry : std::filesystem::directory_iterator(plugins_dir_, ec)) {
        if (!entry.is_directory()) {
            continue;
        }
        const auto manifest_path = entry.path() / "plugin.toml";
        if (!std::filesystem::exists(manifest_path, ec)) {
            continue;
        }
        toml::table table;
        try {
            table = toml::parse_file(manifest_path.string());
        } catch (const toml::parse_error&) {
            // 清单坏了不阻断就绪：这个插件不进面板表，其他面板照常可用（§8 第 4 步）。
            continue;
        }
        const auto* ui = table.get("ui");
        if (ui == nullptr || !ui->is_table()) {
            continue;
        }
        const auto id = entry.path().filename().string();
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
            if (!item.is_object() || item.value("id", std::string()) != panel.id) {
                continue;
            }
            panel.state = map_state(item.value("state", std::string()));
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
