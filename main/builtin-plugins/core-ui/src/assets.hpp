// 编译期嵌入的外壳文件。
//
// 外壳页面、样式、三个脚本、六个页签脚本都在 shell.html 之外的普通文件里写，
// 构建期由 tools/embed.py 拼成一张常量表（见 CMakeLists.txt）。
// 服务端只认这几个取用接口，不关心它们是怎么嵌进来的。
#pragma once

#include <string>

namespace ling {
namespace ui {

// 名字与 embed.py 的键一致；找不到返回 nullptr。
const char* embedded_asset(const std::string& name);

const char* shell_html();
const char* shell_css();
const char* shell_js();

// 六个页签之一的脚本，name 只允许 plugins/logs/config/jobs/appearance/about。
const char* view_js(const std::string& name);

// 页签名白名单，供服务端判断 `/views/<名称>.js` 是否 404。
bool valid_view_name(const std::string& name);

}  // namespace ui
}  // namespace ling
