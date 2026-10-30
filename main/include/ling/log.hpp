// 极简内核日志：stderr + <数据根>/logs/kernel-YYYY-MM-DD.log。
// 完整字段（trace id、错误码、来源模块）在 04/02 §7 定义，S1 先落四个必需项。
#pragma once

#include <filesystem>
#include <string>

namespace ling {

enum class LogLevel { kError, kWarn, kInfo, kDebug };

void log_init(const std::filesystem::path& log_dir);
void log_write(LogLevel level, const std::string& module, const std::string& message);

inline void log_info(const std::string& module, const std::string& message) {
    log_write(LogLevel::kInfo, module, message);
}
inline void log_warn(const std::string& module, const std::string& message) {
    log_write(LogLevel::kWarn, module, message);
}
inline void log_error(const std::string& module, const std::string& message) {
    log_write(LogLevel::kError, module, message);
}

}  // namespace ling
