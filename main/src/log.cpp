#include "ling/log.hpp"
#include "ling/sys.hpp"

#include <chrono>
#include <cstdio>
#include <ctime>
#include <mutex>
#include <string>

namespace ling {
namespace {

std::mutex g_mutex;
std::filesystem::path g_log_file;

const char* level_text(LogLevel level) {
    switch (level) {
        case LogLevel::kError:
            return "error";
        case LogLevel::kWarn:
            return "warn";
        case LogLevel::kInfo:
            return "info";
        case LogLevel::kDebug:
            return "debug";
    }
    return "info";
}

std::string timestamp() {
    const auto now = std::chrono::system_clock::now();
    const std::time_t seconds = std::chrono::system_clock::to_time_t(now);
    const std::tm local = sys::local_time(&seconds);
    char buffer[32] = {};
    std::snprintf(buffer, sizeof(buffer), "%04d-%02d-%02dT%02d:%02d:%02d",
                  local.tm_year + 1900, local.tm_mon + 1, local.tm_mday, local.tm_hour,
                  local.tm_min, local.tm_sec);
    return buffer;
}

std::string date_part() {
    const std::string full = timestamp();
    return full.substr(0, 10);
}

}  // namespace

void log_init(const std::filesystem::path& log_dir) {
    std::lock_guard<std::mutex> guard(g_mutex);
    std::error_code ec;
    std::filesystem::create_directories(log_dir, ec);
    g_log_file = log_dir / ("kernel-" + date_part() + ".log");
}

void log_write(LogLevel level, const std::string& module, const std::string& message) {
    std::lock_guard<std::mutex> guard(g_mutex);
    const std::string line = timestamp() + " [" + level_text(level) + "] " + module + " " + message;
    std::fprintf(stderr, "%s\n", line.c_str());
    if (!g_log_file.empty()) {
        std::FILE* file = sys::open_append(g_log_file);
        if (file != nullptr) {
            std::fprintf(file, "%s\n", line.c_str());
            std::fclose(file);
        }
    }
}

}  // namespace ling
