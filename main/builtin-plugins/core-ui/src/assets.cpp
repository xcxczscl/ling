// 嵌入文件的取用。见 assets.hpp。
#include "assets.hpp"

#include <cstring>

namespace ling {
namespace ui {
namespace {

// ui_assets.inc 由 tools/embed.py 生成，里面是一张 {名字, 文本} 静态表。
#include "ui_assets.inc"

}  // namespace

const char* embedded_asset(const std::string& name) {
    for (unsigned i = 0; i < kEmbeddedAssetCount; ++i) {
        if (name == kEmbeddedAssets[i].name) {
            return kEmbeddedAssets[i].text;
        }
    }
    return nullptr;
}

const char* shell_html() { return embedded_asset("shell.html"); }

const char* shell_css() { return embedded_asset("shell.css"); }

const char* shell_js() { return embedded_asset("shell.js"); }

bool valid_view_name(const std::string& name) {
    return name == "plugins" || name == "logs" || name == "config" || name == "jobs" ||
           name == "appearance" || name == "about";
}

const char* view_js(const std::string& name) {
    if (!valid_view_name(name)) {
        return nullptr;
    }
    return embedded_asset("views/" + name + ".js");
}

}  // namespace ui
}  // namespace ling
