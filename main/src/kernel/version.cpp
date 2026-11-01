#include "ling/version.hpp"

namespace ling {

const char* version_string() noexcept { return LING_VERSION; }

// S0 阶段 api 版本固定为 1，与清单字段 api = "1" 对应。
int api_version() noexcept { return 1; }

}  // namespace ling
