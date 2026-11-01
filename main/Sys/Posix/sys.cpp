// POSIX（Linux 等 Unix 系）平台实现。对应 main/include/ling/sys.hpp 的接口声明。
//
// 这一份只在非 Windows 上编译（CMake 里 else 分支把 main/Sys/Posix 加进源列表）。
// 注意：从 S0 起只在 Windows 上编过，这份代码第一次上 Linux 要重新编译并跑回归。
#include "ling/sys.hpp"

#include <cerrno>
#include <csignal>
#include <cstdlib>

namespace ling {
namespace sys {

// ---- 路径与进程自身 ----

std::filesystem::path executable_dir() {
    std::string buf(4096, '\0');
    const ssize_t n = ::readlink("/proc/self/exe", buf.data(), buf.size() - 1);
    if (n < 0) {
        return {};
    }
    buf.resize(static_cast<std::size_t>(n));
    return std::filesystem::path(buf).parent_path();
}

std::string read_env(const char* name) {
    const char* value = std::getenv(name);
    return value == nullptr ? std::string() : std::string(value);
}

std::string from_ansi(const char* text) {
    // POSIX 的命令行参数本来就是 UTF-8 字节，无需转码。
    return text == nullptr ? std::string() : std::string(text);
}

// ---- 控制台 ----

ConsoleUtf8Guard::ConsoleUtf8Guard() = default;
ConsoleUtf8Guard::~ConsoleUtf8Guard() = default;

namespace {
// signal 的处理函数要 void(int) 签名，业务侧只想传「置标志」的无参函数，这里桥一层。
void (*g_ctrl_c_handler)() = nullptr;

void ctrl_c_dispatch(int) {
    if (g_ctrl_c_handler != nullptr) {
        g_ctrl_c_handler();
    }
}
}  // namespace

void register_ctrl_c(void (*handler)()) {
    g_ctrl_c_handler = handler;
    std::signal(SIGINT, ctrl_c_dispatch);
    std::signal(SIGTERM, ctrl_c_dispatch);
}

// ---- 外部命令 ----

bool open_in_browser(const std::string& url, std::string* error) {
    const std::string command = "xdg-open '" + url + "' >/dev/null 2>&1";
    if (std::system(command.c_str()) != 0) {
        if (error != nullptr) {
            *error = "xdg-open 执行失败";
        }
        return false;
    }
    return true;
}

// ---- 动态库 ----

void* load_library(const std::filesystem::path& path) {
    return ::dlopen(path.string().c_str(), RTLD_NOW | RTLD_LOCAL);
}

void* find_symbol(void* module, const char* name) { return ::dlsym(module, name); }

void unload_library(void* module) { ::dlclose(module); }

// ---- 套接字 ----

void socket_startup() {
    // POSIX 不需要任何初始化，套接字就是普通文件描述符。
}

void close_socket(socket_handle fd) { ::close(fd); }

bool socket_would_block() { return errno == EAGAIN || errno == EWOULDBLOCK; }

int socket_select(socket_handle max_fd, fd_set* readfds, int timeout_ms) {
    timeval timeout{};
    timeout.tv_sec = timeout_ms / 1000;
    timeout.tv_usec = (timeout_ms % 1000) * 1000;
    return ::select(max_fd + 1, readfds, nullptr, nullptr, &timeout);
}

void set_socket_timeout(socket_handle fd, int timeout_ms) {
    timeval tv{};
    tv.tv_sec = timeout_ms / 1000;
    tv.tv_usec = (timeout_ms % 1000) * 1000;
    ::setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    ::setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
}

// ---- 时间与文件 ----

std::tm local_time(const std::time_t* seconds) {
    std::tm local{};
    localtime_r(seconds, &local);
    return local;
}

std::FILE* open_append(const std::filesystem::path& path) {
    return std::fopen(path.string().c_str(), "a");
}

bool atomic_replace(const std::filesystem::path& src, const std::filesystem::path& dst) {
    std::error_code ec;
    std::filesystem::rename(src, dst, ec);
    return !ec;
}

const char* shared_library_suffix() { return ".so"; }

}  // namespace sys
}  // namespace ling
