// 版本信息。编译期常量，不读配置文件、不读环境变量。
// 版本规则见 docs/common/02-版本号与兼容约定.md §2。
#pragma once

namespace ling {

// 形如 "0.0.1-b1"。
const char* version_string() noexcept;

// 只含主版本号，用于 api 兼容判断（docs/ling/04-详细设计/02-插件装载与接口.md §5.1）。
int api_version() noexcept;

}  // namespace ling
