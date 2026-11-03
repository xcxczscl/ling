// 设计令牌的唯一来源：`theme.css` 与 `init` 消息里的 tokens 都从这里生成。
//
// 依据 docs/ling/builtin-plugins/core-ui/04-详细设计/04-设计令牌与主题.md §2：
// 26 个令牌 = 14 个颜色 + 4 个字号 + 5 个间距 + 3 个形状，不设预留位。
// 加令牌时连同亮暗两套取值一起加，并同步 shell.css 的 :root 块。
// 当前没有自动比对的脚本或构建步骤，保持一致要靠人工。
#pragma once

#include <string>
#include <vector>

namespace ling {
namespace ui {

struct Token {
    const char* name;    // 带 --ling- 前缀的变量名
    const char* light;   // 亮色取值
    const char* dark;    // 暗色取值；与 light 相同的（尺度类）两列写一样
};

// 全部 26 个令牌，顺序即 theme.css 里的书写顺序。
const std::vector<Token>& tokens();

// 主题名。只认 light 与 dark，其余一律当 light（配置可手改，值不能信）。
bool valid_theme(const std::string& name);

// 生成 `GET /api/ui/theme.css` 的三段内容：
//   :root              当前主题（首屏直接可用，暗色用户不白闪）
//   :root[data-theme="light"]  亮色全量 26 个
//   :root[data-theme="dark"]   暗色全量 26 个
std::string render_theme_css(const std::string& current_theme, const std::string& generated_at);

// `init` / `theme` 消息里的 tokens 对象：恒为全量 26 个，不随主题只给变化的 15 个。
std::string render_tokens_json(const std::string& theme);

}  // namespace ui
}  // namespace ling
