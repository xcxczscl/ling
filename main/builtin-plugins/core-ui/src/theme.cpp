// 令牌表与 theme.css 生成。见 theme.hpp。
#include "theme.hpp"

#include <nlohmann/json.hpp>

namespace ling {
namespace ui {

const std::vector<Token>& tokens() {
    // 颜色 14 个。
    static const std::vector<Token> kTokens = {
        {"--ling-bg", "#ffffff", "#1b1b1d"},
        {"--ling-bg-raised", "#ffffff", "#242427"},
        {"--ling-bg-sunken", "#f6f7f9", "#141416"},
        {"--ling-text", "#1f2328", "#e6e6e6"},
        {"--ling-text-dim", "#5b6472", "#a0a7b4"},
        {"--ling-text-invert", "#ffffff", "#141416"},
        {"--ling-border", "#d8dde5", "#33383f"},
        {"--ling-border-strong", "#b9c0cb", "#4a515a"},
        {"--ling-accent", "#2f6fed", "#6f9bff"},
        {"--ling-accent-hover", "#2560d8", "#8ab0ff"},
        {"--ling-accent-fg", "#ffffff", "#10141c"},
        {"--ling-ok", "#1a7f37", "#4ac26b"},
        {"--ling-warn", "#b26a00", "#e0a33a"},
        {"--ling-danger", "#c0392b", "#ff6b5e"},
        // 字号 4 个。
        {"--ling-fs-sm", "12px", "12px"},
        {"--ling-fs-md", "13px", "13px"},
        {"--ling-fs-lg", "15px", "15px"},
        {"--ling-fs-xl", "18px", "18px"},
        // 间距 5 个。
        {"--ling-sp-1", "4px", "4px"},
        {"--ling-sp-2", "8px", "8px"},
        {"--ling-sp-3", "12px", "12px"},
        {"--ling-sp-4", "16px", "16px"},
        {"--ling-sp-5", "24px", "24px"},
        // 形状 3 个。圆角不随主题变（尺度不是颜色），阴影随主题变。
        {"--ling-radius", "8px", "8px"},
        {"--ling-radius-sm", "4px", "4px"},
        {"--ling-shadow", "0 1px 2px rgba(0,0,0,0.06)", "0 1px 2px rgba(0,0,0,0.4)"},
    };
    return kTokens;
}

bool valid_theme(const std::string& name) { return name == "light" || name == "dark"; }

namespace {

void append_block(std::string* out, const char* selector, bool dark) {
    *out += selector;
    *out += " {\n";
    for (const auto& token : tokens()) {
        *out += "  ";
        *out += token.name;
        *out += ": ";
        *out += dark ? token.dark : token.light;
        *out += ";\n";
    }
    *out += "}\n";
}

}  // namespace

std::string render_theme_css(const std::string& current_theme, const std::string& generated_at) {
    const bool dark = current_theme == "dark";
    std::string out;
    out += "/* core-ui theme.css — 生成于 ";
    out += generated_at;
    out += "，当前主题 ";
    out += dark ? "dark" : "light";
    out += " */\n";
    // 三段缺一不可：:root 取当前主题（面板首屏不用等 init），两个 [data-theme] 各给全量。
    append_block(&out, ":root", dark);
    append_block(&out, ":root[data-theme=\"light\"]", false);
    append_block(&out, ":root[data-theme=\"dark\"]", true);
    return out;
}

std::string render_tokens_json(const std::string& theme) {
    const bool dark = theme == "dark";
    nlohmann::json object = nlohmann::json::object();
    for (const auto& token : tokens()) {
        object[token.name] = dark ? token.dark : token.light;
    }
    return object.dump();
}

}  // namespace ui
}  // namespace ling
